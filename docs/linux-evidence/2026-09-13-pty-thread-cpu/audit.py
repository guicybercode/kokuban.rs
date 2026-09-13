#!/usr/bin/env python3
"""Read-only audit of retained PTY measurements; never runs a measured executable."""

import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import statistics
import struct
import subprocess
import tarfile


RUN = 34751544546
HARNESS = "8c1762e58bec79e71d991d3d7bd7a49e57c3abb0"
REFS = {"before": "d54f801e9dec41a6eb4f159397e45939c367531d",
        "after": "62c99adc6fc1dbf12d89ee717f5823644c162d47"}
PREFIX = "linux-paired-revision-measurements/"
LINES = {
    "ascii": b"abcdefghijklmnopqrstuvwxyz0123456789 " * 2 + b"\r\n",
    "ansi": b"\x1b[31mred\x1b[0m \x1b[1;34mblue\x1b[0m \x1b[38;2;80;200;90mtruecolor\x1b[0m\r\n",
    "unicode": ("café 日本 λ e\u0301 " * 3 + "\r\n").encode(),
    "short_lines": b"x\r\n",
}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def close(actual, expected):
    assert math.isfinite(actual) and math.isclose(actual, expected, rel_tol=1e-10, abs_tol=1e-10), (actual, expected)


def dist(values):
    mid = statistics.median(values)
    return {"samples": values, "median": mid, "minimum": min(values), "maximum": max(values),
            "median_absolute_deviation": statistics.median(abs(x - mid) for x in values)}


def parse_stat(raw):
    # Independently parse the saved public proc format, never import the harness.
    match = re.fullmatch(r"(\d+) \((.*)\) (.+)\n?", raw, flags=re.DOTALL)
    assert match, raw
    fields = match[3].split()
    assert len(fields) >= 20
    return {"tid": int(match[1]), "name": match[2], "user_ticks": int(fields[11]),
            "system_ticks": int(fields[12]), "start_ticks": int(fields[19])}


def audit_measurements(read):
    def load(name):
        return json.loads(read(PREFIX + name))

    report = load("measurements/report.json")
    assert report["status"] == "passed" and report["thread_cpu_enabled"] is True
    assert report["comparison_mode"] == "revision-comparison" and not report["binaries_identical"]
    assert (report["screen"], report["backend"], report["pairs_requested"]) == ("primary", "wayland", 5)
    assert (report["bytes_requested_per_workload"], report["font_pixels"], report["scrollback_lines"],
            report["settle_seconds"], report["timeout_seconds"]) == (33554432, 14, 10000, 1, 120)
    assert report["cpu_affinity"] == [0] and report["requested_geometry"] == [24, 80]
    expected_order = [{"pair": pair, "side": side} for pair in range(1, 6)
                      for side in (("before", "after") if pair % 2 else ("after", "before"))]
    assert report["execution_order"] == expected_order and report["in_progress"] is None
    assert report["comparability"]["paired_inputs_validated"] and not report["comparability"]["reasons"]
    assert not report["rendering_equivalence_verified"]
    for field, filename in [("script_sha256", "compare-kokuban-revisions.py"),
                            ("comparator_sha256", "compare-terminal-performance.py"),
                            ("pty_driver_helpers_sha256", "linux-resource-smoke.py")]:
        assert sha(read("harness-reference/scripts/" + filename)) == report[field]
    assert read(PREFIX + "harness-revision.txt").decode().strip() == HARNESS
    payloads = {}
    assert set(report["payloads"]) == set(LINES)
    for name, line in LINES.items():
        repeats = 33554432 // len(line)
        digest = hashlib.sha256()
        for _ in range(repeats // 4096):
            digest.update(line * 4096)
        digest.update(line * (repeats % 4096))
        payloads[name] = {"bytes": repeats * len(line), "sha256": digest.hexdigest()}
        for field in ("bytes", "sha256"):
            assert payloads[name][field] == report["payloads"][name][field]

    records, raw_stats, configs, observed_pids = [], 0, set(), set()
    for label in ("before", "after"):
        terminal = report["terminals"][label]
        assert terminal["source_ref"] == REFS[label] and len(terminal["samples"]) == 5
        assert read(PREFIX + label + "-revision.txt").decode().strip() == REFS[label]
        for pair, sample in enumerate(terminal["samples"], 1):
            directory = f"measurements/{pair:02d}-{label}/"
            assert sample["pair"] == pair
            assert {k: v for k, v in sample.items() if k != "pair"} == load(directory + "sample.json")
            assert sample["status"] == "passed" and sample["thread_cpu_enabled"] is True
            result = load(directory + "result.json")
            assert result == sample["measurements"] and result["thread_cpu_enabled"] is True
            assert result["initial_geometry"] == [24, 80, 720, 408]
            assert len(result["protocol_rtt_seconds"]) == 30
            assert all(math.isfinite(x) and x > 0 for x in result["protocol_rtt_seconds"])
            case = load(directory + "case.json")
            assert case["thread_cpu"] is True and case["screen"] == "primary" and case["settle_seconds"] == 1
            assert case["payloads"] == {k: v["path"] for k, v in report["payloads"].items()}
            assert sample["command"][0] == terminal["path"]
            assert sample["command"][-1] == "--thread-cpu" and "--child-dir" in sample["command"]
            assert sample["command"][-2].endswith(f"/{pair:02d}-{label}")
            config = read(PREFIX + directory + "kokuban.toml")
            assert config.decode() == sample["config"]["text"] and sha(config) == sample["config"]["sha256"]
            assert b"scrollback_lines = 10000" in config and b"enabled = false" in config
            configs.add(sha(config))
            observed_pids.add(case["terminal_pid"])
            assert set(result["workloads"]) == set(LINES)
            process_identity, thread_identity = None, None
            previous_ticks = {}
            for name, value in result["workloads"].items():
                assert all(value[k] == payloads[name][k] for k in ("bytes", "sha256"))
                assert value["reply_hex"] == "1b5b313b3552"  # done marker: row 1, column 5
                assert value["geometry_before"] == value["geometry_after"] == [24, 80, 720, 408]
                elapsed = value["write_and_dsr_seconds"]
                assert elapsed > 0 and value["write_seconds"] > 0 and value["drain_rtt_seconds"] > 0
                close(value["write_seconds"] + value["drain_rtt_seconds"], elapsed)
                close(value["mib_per_second"], value["bytes"] / 1048576 / elapsed)
                diagnostic = value["thread_cpu"]
                before, after = diagnostic["before"], diagnostic["after"]
                assert before["pid"] == after["pid"] == case["terminal_pid"]
                assert before["ticks_per_second"] == after["ticks_per_second"] == 100
                assert diagnostic["newly_observed_tids"] == diagnostic["disappeared_tids"] == []
                snapshots = []
                for snapshot in (before, after):
                    assert snapshot["clock"] == "time.monotonic" and snapshot["status"] == "complete"
                    assert snapshot["enumeration_complete"] is True
                    assert snapshot["listed_tids"] == sorted(t["tid"] for t in snapshot["threads"])
                    assert len(set(snapshot["listed_tids"])) == len(snapshot["threads"]) == 4
                    parsed = {}
                    previous_read = snapshot["read_started_seconds"]
                    for thread in snapshot["threads"]:
                        raw_stats += 1
                        assert thread["status"] == "read"
                        stat = parse_stat(thread["stat"])
                        assert all(thread[k] == v for k, v in stat.items())
                        assert stat["user_ticks"] >= 0 and stat["system_ticks"] >= 0 and stat["start_ticks"] > 0
                        assert thread["cpu_ticks"] == stat["user_ticks"] + stat["system_ticks"]
                        assert thread["is_main_thread"] == (thread["tid"] == snapshot["pid"])
                        assert thread["is_terminal_reader"] == (thread["name"] == "terminal-reader")
                        assert previous_read <= thread["read_started_seconds"] <= thread["read_finished_seconds"] <= snapshot["read_finished_seconds"]
                        previous_read = thread["read_finished_seconds"]
                        identity = (stat["tid"], stat["start_ticks"])
                        if identity in previous_ticks:
                            assert all(stat[k] >= previous_ticks[identity][k] for k in ("user_ticks", "system_ticks"))
                        previous_ticks[identity] = stat
                        parsed[stat["tid"]] = thread
                    identities = {(x["tid"], x["start_ticks"], x["name"]) for x in parsed.values()}
                    if thread_identity is None:
                        thread_identity = identities
                    assert thread_identity == identities
                    snapshots.append(parsed)
                cpu_before, cpu_after = diagnostic["process_cpu_before"], diagnostic["process_cpu_after"]
                for cpu, snapshot in ((cpu_before, snapshots[0]), (cpu_after, snapshots[1])):
                    identity = (cpu["pid"], cpu["start_ticks"])
                    if process_identity is None:
                        process_identity = identity
                    assert identity == process_identity == (case["terminal_pid"], snapshot[case["terminal_pid"]]["start_ticks"])
                    assert cpu["threads"] == 4 and cpu["hwm_kib"] >= cpu["rss_kib"] > 0
                    close(cpu["cpu_seconds"], cpu["cpu_ticks"] / 100)
                assert cpu_after["cpu_ticks"] >= cpu_before["cpu_ticks"]
                process_seconds = (cpu_after["cpu_ticks"] - cpu_before["cpu_ticks"]) / 100
                close(value["terminal_cpu_seconds"], process_seconds)
                close(value["terminal_rss_before_kib"], cpu_before["rss_kib"])
                close(value["terminal_rss_after_kib"], cpu_after["rss_kib"])
                started, finished = (diagnostic["write_and_dsr_started_perf_counter"],
                                     diagnostic["write_and_dsr_finished_perf_counter"])
                close(finished - started, elapsed)
                # CPython Linux uses CLOCK_MONOTONIC for both clocks on this runner.
                assert before["read_finished_seconds"] <= cpu_before["monotonic_seconds"] <= started < finished <= cpu_after["monotonic_seconds"] <= after["read_started_seconds"]
                assert set(x["tid"] for x in diagnostic["threads"]) == set(snapshots[0]) == set(snapshots[1])
                roles = {}
                for row in diagnostic["threads"]:
                    old, new = (s[row["tid"]] for s in snapshots)
                    assert row["status"] == "matched"
                    assert old["start_ticks"] == new["start_ticks"] == row["before_start_ticks"] == row["after_start_ticks"]
                    assert old["name"] == new["name"] == row["before_name"] == row["after_name"]
                    assert row["is_main_thread"] == old["is_main_thread"]
                    assert row["terminal_reader_before"] == row["terminal_reader_after"] == old["is_terminal_reader"]
                    for counter in ("user_ticks", "system_ticks", "cpu_ticks"):
                        assert row[counter + "_delta"] == new[counter] - old[counter] >= 0
                    close(row["cpu_seconds_delta"], row["cpu_ticks_delta"] / 100)
                    low = new["read_started_seconds"] - old["read_finished_seconds"]
                    high = new["read_finished_seconds"] - old["read_started_seconds"]
                    close(row["read_window_seconds"]["minimum"], low)
                    close(row["read_window_seconds"]["maximum"], high)
                    role = "main" if row["is_main_thread"] else old["name"]
                    roles[role] = {"tid": row["tid"], "start_ticks": old["start_ticks"],
                                   "cpu_ms": row["cpu_ticks_delta"] * 10, "user_ms": row["user_ticks_delta"] * 10,
                                   "system_ms": row["system_ticks_delta"] * 10, "window_min_ms": low * 1000,
                                   "window_max_ms": high * 1000}
                assert set(roles) == {"main", "terminal-reader", "terminal-writer", "kokuban-clipboa"}
                process_window = cpu_after["monotonic_seconds"] - cpu_before["monotonic_seconds"]
                records.append({"pair": pair, "side": label, "workload": name, "pid": case["terminal_pid"],
                                "wall_ms": elapsed * 1000, "process_cpu_ms": process_seconds * 1000,
                                "process_read_timestamp_window_ms": process_window * 1000,
                                "process_percent_of_one_cpu_approx": process_seconds / process_window * 100,
                                "before_sweep_ms": (before["read_finished_seconds"] - before["read_started_seconds"]) * 1000,
                                "after_sweep_ms": (after["read_finished_seconds"] - after["read_started_seconds"]) * 1000,
                                "threads": roles})
    assert len(configs) == 1 and len(observed_pids) == 10 and len(records) == 40 and raw_stats == 320
    summaries = {}
    for workload in LINES:
        pairs = []
        for pair in range(1, 6):
            sides = {r["side"]: r for r in records if r["workload"] == workload and r["pair"] == pair}
            change = (sides["after"]["wall_ms"] / sides["before"]["wall_ms"] - 1) * 100
            pairs.append({"pair": pair, "order": "AB" if pair % 2 else "BA", "elapsed_change_percent": change,
                          "process_cpu_change_ms": sides["after"]["process_cpu_ms"] - sides["before"]["process_cpu_ms"],
                          "main_cpu_change_ms": sides["after"]["threads"]["main"]["cpu_ms"] - sides["before"]["threads"]["main"]["cpu_ms"],
                          "reader_cpu_change_ms": sides["after"]["threads"]["terminal-reader"]["cpu_ms"] - sides["before"]["threads"]["terminal-reader"]["cpu_ms"]})
        changes = [p["elapsed_change_percent"] for p in pairs]
        recorded = report["paired_summary"][workload]["elapsed_change_percent"]
        close(statistics.median(changes), recorded["median"])
        for a, b in zip(changes, recorded["samples"]):
            close(a, b)
        summaries[workload] = {"pairs": pairs, "paired_elapsed_change_percent": dist(changes),
                               "ab_median_percent": statistics.median(p["elapsed_change_percent"] for p in pairs if p["order"] == "AB"),
                               "ba_median_percent": statistics.median(p["elapsed_change_percent"] for p in pairs if p["order"] == "BA"),
                               "after_slower_count": sum(x > 0 for x in changes), "sides": {}}
        for side in ("before", "after"):
            rows = [r for r in records if r["workload"] == workload and r["side"] == side]
            summaries[workload]["sides"][side] = {"wall_ms": dist([r["wall_ms"] for r in rows]),
                "process_cpu_ms": dist([r["process_cpu_ms"] for r in rows]),
                "main_cpu_ms": dist([r["threads"]["main"]["cpu_ms"] for r in rows]),
                "reader_cpu_ms": dist([r["threads"]["terminal-reader"]["cpu_ms"] for r in rows])}
    return {"status": "PASS", "run": RUN, "harness": HARNESS, "source_refs": REFS,
            "samples": 10, "workload_observations": 40, "snapshots": 80, "raw_stats_reparsed": raw_stats,
            "thread_deltas_verified": 160, "new_disappeared_unavailable_or_reused_threads": 0,
            "ticks_per_second": 100, "tick_ms": 10, "payloads_reconstructed": payloads,
            "config_sha256": next(iter(configs)), "cpu_affinity": report["cpu_affinity"],
            "hardware_source": PREFIX + "cpu.txt", "font_match": report["font_match"],
            "summaries": summaries, "records": records,
            "limits": ["Five pairs are three AB and two BA; preserve both order subsets.",
                       "No rendered-pixel validation: DSR checks the final done marker at row 1 column 5.",
                       "Process reads, individual thread reads and write-to-DSR use nearby, distinct windows.",
                       "CPU counters are quantized to 10 ms; a zero valid delta does not prove no work.",
                       "Threads can run between sequential reads. Do not require thread sums to equal process CPU.",
                       "Opt-in observation can perturb scheduling even though its reads are outside the elapsed clock.",
                       "This run does not reproduce the earlier short-lines loss; it does not disprove other hosts/runs or identify its cause."]}


def audit_full(root, repository):
    read = lambda name: (root / name).read_bytes()
    result = audit_measurements(read)
    run = json.loads(read("github-run.json"))
    assert run["headSha"] == HARNESS and run["status"] == "completed" and run["conclusion"] == "success"
    assert len(run["jobs"]) == 1 and run["jobs"][0]["conclusion"] == "success"
    result["github_url"] = run["url"]
    source_manifests, builds = {}, {}
    for label, ref in REFS.items():
        archive_data = read(PREFIX + label + "-source.tar.gz")
        assert sha(archive_data) == read(PREFIX + label + "-source-sha256.txt").decode().split()[0]
        tree = subprocess.check_output(["git", "ls-tree", "-r", "-z", ref], cwd=repository)
        expected = {}
        for item in tree.split(b"\0"):
            if item:
                metadata, name = item.split(b"\t", 1)
                mode, kind, oid = metadata.decode().split()
                assert kind == "blob"
                expected[name.decode()] = {"git_blob": oid, "mode": mode}
        manifest = {}
        with tarfile.open(root / (PREFIX + label + "-source.tar.gz"), "r:gz") as archive:
            for member in archive:
                if member.isdir():
                    continue
                assert member.name not in manifest and member.name in expected
                assert member.isfile() or member.issym()
                data = archive.extractfile(member).read() if member.isfile() else member.linkname.encode()
                git_blob = hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()
                assert git_blob == expected[member.name]["git_blob"], member.name
                mode = "120000" if member.issym() else ("100755" if member.mode & 0o111 else "100644")
                assert mode == expected[member.name]["mode"]
                manifest[member.name] = {"bytes": len(data), "sha256": sha(data), "git_blob": git_blob, "mode": mode}
                if member.name in ("Cargo.toml", "Cargo.lock"):
                    assert data == read(PREFIX + label + "-build/" + member.name)
        assert set(manifest) == set(expected)
        source_manifests[label] = manifest
        binary = read(PREFIX + label + "-build/kokuban")
        digest = sha(binary)
        report = json.loads(read(PREFIX + "measurements/report.json"))
        assert digest == report["terminals"][label]["sha256"] == read(PREFIX + label + "-build/binary-sha256.txt").decode().split()[0]
        assert binary[:6] == b"\x7fELF\x02\x01" and struct.unpack_from("<H", binary, 18)[0] == 62
        shoff = struct.unpack_from("<Q", binary, 40)[0]
        shentsize, shnum, shstrndx = struct.unpack_from("<HHH", binary, 58)
        headers = [struct.unpack_from("<IIQQQQIIQQ", binary, shoff + i * shentsize) for i in range(shnum)]
        strings_header = headers[shstrndx]
        strings = binary[strings_header[4]:strings_header[4] + strings_header[5]]
        sections = {strings[h[0]:].split(b"\0", 1)[0].decode(): h[5] for h in headers}
        messages = [json.loads(line) for line in read(PREFIX + label + "-build/cargo-messages.jsonl").splitlines()]
        assert messages[-1]["reason"] == "build-finished" and messages[-1]["success"] is True
        artifacts = [x for x in messages if x["reason"] == "compiler-artifact"]
        assert artifacts and all(x["fresh"] is False for x in artifacts)
        roots = [x for x in artifacts if x["target"]["name"] == "kokuban" and x["executable"]]
        assert len(roots) == 1
        cargo = roots[0]
        assert cargo["executable"] == f"/home/runner/work/_temp/kokuban-paired-build-{label}/release/kokuban"
        assert cargo["manifest_path"] == f"/home/runner/work/_temp/kokuban-paired-source-{label}/Cargo.toml"
        assert cargo["profile"]["opt_level"] == "3" and not cargo["profile"]["debug_assertions"] and not cargo["profile"]["test"]
        builds[label] = {"source_revision": ref, "archive_sha256": sha(archive_data), "source_files_git_verified": len(manifest),
                         "binary_sha256": digest, "binary_bytes": len(binary), "elf_machine": "x86_64",
                         "elf_sections_bytes": {k: sections[k] for k in (".text", ".rodata", ".data", ".bss")},
                         "compiler_artifacts_fresh_false": len(artifacts), "root_cargo_artifact": cargo}
    result["builds"] = builds
    result["runtime_source_changed_paths"] = sorted(path for path in set(source_manifests["before"]) | set(source_manifests["after"])
        if (path.startswith("src/") or path in ("Cargo.toml", "Cargo.lock")) and source_manifests["before"].get(path) != source_manifests["after"].get(path))
    result["provenance_limit"] = "ELF/TAR bytes and Git blob identities independently checked; compiler linkage relies on retained CI build records, not a local rebuild. GitHub ZIP digest is API metadata because gh download unpacks it."
    return result, source_manifests


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact_root", type=Path)
    parser.add_argument("--git-repository", required=True, type=Path)
    args = parser.parse_args()
    result, manifests = audit_full(args.artifact_root, args.git_repository)
    (args.artifact_root / "independent-thread-cpu-audit.json").write_text(json.dumps(result, indent=2) + "\n")
    (args.artifact_root / "source-file-manifests.json").write_text(json.dumps(manifests, indent=2) + "\n")
    print(json.dumps({"status": result["status"], "raw_stats_reparsed": result["raw_stats_reparsed"],
                      "medians_percent": {k: v["paired_elapsed_change_percent"]["median"] for k, v in result["summaries"].items()}}, indent=2))
