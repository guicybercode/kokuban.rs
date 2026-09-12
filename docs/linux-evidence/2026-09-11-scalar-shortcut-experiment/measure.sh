#!/bin/sh
set -eu
export XDG_RUNTIME_DIR=/validation/wayland-runtime
mkdir -m 700 -p "$XDG_RUNTIME_DIR"
export WAYLAND_DISPLAY=wayland-kokuban-goal
unset DISPLAY WAYLAND_SOCKET
weston --backend=headless-backend.so --use-pixman \
  --socket="$WAYLAND_DISPLAY" --idle-time=0 --width=1280 --height=900 \
  --log=/validation/weston.log &
weston_pid=$!
trap 'kill "$weston_pid" 2>/dev/null || true; wait "$weston_pid" 2>/dev/null || true' EXIT
attempt=0
while [ ! -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]; do
  kill -0 "$weston_pid"
  attempt=$((attempt + 1))
  test "$attempt" -lt 100
  sleep 0.1
done
benchmark_cpu="$(python3 -c 'import os; print(min(os.sched_getaffinity(0)))')"
for screen in alternate primary; do
  taskset -c "$benchmark_cpu" python3 /workspace/scripts/compare-kokuban-revisions.py \
    --before /validation/kokuban-before --after /validation/kokuban-after \
    --before-ref 8dafe9e5ba4106994cae27d702778a277f108199 \
    --after-ref working-tree-scalar-boundary-and-ctrl-insert \
    --artifacts-dir "/validation/paired-$screen" --backend wayland \
    --screen "$screen" --samples 5 --bytes 33554432 --timeout 120 \
    --environment-note "Local Docker arm64 VM on macOS; Weston headless Pixman; CPU $benchmark_cpu; no physical Omarchy/GPU/display or input-to-photon measurement"
done
