"""Regressions from the real pinned clients' first Linux GUI run."""

import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('ai_tui_smoke', Path(__file__).with_name('linux-ai-tui-smoke.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class ClientObservationTests(unittest.TestCase):
    def test_claude_real_plan_footer_is_ready_without_codex_shortcuts_hint(self):
        observed = ('Claude Code v2.1.261\ncompat-fixture · API Usage Billing\n'
                    '❯ \n⏸ plan mode on (shift+tab to cycle) · ← for agents')
        self.assertTrue(smoke.prompt_ready('claude', observed))
        self.assertFalse(smoke.prompt_ready('claude', 'compat-fixture\nChoose the text style'))
        self.assertFalse(smoke.prompt_ready('codex', observed))

    def test_codex_prompt_still_requires_its_interactive_footer(self):
        self.assertTrue(smoke.prompt_ready('codex', 'model: compat-fixture low /model to change\n› Ask Codex to do anything'))
        self.assertFalse(smoke.prompt_ready('codex', 'compat-fixture: starting'))
        # Actual early PTY log contains the configured model in script's
        # COMMAND header before the initial loading screen is ready.
        self.assertFalse(smoke.prompt_ready('codex', 'COMMAND="model=compat-fixture"\nmodel: loading /model to change\n› Ask Codex to do anything\n? for shortcuts'))

    def test_onboarding_waits_for_latest_selected_option_in_real_update_fragments(self):
        yes = '\x1b[?2026h\x1b[38;2;177;185;249m❯\x1b[4GYes, I trust this folder\x1b[39m\x1b[?2026l'.encode()
        no = '\x1b[?2026h\x1b[38;2;177;185;249m❯\x1b[4GNo, exit\x1b[39m\x1b[?2026l'.encode()
        self.assertTrue(smoke.option_selected(yes, 'Yes, I trust this folder', 'No, exit'))
        self.assertFalse(smoke.option_selected(no, 'Yes, I trust this folder', 'No, exit'))
        self.assertFalse(smoke.option_selected(yes + no, 'Yes, I trust this folder', 'No, exit'))

    def test_transient_selected_option_does_not_confirm_before_it_stays_selected(self):
        yes = '❯Yes, I trust this folder\x1b[?2026l'.encode()
        no = '❯No, exit\x1b[?2026l'.encode()
        state = {}
        args = ('Yes, I trust this folder', 'No, exit', state)
        self.assertFalse(smoke.stable_option(yes, *args, 1.0))
        self.assertFalse(smoke.stable_option(yes, *args, 1.035))
        self.assertFalse(smoke.stable_option(no, *args, 1.2))
        self.assertFalse(smoke.stable_option(yes, *args, 1.3))
        self.assertFalse(smoke.stable_option(yes, *args, 1.54))
        self.assertTrue(smoke.stable_option(yes, *args, 1.56))

    def test_resize_requires_marker_followed_by_completed_sync_frame(self):
        marker = b'\x1b[?2026h\x1b[22mCOMPAT_BEGIN\x1b[39m'
        self.assertFalse(smoke.cli_resize_frame(marker))
        self.assertFalse(smoke.cli_resize_frame(b'\x1b[?2026l' + marker))
        self.assertFalse(smoke.cli_resize_frame(b'\x1b[?2026h\x1b[?2026l'))
        self.assertTrue(smoke.cli_resize_frame(marker + b'\x1b[?2026l'))


if __name__ == '__main__':
    unittest.main()
