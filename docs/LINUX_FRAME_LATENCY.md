# X11 input to observed terminal frames

`scripts/compare-terminal-frame-latency.py` complements the [throughput and DSR comparison](LINUX_PERFORMANCE.md). It measures a synthetic key event followed by a verified change in a large region of the terminal window. A terminal protocol reply alone cannot satisfy this test.

The interval is an **upper bound from before `xdotool` starts until the matching `xwd` capture finishes**. It includes process launch, XTest delivery, terminal input handling, the controlled PTY application, terminal rendering, X11 readback and observer scheduling. It does not measure physical keyboard latency, compositor presentation, vblank, GPU time alone or input-to-photon latency. Xvfb results describe its virtual software display.

## Run

Use Linux with the four terminal executables, Python 3, `xdotool`, `xwd`, `fc-match`, and DejaVu Sans Mono already installed. `xvfb-run` provides an isolated display; `lscpu`, `xdpyinfo` and `glxinfo` provide optional provenance. The script records missing optional commands and never installs software. Build Kokuban before starting a measured run and avoid competing builds or benchmarks on the same host.

```sh
xvfb-run -a -s '-screen 0 1280x1024x24 -nolisten tcp' \
  env LIBGL_ALWAYS_SOFTWARE=1 \
  python3 scripts/compare-terminal-frame-latency.py \
    --kokuban "$PWD/target/release/kokuban" \
    --output-dir /tmp/kokuban-frame-latency \
    --samples 3 --events 40 --warmup 5 \
    --environment-note 'Isolated Xvfb; Mesa software renderer; otherwise idle host'
```

The output directory must not exist. Explicit `--ghostty`, `--kitty` and `--alacritty` paths override the executables on `PATH`. `--terminals kokuban kitty` selects a smaller comparison. At least two terminals must finish successfully for the comparison to pass. For a physical X11 desktop, run on its `DISPLAY` and describe the actual GPU, display and compositor in `--environment-note`; the measurement still ends at window readback, not physical presentation.

The manual `Linux four-terminal observed frame latency` workflow builds its selected exact Kokuban revision with Rust 1.94.1 in the Ubuntu image used by processing run `34743860822`. It pins the three competitor packages and fonts from that run, keeps Xvfb alive with `-noreset`, and uploads available evidence even on failure. Its default `events=5`, `samples=3` is a functional smoke with weak tail resolution; select `events=40` for the default comparison above. The workflow source/harness revision and the independently selected Kokuban source revision are recorded separately.

Changes to this harness or workflow on the dedicated `perf/input-frame-latency` branch also run that bounded smoke automatically against the recorded baseline source.

The script reuses the existing comparison's isolated configurations and untimed cell-size calibration. It requests 80 columns, 24 rows, DejaVu Sans Mono at 14 logical pixels, no padding, and the alternate screen. Two preflight launches per terminal observe natural cell dimensions and verify the supported spacing adjustments. These launches also use the existing DSR probe; their results are retained separately and never enter frame timing statistics. Incompatible versions, unavailable executables or unmatchable cell dimensions fail the run instead of silently comparing different geometry.

Window discovery ignores auxiliary windows and requires a unique calibrated window. Some terminals leave spare edge pixels: Kitty 0.45.0 opened a 721×409 window for an 80×24 PTY with 9×17 cells in the initial Linux smoke. When each surplus is smaller than one cell, the runner requests the exact 720×408 window size before warmup and records the adjustment. It then requires both XWD and PTY dimensions to agree; it does not accept a larger rendering area as equivalent.

## What each sample proves

1. A fresh terminal process opens one visible X11 window with the calibrated pixel dimensions. The controlled child waits for its PTY geometry to agree.
2. The child hides the cursor and paints an opaque truecolor rectangle, leaving a one-cell border. The observer verifies the whole rectangle before beginning.
3. Each key alternates between `b` and `a`. The raw PTY child requires exactly that byte, writes the corresponding precomputed rectangle and records the event identity and PTY dimensions.
4. `xwd` captures are polled until **every RGB pixel in the region** equals the next color. A partial update, the previous color, a lost key or changed geometry cannot count as a completed sample.
5. Warmup events are excluded. Measured events retain their input timestamp, injection completion, child receipt/write acknowledgment, capture start/end, pixel validation completion and exact color. Terminal order rotates between fresh-process samples.
6. The child must report the expected number of events and exit successfully. Failure paths retain available observations and stop the owned terminal and PTY child.

Default settings collect 120 measured key/frame pairs per terminal across three processes, plus five warmup events per process. The interval between unsuccessful captures defaults to 1 ms, in addition to capture and validation overhead; `--poll-interval` records any adjustment. The observer can perturb the workload, especially through synchronous X11 readback. Its measured overhead is reported and is **not subtracted** from the upper bound.

## Read the evidence

`report.json` includes binary versions and hashes, configuration and helper hashes, environment observations, cell calibration, execution order and per-terminal summaries. Each process directory contains its configuration, terminal log, controlled-child event records, `sample.json`, and initial, first-transition and final XWD snapshots. The first-transition snapshot preserves the second color even if an even event count finishes on the initial color. A color timeout also retains its final unmatched snapshot and partial capture observations. Every successful target capture has an XWD SHA-256 in the raw sample; only these selected captures are retained as files, outside their timed intervals.

The main summary is `input_to_observed_frame_upper_bound_seconds`. Separate distributions report `injection_seconds`, `observer_capture_seconds` and `observer_validation_seconds`. Capture timing covers the `xwd` subprocess and transfer; validation timing covers XWD decoding and RGB checking. The child write timestamp is a post-write acknowledgment: scheduler preemption can make it occur after the observer has already captured the updated pixels.

Median, p95 and p99 use the recorded samples and the nearest-rank percentile method. With fewer than 100 observations, p99 is the maximum; the report flags that limited tail resolution. Events from one process are correlated, so inspect the retained per-process samples as well as pooled statistics. This harness supplies no overall terminal ranking, confidence interval or universal performance threshold. It verifies opaque background updates, not text shaping, Unicode fidelity or application responsiveness under sustained output.

The local regression suite needs no display or terminal executables:

```sh
python3 -m unittest scripts/test_compare_terminal_frame_latency.py
```

It exercises a real PTY for alternating input, duplicate-input rejection and resize detection; synthetic XWD fixtures cover incomplete frames, channel order and truncation; lifecycle tests cover display failure and process termination. Passing these tests validates the harness logic, not Linux terminal runtime behavior. Preserve an actual Linux run before making performance claims.
