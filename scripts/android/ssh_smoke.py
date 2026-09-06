#!/usr/bin/env python3
"""Exercise the APK's SSH client through its PTY against an ephemeral local sshd.

Run on an isolated Linux CI runner with sshd, git, neovim, tmux, fzf and Rust.
Private test credentials stay in target/android-private and are deleted on exit;
only screenshots, public host fingerprints and result summaries are evidence.
"""

import argparse
import getpass
import json
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time

from device import Device
from media_scenarios import run_media_scenarios
from smoke import eventually


def run(*args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", default="emulator-5554")
    parser.add_argument("--package", default="com.kokuban.terminal")
    parser.add_argument("--host", default="10.0.2.2", help="Runner host as seen by the Android emulator")
    parser.add_argument("--port", type=int, default=2222)
    parser.add_argument("--output", type=Path, default=Path("target/android-evidence/ssh"))
    parser.add_argument("--upgrade-apk", type=Path, help="After debug trust checks, upgrade to this release APK for interactive tests")
    parser.add_argument("--debug-apk", type=Path, default=Path("target/debug/apk/kokuban.apk"), help="Restore this debuggable APK after release tests to verify credential cleanup")
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("This ephemeral sshd fixture runs on an isolated Linux runner")
    if not 1024 <= args.port <= 65535:
        parser.error("Use an unprivileged test port from 1024 to 65535")
    if args.upgrade_apk and (not args.upgrade_apk.is_file() or not args.debug_apk.is_file()):
        parser.error("Both upgrade and debug APKs must exist and share the test signing key")
    args.output.mkdir(parents=True, exist_ok=True)
    private_root = Path("target/android-private").resolve()
    private_root.mkdir(parents=True, exist_ok=True)
    device = Device(args.serial)
    device.pid(args.package)
    user = getpass.getuser()
    results = {"device": device.details(), "checks": [], "status": "incomplete",
               "batch_profile": "debug-opt1", "interactive_profile": "release" if args.upgrade_apk else "debug-opt1"}
    started = time.monotonic()

    def checkpoint(stage):
        progress = {"stage": stage, "elapsed_seconds": round(time.monotonic() - started, 3)}
        results.setdefault("checkpoints", []).append(progress)
        pending = args.output / "results.json.tmp"
        pending.write_text(json.dumps(results, indent=2) + "\n")
        pending.replace(args.output / "results.json")
        print(f"SSH {results['interactive_profile']}: {stage} ({progress['elapsed_seconds']:.1f}s)", flush=True)

    checkpoint("preparing isolated SSH fixture")
    upgraded = False
    old_auto = device.shell("settings", "get", "system", "accelerometer_rotation")
    old_rotation = device.shell("settings", "get", "system", "user_rotation")

    def enter(command):
        device.type_text(command)
        device.shell("input", "keyevent", "KEYCODE_ENTER")

    def private_write(path, content):
        command = f"umask 077; mkdir -p {shlex.quote(str(Path(path).parent))}; cat > {shlex.quote(path)}"
        result = subprocess.run(device.command + ["shell", shlex.join(["run-as", args.package, "sh", "-c", command])],
                                input=content, capture_output=True, timeout=20)
        if result.returncode:
            raise RuntimeError(f"Could not populate private test input: {result.stderr.decode(errors='replace')}")

    def private_read(path):
        return device.shell("run-as", args.package, "cat", path, check=False)

    with tempfile.TemporaryDirectory(prefix="ssh-ci-", dir=private_root) as temporary:
        server_root = Path(temporary)
        project = server_root / "project"
        project.mkdir()
        (project / "src").mkdir()
        (project / "Cargo.toml").write_text('[package]\nname="android-ssh-proof"\nversion="0.1.0"\nedition="2021"\n[workspace]\n')
        (project / "src/main.rs").write_text('fn main() { println!("KOKUBAN_DEV_OK"); }\n')
        (server_root / "choices.txt").write_text("alpha\nbeta\ngamma\n")
        for name in ("host", "changed-host", "identity"):
            run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", server_root / name)
        fingerprint = subprocess.check_output(["ssh-keygen", "-lf", str(server_root / "host.pub")], text=True).strip()
        results["verified_host_fingerprint"] = fingerprint
        config = server_root / "sshd_config"
        config.write_text(f"""Port {args.port}
ListenAddress 0.0.0.0
HostKey {server_root / 'host'}
PidFile {server_root / 'sshd.pid'}
AuthorizedKeysFile {server_root / 'identity.pub'}
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
AuthenticationMethods publickey
PermitRootLogin no
StrictModes no
UsePAM yes
AllowUsers {user}
LogLevel VERBOSE
""")
        public = (server_root / "host.pub").read_text().split()[:2]
        changed = (server_root / "changed-host.pub").read_text().split()[:2]
        app_test = "files/home/.kokuban-ssh-ci"
        local_back = f"files/home/.kokuban-ssh-back-{server_root.name}"
        device.shell("run-as", args.package, "rm", "-f", local_back)
        private_write(f"{app_test}/identity", (server_root / "identity").read_bytes())
        private_write(f"{app_test}/known_hosts", f"[{args.host}]:{args.port} {' '.join(public)}\n".encode())
        private_write(f"{app_test}/changed_hosts", f"[{args.host}]:{args.port} {' '.join(changed)}\n".encode())
        private_write(f"{app_test}/unknown_hosts", b"")
        ssh = f'ssh --batch -p {args.port} -i "$HOME/.kokuban-ssh-ci/identity"'
        connection = shlex.quote(f"{user}@{args.host}")
        unknown_marker = server_root / "unknown-reached"
        changed_marker = server_root / "changed-reached"
        # This script is sourced in the actual terminal shell so its ssh()
        # function must invoke the installed APK-native SSH executable.
        checks = f"""{ssh} --known-hosts "$HOME/.kokuban-ssh-ci/unknown_hosts" {connection} 'touch {unknown_marker}' > "$HOME/.kokuban-ssh-ci/unknown.log" 2>&1
echo $? > "$HOME/.kokuban-ssh-ci/unknown.status"
{ssh} --known-hosts "$HOME/.kokuban-ssh-ci/changed_hosts" {connection} 'touch {changed_marker}' > "$HOME/.kokuban-ssh-ci/changed.log" 2>&1
echo $? > "$HOME/.kokuban-ssh-ci/changed.status"
{ssh} --known-hosts "$HOME/.kokuban-ssh-ci/known_hosts" {connection} 'printf SSH_ANDROID_OK' > "$HOME/.kokuban-ssh-ci/success.log" 2>&1
echo $? > "$HOME/.kokuban-ssh-ci/success.status"
printf DONE > "$HOME/.kokuban-ssh-ci/checks.done"
"""
        private_write(f"{app_test}/checks.sh", checks.encode())
        sshd = shutil.which("sshd") or "/usr/sbin/sshd"
        server_log = (server_root / "server.log").open("wb")
        process = subprocess.Popen(["sudo", "-n", sshd, "-D", "-e", "-f", str(config)], stdout=server_log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 15
            while True:
                if process.poll() is not None:
                    raise RuntimeError("Ephemeral sshd exited: " + (server_root / "server.log").read_text())
                try:
                    with socket.create_connection(("127.0.0.1", args.port), timeout=1):
                        break
                except OSError:
                    if time.monotonic() >= deadline:
                        raise RuntimeError("Ephemeral sshd did not listen")
                    time.sleep(0.2)
            checkpoint("ephemeral SSH server listening")
            enter('. "$HOME/.kokuban-ssh-ci/checks.sh"')
            eventually(lambda: private_read(f"{app_test}/checks.done"), "DONE", timeout=60)
            for scenario, expected in (("unknown", "unknown host key"), ("changed", "HOST KEY CHANGED")):
                if private_read(f"{app_test}/{scenario}.status") != "255" or expected not in private_read(f"{app_test}/{scenario}.log"):
                    raise AssertionError(f"{scenario} host key was not rejected as expected")
            if unknown_marker.exists() or changed_marker.exists():
                raise AssertionError("An untrusted server command was executed")
            if private_read(f"{app_test}/success.status") != "0" or private_read(f"{app_test}/success.log") != "SSH_ANDROID_OK":
                raise AssertionError("Pinned key-authenticated SSH command failed")
            results["checks"].extend(["unknown host rejected in batch", "changed host rejected before command execution", "verified host and client key execute a remote command"])
            results["batch_diagnostics"] = {scenario: private_read(f"{app_test}/{scenario}.log")[:2000]
                                            for scenario in ("unknown", "changed", "success")}
            checkpoint("host trust rejection and pinned key authentication passed")
            if args.upgrade_apk:
                device.adb("install", "-r", str(args.upgrade_apk.resolve()), timeout=120)
                upgraded = True
                component = device.shell("cmd", "package", "resolve-activity", "--brief", args.package).splitlines()[-1]
                device.shell("am", "start", "-W", "-n", component)
                release_pid = device.pid(args.package)
                eventually(lambda: "first frame presented" in device.adb("logcat", "-d", "--pid", release_pid), True, timeout=30)
                if device.shell("sh", "-c", f"run-as {shlex.quote(args.package)} pwd >/dev/null 2>&1; echo $?") == "0":
                    raise AssertionError("Release APK unexpectedly permits run-as")
                results["release_pid"] = release_pid
                checkpoint("non-debuggable release upgrade presented a frame")
            enter(f'{ssh} --known-hosts "$HOME/.kokuban-ssh-ci/known_hosts" {connection}')
            time.sleep(2)
            ready = server_root / "interactive-ready"
            enter(f"printf READY > {ready}")
            eventually(lambda: ready.read_text() if ready.exists() else "", "READY")
            checkpoint("interactive SSH shell accepted a command")
            device.screenshot(args.output / "01-ssh-shell.png")
            before, after = server_root / "size-before", server_root / "size-after"
            enter(f"stty size > {before}")
            eventually(lambda: before.exists(), True)
            device.shell("settings", "put", "system", "accelerometer_rotation", "0")
            device.shell("settings", "put", "system", "user_rotation", "1")
            time.sleep(2)
            enter(f"stty size > {after}")
            eventually(lambda: after.exists(), True)
            if before.read_text() == after.read_text():
                raise AssertionError("Remote PTY dimensions did not change after Android rotation")
            results["remote_pty_sizes"] = {"before": before.read_text().strip(), "after": after.read_text().strip()}
            results["checks"].append("interactive SSH propagates Android PTY resize")
            checkpoint("remote PTY resize passed")
            device.shell("settings", "put", "system", "user_rotation", "0")
            time.sleep(1)
            enter(f"cd {project}; git init -q; git add .; git -c user.name=KokubanTest -c user.email=test@example.invalid commit -qm initial")
            time.sleep(1)
            enter("nvim --clean src/main.rs")
            time.sleep(2)
            # Open a line above the Rust function. Splitting a // comment with
            # Enter would trigger Neovim's default automatic comment prefix and
            # comment out the existing function, invalidating the later build.
            device.type_text("O//edited-on-android")
            device.shell("input", "keyevent", "KEYCODE_ESCAPE")
            device.screenshot(args.output / "02-neovim-edited.png")
            enter(":wq")
            eventually(lambda: (project / "src/main.rs").read_text().startswith("//edited-on-android\n"), True)
            results["checks"].append("Neovim edits and saves Rust source through Android SSH")
            checkpoint("Neovim saved the source edit")
            enter("git --no-pager diff")
            time.sleep(1)
            device.screenshot(args.output / "03-git-diff.png")
            enter("tmux -L kokuban_ci -f /dev/null new-session -s dev")
            time.sleep(2)
            enter("tmux split-window -h")
            time.sleep(1)
            panes = server_root / "panes"
            enter(f"tmux list-panes > {panes}")
            eventually(lambda: panes.exists(), True)
            if len(panes.read_text().splitlines()) != 2:
                raise AssertionError("tmux did not create two interactive panes")
            device.screenshot(args.output / "04-tmux-panes.png")
            enter("tmux kill-server")
            results["checks"].append("tmux creates two interactive panes and returns to SSH shell")
            checkpoint("tmux pane scenario passed")
            time.sleep(1)
            chosen = server_root / "chosen"
            enter(f"fzf --no-sort < {server_root / 'choices.txt'} > {chosen}")
            time.sleep(1)
            device.type_text("beta")
            device.screenshot(args.output / "05-fzf-selection.png")
            device.shell("input", "keyevent", "KEYCODE_ENTER")
            eventually(lambda: chosen.read_text().strip() if chosen.exists() else "", "beta")
            results["checks"].append("fzf filters and selects beta interactively")
            checkpoint("fzf selected the expected item")
            build_result = server_root / "build-result"
            rustup = shlex.quote(shutil.which("rustup") or "rustup")
            compiler = subprocess.check_output([shutil.which("rustup") or "rustup", "which", "--toolchain", "1.94.1", "rustc"], text=True).strip()
            enter(f"RUSTC={shlex.quote(compiler)} {rustup} run 1.94.1 cargo run --offline > {build_result}")
            eventually(lambda: build_result.read_text().strip() if build_result.exists() else "", "KOKUBAN_DEV_OK", timeout=90)
            enter(f"cat {build_result}; git status --short")
            time.sleep(1)
            device.screenshot(args.output / "06-rust-project.png")
            results["checks"].append("edited Rust project compiles and runs remotely; Git sees the edit")
            checkpoint("remote Rust build and Git edit passed; starting media")
            results["media"] = run_media_scenarios(device, enter, server_root, args.output / "media", args.serial, args.package)
            checkpoint("media pixel scenarios passed")
            app_pid = device.pid(args.package)
            clients = [process for process in device.process_tree(app_pid) if process["name"] == "libkokuban_ssh.so"]
            if len(clients) != 1:
                raise AssertionError(f"Expected one interactive packaged SSH client before exit, got {clients}")
            client_pid = clients[0]["pid"]
            enter("exit")
            eventually(lambda: client_pid not in [process["pid"] for process in device.process_tree(app_pid)], True, timeout=15)
            if device.pid(args.package) != app_pid:
                raise AssertionError("Application process changed while disconnecting SSH")
            results["ssh_exit_observed"] = True
            checkpoint("packaged SSH process exited; checking local shell")
            # Returning to the local shell must not depend on a new network
            # connection. Only a command entered through the PTY writes this
            # marker; release verification waits until the debug restore below.
            enter(f'printf LOCAL_BACK > "$HOME/{Path(local_back).name}"')
            if not upgraded:
                eventually(lambda: private_read(local_back), "LOCAL_BACK")
            enter('rm -rf "$HOME/.kokuban-ssh-ci"')
            time.sleep(1)
            device.screenshot(args.output / "07-local-shell.png")
            results["versions"] = {name: subprocess.check_output(command, text=True).splitlines()[0] for name, command in {
                "git": ["git", "--version"], "neovim": ["nvim", "--version"],
                "tmux": ["tmux", "-V"], "fzf": ["fzf", "--version"],
                "rust": [shutil.which("rustup") or "rustup", "run", "1.94.1", "rustc", "--version"],
            }.items()}
            results["status"] = "passed"
        except (Exception, KeyboardInterrupt) as error:
            results["status"] = "failed"
            results["error"] = str(error) or type(error).__name__
            raise
        finally:
            cleanup_errors = []
            def cleanup(action):
                try:
                    action()
                except Exception as error:
                    cleanup_errors.append(str(error))
            if results["status"] != "passed":
                cleanup(lambda: device.screenshot(args.output / "failure.png"))
                cleanup(lambda: device.shell("am", "force-stop", args.package, check=False))
            for name, value in (("accelerometer_rotation", old_auto), ("user_rotation", old_rotation)):
                cleanup(lambda name=name, value=value: device.shell("settings", "delete" if value == "null" else "put", "system", name, *([] if value == "null" else [value]), check=False))
            if "batch_diagnostics" not in results:
                results["batch_diagnostics"] = {}
                for scenario in ("unknown", "changed", "success"):
                    cleanup(lambda scenario=scenario: results["batch_diagnostics"].update({scenario: private_read(f"{app_test}/{scenario}.log")[:2000]}))
            if upgraded:
                cleanup(lambda: device.adb("install", "-r", str(args.debug_apk.resolve()), timeout=120))
            returned = private_read(local_back) == "LOCAL_BACK"
            results["local_shell_marker_verified"] = returned
            if results["status"] == "passed":
                if returned and results.get("ssh_exit_observed"):
                    results["checks"].append("SSH disconnect returns to a usable Android shell")
                else:
                    cleanup_errors.append("Private marker did not prove a usable local shell after SSH exited")
            cleanup(lambda: device.shell("run-as", args.package, "rm", "-f", local_back, check=False))
            absent = device.shell("run-as", args.package, "sh", "-c", f"test ! -e {shlex.quote(app_test)} && echo CLEAN", check=False) == "CLEAN"
            results["terminal_removed_private_credentials"] = absent
            if results["status"] == "passed" and not absent:
                cleanup_errors.append("Terminal command did not remove private test credentials")
            cleanup(lambda: device.shell("run-as", args.package, "rm", "-rf", app_test, check=False))
            cleanup(lambda: subprocess.run(["tmux", "-L", "kokuban_ci", "kill-server"], capture_output=True))
            if process.poll() is None:
                pid_file = server_root / "sshd.pid"
                if pid_file.exists():
                    cleanup(lambda: subprocess.run(["sudo", "-n", "kill", "-TERM", pid_file.read_text().strip()], capture_output=True))
                cleanup(lambda: process.wait(timeout=10))
            server_log.close()
            results["cleanup_errors"] = cleanup_errors
            if cleanup_errors:
                results["status"] = "failed"
            (args.output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
            if cleanup_errors:
                raise RuntimeError("Test cleanup failed: " + "; ".join(cleanup_errors))
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
