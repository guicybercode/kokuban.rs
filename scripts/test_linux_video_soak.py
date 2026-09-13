import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location('video_soak', Path(__file__).with_name('linux-video-soak.py'))
soak = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(soak)


def stats(pid=10, start=20, cpu=1.0, rss=1000, hwm=2000):
    return {'pid': pid, 'start_ticks': start, 'cpu_seconds': cpu, 'rss_kib': rss, 'hwm_kib': hwm}


class SoakTests(unittest.TestCase):
    def test_duration_and_memory_guard_cli_boundaries(self):
        for duration in (30, 900, 1800):
            parsed = soak.parse_args(['binary', '--artifacts-dir', 'new', '--duration-seconds', str(duration)])
            self.assertEqual(parsed.duration_seconds, duration)
            self.assertEqual(parsed.max_rss_mib, 1024)
        for values in (['--duration-seconds', '0'], ['--duration-seconds', '29'],
                       ['--duration-seconds', '1801'], ['--duration-seconds', 'nan'],
                       ['--max-rss-mib', '0'], ['--max-rss-mib', '4097']):
            with self.subTest(values=values), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                soak.parse_args(['binary', '--artifacts-dir', 'new', *values])

    def test_repeated_clip_wraps_are_valid_real_progress(self):
        progress = soak.Progress()
        for half in range(61):
            seconds = half / 2
            progress.observe(seconds, int(seconds * 12) % 72)
        result = progress.finish(30.1)
        self.assertEqual(result['observed_wraps'], 5)
        self.assertEqual(result['observed_transitions'], 60)
        self.assertEqual(len(result['windows']), 3)

    def test_frozen_valid_pixels_do_not_renew_progress_deadline(self):
        progress = soak.Progress()
        for half in range(11):
            progress.observe(half / 2, 0)
        with self.assertRaisesRegex(AssertionError, 'no new valid visible frame'):
            progress.observe(5.5, 0)

    def test_observer_stall_is_distinguished_from_video_stall(self):
        progress = soak.Progress()
        progress.observe(0, 0)
        with self.assertRaisesRegex(AssertionError, 'observer gap'):
            progress.observe(3, 36)

    def test_backwards_or_impossibly_fast_frame_jump_fails(self):
        for frame in (9, 40):
            with self.subTest(frame=frame):
                progress = soak.Progress()
                progress.observe(0, 10)
                with self.assertRaisesRegex(AssertionError, 'implausible cyclic frame advance'):
                    progress.observe(0.5, frame)

    def test_invalid_frames_are_bounded_even_when_valid_frames_advance(self):
        progress = soak.Progress()
        with self.assertRaisesRegex(AssertionError, '10% invalid'):
            for half in range(21):
                progress.observe(half / 2, None if half % 3 == 1 else (half * 6) % 72)

    def test_three_repeating_ids_do_not_satisfy_a_progress_window(self):
        progress = soak.Progress()
        # Simulate three distant IDs at an interval that permits each
        # modular advance; neither observer nor five-second watchdog is enough.
        with self.assertRaisesRegex(AssertionError, 'fewer than four'):
            for index in range(6):
                progress.observe(index * 2, (index * 24) % 72)

    def test_missing_reused_invalid_or_regressing_process_samples_fail(self):
        cases = [None, stats(start=21), stats(rss=0), stats(cpu=0.5), stats(rss=3000, hwm=3000)]
        for current in cases:
            with self.subTest(current=current), self.assertRaises(AssertionError):
                soak.validate_stats(current, (10, 20), stats(), 2048)
        soak.validate_stats(stats(cpu=1.1), (10, 20), stats(), 2048)

    def test_cpu_rss_and_lifetime_hwm_use_different_definitions(self):
        rows = [{'seconds': t, 'kokuban': stats(cpu=1 + t / 4, rss=1000, hwm=2000 + t * 100),
                 'mpv': stats(pid=11, cpu=2 + t / 8, rss=2000, hwm=4000)} for t in (0, 1, 2)]
        result = soak.resource_window(rows, 0, 2)
        self.assertEqual(result['kokuban']['cpu_percent_one_core'], 25)
        self.assertEqual(result['mpv']['cpu_percent_one_core'], 12.5)
        self.assertEqual(result['kokuban']['rss_median_kib'], 1000)
        self.assertEqual(result['kokuban']['hwm_lifetime_kib'], 2200)
        self.assertEqual(result['kokuban']['rss_linear_slope_mib_per_minute'], 0)

    def test_pilot_has_no_invented_post_180_second_or_late_comparison(self):
        rows = [{'seconds': t, 'kokuban': stats(), 'mpv': stats(pid=11)} for t in (0, 15, 30)]
        result = soak.summarize_resources(rows)
        self.assertFalse(result['after_180_seconds']['available'])
        self.assertFalse(result['late_window_comparison']['available'])

    def test_long_run_compares_disjoint_memory_windows(self):
        rows = [{'seconds': t, 'kokuban': stats(rss=1000 + t, hwm=3000), 'mpv': stats(pid=11)}
                for t in range(0, 901, 60)]
        result = soak.summarize_resources(rows)['late_window_comparison']
        self.assertTrue(result['available'])
        self.assertEqual(result['rss_median_delta_kib'], {'kokuban': 600, 'mpv': 0})
        self.assertLess(result['post_180_to_300_seconds']['end_seconds'], result['final_120_seconds']['start_seconds'])

    def test_cleanup_attempts_every_owned_process_after_an_ipc_error(self):
        ipc = mock.Mock()
        ipc.close.side_effect = OSError('already closed')
        terminal = mock.Mock()
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / 'mpv.json').write_text(json.dumps(stats(pid=11)))
            (directory / 'child.json').write_text(json.dumps(stats(pid=12)))
            with mock.patch.object(soak.smoke, 'stop_owned') as owned, mock.patch.object(soak.smoke, 'stop_process') as stop:
                errors = soak.cleanup(ipc, terminal, directory)
            self.assertEqual([call.args[0]['pid'] for call in owned.call_args_list], [11, 12])
            stop.assert_called_once_with(terminal)
            self.assertEqual(len(errors), 1)

    def test_child_timeout_records_failure_and_stops_its_player(self):
        player = mock.Mock(pid=11)
        player.wait.side_effect = subprocess.TimeoutExpired('mpv', 90)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / 'mpv-command.json').write_text('["mpv"]')
            with mock.patch.object(soak.subprocess, 'Popen', return_value=player), \
                 mock.patch.object(soak.smoke, 'process_stats', return_value=stats(pid=11)), \
                 mock.patch.object(soak.smoke, 'stop_process') as stop:
                with self.assertRaises(subprocess.TimeoutExpired):
                    soak.child(directory, 30)
            player.wait.assert_called_once_with(timeout=90)
            stop.assert_called_once_with(player)
            self.assertIn('TimeoutExpired', json.loads((directory / 'child-error.json').read_text())['error'])

    def test_hard_deadline_fails_explicitly(self):
        with self.assertRaisesRegex(TimeoutError, 'hard deadline'):
            soak.hard_timeout(None, None)


if __name__ == '__main__':
    unittest.main()
