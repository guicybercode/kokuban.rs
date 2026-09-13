# Prolonged Linux video observation

The optional soak in `Linux release resource measurements` keeps one Kokuban
process and one mpv process alive while a local 320×180, 12 fps, six-second
lossless clip loops. It checks cyclic visible frame IDs, samples process
resources, then verifies a stable pause and clean shutdown. This is a bounded
Xvfb observation, not an A/B comparison, GPU benchmark or physical-display test.

Validate the observer with a short pilot before a longer run:

```sh
gh workflow run linux-performance.yml --ref perf/video-soak \
  -f video_soak_seconds=60
gh workflow run linux-performance.yml --ref perf/video-soak \
  -f video_soak_seconds=900
```

`video_soak_seconds=0` is the default and disables the soak. Enabled runs accept
30–1,800 seconds; the existing resource and short-video checks run first.
The job has a 45-minute bound, with additional bounded driver and subprocess
timeouts. The soak retains the exact release executable, Git source archive,
revision and checksums alongside the reports. Rust is pinned to 1.94.1 and
dependencies use `Cargo.lock`; record the runner and package versions when
interpreting or repeating a run.

The observer requests an XWD capture about every 0.5 seconds and resource
samples about every second. Its existing pixel classifier checks selected
locations and the fixture's frame ID; it does not compare every pixel of every
frame. Excessive capture gaps, stalled visible progress, invalid observations
and process-identity changes fail explicitly. Initial, midpoint and final
paused PNGs remain available for fuller inspection. Continuous frame images
are not retained.

CPU is each process's user+system time divided by elapsed time; 100% means one
logical core. RSS is sampled and `VmHWM` covers the process lifetime. Reports
preserve raw samples, minute windows and, when available, a post-180-second
window. Runs lasting at least 420 seconds also compare separate 180–300-second
and final-120-second windows. These windows do not establish that warming has
finished or prove absence of a leak.

The image-cache setting is 256 MiB; it is not an RSS limit and actual cache
occupancy is not measured. The driver's default 1,024 MiB per-process RSS guard
and bounded logs/artifacts protect the experiment; they are not product
performance thresholds. Capture, PNG encoding and report writing add observer
overhead. This small silent clip does not establish audio, remote playback or
general video performance.
