#!/usr/bin/env bash
# Read-only failure evidence. Never reconnect, reboot, or retry a device test.
set -u

output="${1:-target/android-evidence/host-diagnostics}"
serial="${ANDROID_SERIAL:-emulator-5554}"
mkdir -p "$output"
adb_executable="$(command -v adb || true)"
if [[ -z "$adb_executable" ]]; then
  adb_executable="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}/platform-tools/adb"
fi

capture() {
  local name="$1"
  shift
  {
    date -u +'%Y-%m-%dT%H:%M:%SZ'
    timeout --signal=TERM --kill-after=2s 10s "$@"
    printf '\nexit_status=%s\n' "$?"
  } > "$output/$name.txt" 2>&1
}

capture host-memory free -m
capture host-disk df -h
capture host-processes ps -eo pid,ppid,comm,pcpu,pmem,rss
capture host-kernel bash -o pipefail -c 'sudo -n dmesg --ctime | tail -n 200'
capture adb-devices "$adb_executable" devices -l
capture adb-state "$adb_executable" -s "$serial" get-state
capture guest-boot "$adb_executable" -s "$serial" shell getprop sys.boot_completed
capture guest-crashes "$adb_executable" -s "$serial" logcat -d -b crash -t 200
capture guest-kokuban "$adb_executable" -s "$serial" logcat -d -t 500 Kokuban:V AndroidRuntime:E '*:S'
