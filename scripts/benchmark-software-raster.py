#!/usr/bin/env python3
"""Compare software raster kernels against a Git revision using rustc -O.

Run on an otherwise idle host, for example:
  python3 scripts/benchmark-software-raster.py --baseline 9fb819d

This measures CPU rasterization of 120x40 cells, not terminal throughput,
presentation latency or performance relative to other terminals. No thresholds.
Both versions receive identical pixels. Untimed checks compare pixels immediately
after a pass and after individual operations; timed final frames are also checked.
Use --artifacts-dir to preserve the exact sources, harness, binary and raw results.
"""

import argparse
from contextlib import nullcontext
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile


HARNESS = r'''
#[allow(dead_code)]
mod glyph_atlas {
    #[derive(Clone, Copy)]
    pub struct GlyphEntry {
        pub atlas_x: u32, pub atlas_y: u32, pub pixel_w: u32, pub pixel_h: u32,
        pub bearing_x: i32, pub bearing_y: i32,
    }
}
#[allow(dead_code)]
#[path = "before.rs"] mod before;
#[allow(dead_code)]
#[path = "after.rs"] mod after;
use std::{hint::black_box, time::Instant};
type Draw = fn(&mut [u32], (u32, u32), &[u8], (u32, u32),
    glyph_atlas::GlyphEntry, (i32, i32), u32);
type DrawRgba = fn(&mut [u32], (u32, u32), &[u8], (u32, u32),
    glyph_atlas::GlyphEntry, (i32, i32));
type Fill = fn(&mut [u32], (u32, u32), (i32, i32), (u32, u32), u32, u8);
type Image = fn(&mut [u32], (u32, u32), &[u8], (u32, u32), (f32, f32, f32, f32));

#[derive(Clone, Copy)]
struct Kernels { glyph: Draw, rgba: DrawRgba, fill: Fill, image: Image }

#[derive(Clone, Copy)]
enum Mode { Glyphs, Rgba, Opaque, Alpha, Image }

impl Mode {
    fn parse(value: &str) -> Self {
        match value {
            "glyphs" => Self::Glyphs, "rgba" => Self::Rgba,
            "opaque" => Self::Opaque, "alpha" => Self::Alpha,
            "image" => Self::Image, _ => panic!("unknown mode: {value}"),
        }
    }
}

struct Case {
    width: u32,
    frame_size: (u32, u32),
    a8: Vec<u8>,
    rgba: Vec<u8>,
    image: Vec<u8>,
    image_size: (u32, u32),
    glyph: glyph_atlas::GlyphEntry,
}

fn coverage(index: usize) -> u8 {
    // Rotate at each atlas row so even a one-pixel glyph sees all alpha classes.
    match (index + index / 1024) % 8 {
        0 => 0, 1 => 255, 2 => 1, 3 => 254, 4 => 64, 5 => 128, 6 => 192,
        _ => (index.wrapping_mul(73) % 256) as u8,
    }
}

fn rgba_pixels(count: usize) -> Vec<u8> {
    (0..count).flat_map(|index| [
        (index.wrapping_mul(29) % 256) as u8,
        (index.wrapping_mul(53).wrapping_add(71) % 256) as u8,
        (index.wrapping_mul(97).wrapping_add(31) % 256) as u8,
        coverage(index),
    ]).collect()
}

impl Case {
    fn new(width: u32) -> Self {
        let image_size = (width + 3, 11);
        Self {
            width,
            frame_size: (120 * (width + 1), 800),
            a8: (0..1024 * 32).map(coverage).collect(),
            rgba: rgba_pixels(1024 * 32),
            image: rgba_pixels((image_size.0 * image_size.1) as usize),
            image_size,
            glyph: glyph_atlas::GlyphEntry {
                atlas_x: 13, atlas_y: 5, pixel_w: width, pixel_h: 17,
                bearing_x: 0, bearing_y: 0,
            },
        }
    }
}

fn draw_cell(kernels: Kernels, mode: Mode, case: &Case, frame: &mut [u32],
    size: (u32, u32), origin: (i32, i32), rgb: u32, alpha: u8) {
    match mode {
        Mode::Glyphs => (kernels.glyph)(black_box(frame), size, black_box(&case.a8),
            (1024, 32), case.glyph, origin, black_box(rgb)),
        Mode::Rgba => (kernels.rgba)(black_box(frame), size, black_box(&case.rgba),
            (1024, 32), case.glyph, origin),
        Mode::Opaque | Mode::Alpha => (kernels.fill)(black_box(frame), size, origin,
            (case.width, 20), black_box(rgb),
            black_box(if matches!(mode, Mode::Opaque) { 255 } else { alpha })),
        Mode::Image => (kernels.image)(black_box(frame), size, black_box(&case.image),
            case.image_size, (origin.0 as f32 + 0.25, origin.1 as f32 - 0.25,
                case.width as f32, 17.0)),
    }
}

fn draw_pass(kernels: Kernels, mode: Mode, case: &Case, frame: &mut [u32]) {
    for row in 0..40 {
        for column in 0..120 {
            let origin = (column * (case.width + 1) as i32, row * 20);
            draw_cell(kernels, mode, case, frame, case.frame_size, origin, 0xf0e0d0, 180);
        }
    }
    black_box(frame);
}

#[inline(never)]
fn measure(kernels: Kernels, mode: Mode, case: &Case, frames: usize) -> (u128, Vec<u32>) {
    let mut frame = vec![0x203040; (case.frame_size.0 * case.frame_size.1) as usize];
    let start = Instant::now();
    for _ in 0..frames {
        draw_pass(kernels, mode, case, &mut frame);
    }
    (start.elapsed().as_nanos(), frame)
}

fn compare_frames(before: &[u32], after: &[u32], context: &str) {
    assert_eq!(before.len(), after.len(), "{context}: frame length");
    if let Some(index) = before.iter().zip(after).position(|(old, new)| old != new) {
        panic!("{context}: pixel {index}: before={:08x}, after={:08x}", before[index], after[index]);
    }
}

// Repeated blending can converge and hide errors. Check one complete pass,
// then compare after every draw in three independent, clipped color sequences.
fn validate(before: Kernels, after: Kernels, mode: Mode, case: &Case) -> usize {
    let size = case.frame_size;
    let mut old = vec![0x203040; (size.0 * size.1) as usize];
    let mut new = old.clone();
    draw_pass(before, mode, case, &mut old);
    draw_pass(after, mode, case, &mut new);
    compare_frames(&old, &new, "immediate full pass");
    let size = (case.width + 5, 23);
    let operations = [
        ((0, 0), 0xf0e0d0, 180),
        ((-3, -2), 0x102938, 64),
        ((case.width as i32 + 2, 15), 0xaf13abfe, 255),
        ((1, 1), 0x00ff01, 0),
        ((-100, -100), 0xff7819, 192),
        ((0, 0), 0x719234, 1),
    ];
    for initial in [0x203040, 0xabffffff, 0x71000000] {
        let mut old = vec![initial; (size.0 * size.1) as usize];
        let mut new = old.clone();
        for (index, &(origin, rgb, alpha)) in operations.iter().enumerate() {
            draw_cell(before, mode, case, &mut old, size, origin, rgb, alpha);
            draw_cell(after, mode, case, &mut new, size, origin, rgb, alpha);
            compare_frames(&old, &new, &format!("immediate sequence initial={initial:08x} step={index}"));
        }
    }
    1 + 3 * operations.len()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let samples: usize = args[1].parse().unwrap();
    let frames: usize = args[2].parse().unwrap();
    let warmup_frames: usize = args[3].parse().unwrap();
    let widths: Vec<u32> = args[4].split(',').map(|value| value.parse().unwrap()).collect();
    // Both versions always enter the same non-inlined measurement loop through
    // opaque function pointers; compiler inlining costs must not choose a side.
    let before = black_box(Kernels { glyph: before::draw_glyph_a8, rgba: before::draw_glyph_rgba,
        fill: before::fill_rect, image: before::draw_image_rgba });
    let after = black_box(Kernels { glyph: after::draw_glyph_a8, rgba: after::draw_glyph_rgba,
        fill: after::fill_rect, image: after::draw_image_rgba });
    let mut case_index = 0;
    for width in widths {
        let case = Case::new(width);
        for name in args[5].split(',') {
            let mode = Mode::parse(name);
            let comparisons = validate(before, after, mode, &case);
            let warm_before = measure(black_box(before), mode, &case, warmup_frames);
            let warm_after = measure(black_box(after), mode, &case, warmup_frames);
            compare_frames(&warm_before.1, &warm_after.1, "warmup final frame");
            println!(r#"{{"kind":"validation","mode":"{name}","width":{width},"immediate_frame_comparisons":{comparisons},"passed":true}}"#);
            // The parity includes case number so odd sample counts do not
            // always give the same implementation the first run in every case.
            for sample in 0..samples {
                let ab = (sample + case_index) % 2 == 0;
                let (old, new) = if ab {
                    (measure(black_box(before), mode, &case, frames),
                     measure(black_box(after), mode, &case, frames))
                } else {
                    let new = measure(black_box(after), mode, &case, frames);
                    (measure(black_box(before), mode, &case, frames), new)
                };
                compare_frames(&old.1, &new.1, &format!("{name} width={width} sample={sample} final frame"));
                let order = if ab { "AB" } else { "BA" };
                println!(r#"{{"kind":"sample","mode":"{name}","width":{width},"sample":{sample},"order":"{order}","frames":{frames},"before_elapsed_ns":{},"after_elapsed_ns":{},"final_pixels_equal":true}}"#, old.0, new.0);
            }
            case_index += 1;
        }
    }
}
'''


def positive_integer(value: str) -> int:
    number = int(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return number


def resolve_revision(root: Path, reference: str) -> str:
    """Resolve a trusted local reference before using it in git show."""
    return subprocess.check_output(
        ["git", "rev-parse", "--verify", "--end-of-options", f"{reference}^{{commit}}"],
        cwd=root, text=True,
    ).strip()


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def artifact_directory(value: str) -> Path:
    directory = Path(value).expanduser().resolve()
    if directory.exists() and (not directory.is_dir() or any(directory.iterdir())):
        raise argparse.ArgumentTypeError("artifacts directory must be absent or empty")
    return directory


def summarize_results(output: str, widths: list[int], modes: list[str],
                      samples: int, frames: int) -> list[dict]:
    """Reject missing/duplicate observations instead of reporting partial success."""
    cases = {(width, mode): {"mode": mode, "width": width, "samples": []}
             for width in widths for mode in modes}
    for line in output.splitlines():
        record = json.loads(line)
        key = (record["width"], record["mode"])
        if key not in cases:
            raise ValueError(f"unexpected benchmark case: {key}")
        case = cases[key]
        if record["kind"] == "validation":
            if "correctness" in case or record["passed"] is not True:
                raise ValueError(f"duplicate or failed validation: {key}")
            if record["immediate_frame_comparisons"] != 19:
                raise ValueError(f"missing immediate pixel comparisons: {key}")
            case["correctness"] = record
        elif record["kind"] == "sample":
            if record["frames"] != frames or record["final_pixels_equal"] is not True:
                raise ValueError(f"invalid final-frame sample: {key}")
            before_ns, after_ns = record["before_elapsed_ns"], record["after_elapsed_ns"]
            if (type(before_ns) is not int or type(after_ns) is not int
                    or min(before_ns, after_ns) <= 0):
                raise ValueError(f"nonpositive or noninteger duration: {key}")
            record["before_ms_per_frame"] = before_ns / frames / 1_000_000
            record["after_ms_per_frame"] = after_ns / frames / 1_000_000
            record["paired_speedup"] = before_ns / after_ns
            record["paired_throughput_change_pct"] = (before_ns / after_ns - 1) * 100
            record["paired_time_change_pct"] = (after_ns / before_ns - 1) * 100
            case["samples"].append(record)
        else:
            raise ValueError(f"unexpected benchmark record: {record['kind']}")
    for case_index, ((width, mode), case) in enumerate(cases.items()):
        observations = case["samples"]
        if "correctness" not in case or [row["sample"] for row in observations] != list(range(samples)):
            raise ValueError(f"missing, duplicate or unordered observations: {(width, mode)}")
        expected_order = ["AB" if (sample + case_index) % 2 == 0 else "BA"
                          for sample in range(samples)]
        if [row["order"] for row in observations] != expected_order:
            raise ValueError(f"incorrect paired order: {(width, mode)}")
        case["geometry"] = {
            "columns": 120, "rows": 40, "frame_width": 120 * (width + 1),
            "frame_height": 800, "cell_pitch_x": width + 1, "cell_pitch_y": 20,
            "draw_width": width, "draw_height": 20 if mode in ("opaque", "alpha") else 17,
        }
        if mode == "image":
            case["geometry"]["source_image_size"] = [width + 3, 11]
            case["geometry"]["image_origin_offset"] = [0.25, -0.25]
        case["summary"] = {
            f"median_{field}": statistics.median(row[field] for row in observations)
            for field in ("before_ms_per_frame", "after_ms_per_frame", "paired_speedup",
                          "paired_throughput_change_pct", "paired_time_change_pct")
        }
        case["summary"]["after_faster_pairs"] = sum(
            row["after_elapsed_ns"] < row["before_elapsed_ns"] for row in observations)
        case["summary"]["equal_pairs"] = sum(
            row["after_elapsed_ns"] == row["before_elapsed_ns"] for row in observations)
    return list(cases.values())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, help="trusted local Git revision")
    parser.add_argument("--candidate", help="trusted local Git revision; default: working tree")
    parser.add_argument("--artifacts-dir", type=artifact_directory,
                        help="preserve sources, harness, binary, logs and provenance here")
    parser.add_argument("--samples", type=positive_integer, default=5)
    parser.add_argument("--frames", type=positive_integer, default=100)
    parser.add_argument("--warmup-frames", type=positive_integer, default=3)
    parser.add_argument("--widths", type=int, nargs="+", default=[9],
                        choices=(1, 7, 8, 9, 15, 16, 17, 32), help="actual drawn widths in pixels")
    parser.add_argument("--modes", nargs="+", default=["glyphs", "rgba", "opaque", "alpha", "image"],
                        choices=("glyphs", "rgba", "opaque", "alpha", "image"))
    args = parser.parse_args()
    if len(set(args.widths)) != len(args.widths) or len(set(args.modes)) != len(args.modes):
        parser.error("widths and modes must not contain duplicates")
    root = Path(__file__).resolve().parent.parent
    revision = resolve_revision(root, args.baseline)
    candidate_revision = resolve_revision(root, args.candidate) if args.candidate else None
    baseline = subprocess.check_output(
        ["git", "show", f"{revision}:src/software_raster.rs"], cwd=root,
    )
    candidate = (subprocess.check_output(
        ["git", "show", f"{candidate_revision}:src/software_raster.rs"], cwd=root,
    ) if candidate_revision else (root / "src/software_raster.rs").read_bytes())
    rustc_version = subprocess.check_output(["rustc", "--version", "--verbose"], text=True)
    print(f"baseline={revision}; candidate={candidate_revision or 'working-tree'}", flush=True)
    print(rustc_version, end="", flush=True)
    context = (nullcontext(args.artifacts_dir) if args.artifacts_dir else
               tempfile.TemporaryDirectory(prefix="kokuban-raster-bench-"))
    with context as temporary:
        directory = Path(temporary)
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "before.rs").write_bytes(baseline)
        (directory / "after.rs").write_bytes(candidate)
        (directory / "main.rs").write_text(HARNESS, encoding="utf-8")
        executable = directory / "benchmark"
        compile_command = ["rustc", "--edition=2021", "-O", "main.rs", "-o", "benchmark"]
        run_command = [str(executable), str(args.samples), str(args.frames),
                       str(args.warmup_frames), ",".join(map(str, args.widths)), ",".join(args.modes)]
        provenance = {
            "schema_version": 1,
            "started_at_utc": datetime.now(timezone.utc).isoformat(),
            "baseline": {"requested_ref": args.baseline, "revision": revision},
            "candidate": {"requested_ref": args.candidate, "revision": candidate_revision,
                          "source": "git" if candidate_revision else "working-tree"},
            "driver_revision": resolve_revision(root, "HEAD"),
            "source_files_identical": baseline == candidate,
            "comparison_mode": "same-source-control" if baseline == candidate else "different-sources",
            "rustc_version": rustc_version,
            "platform": platform.platform(),
            "machine": platform.machine(),
            "compile_command": compile_command,
            "run_command": run_command,
            "configuration": {"samples": args.samples, "frames": args.frames,
                              "warmup_frames": args.warmup_frames,
                              "widths": args.widths, "modes": args.modes},
            "measurement": {
                "scope": "CPU software raster kernels in one shared harness executable",
                "clock": "Rust Instant; integer nanoseconds for each complete frame batch",
                "allocation_timed": False,
                "pixel_comparisons_timed": False,
                "warmup": "both revisions per case; excluded from samples",
                "order": "AB/BA alternates by sample and starting order alternates by case",
                "coverage": "deterministic mix of 0, 255, 1, 254, 64, 128, 192 and varying bytes",
                "initial_frame_rgb": "0x203040",
                "limitations": [
                    "CPU microbenchmark; no terminal, font shaping, display or input latency measured",
                    "Timed frames accumulate blending; immediate untimed checks prevent convergence masking pixel errors",
                    "Paired ratios are descriptive; no significance threshold or performance assertion",
                    "Same-source control measures variability; LLVM may merge identical functions",
                    "Host contention, thermal state and cache effects remain possible",
                ],
            },
            "sha256": {name: sha256((directory / name).read_bytes())
                       for name in ("before.rs", "after.rs", "main.rs")},
            "driver_sha256": sha256(Path(__file__).read_bytes()),
            "status": "prepared",
        }
        result_file = directory / "results.json"
        result_file.write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")
        try:
            compiled = subprocess.run(compile_command, cwd=directory, capture_output=True, text=True)
            (directory / "compile.stdout.log").write_text(compiled.stdout, encoding="utf-8")
            (directory / "compile.stderr.log").write_text(compiled.stderr, encoding="utf-8")
            compiled.check_returncode()
            provenance["sha256"]["benchmark"] = sha256(executable.read_bytes())
            measured = subprocess.run(run_command, capture_output=True, text=True)
            (directory / "benchmark.stdout.log").write_text(measured.stdout, encoding="utf-8")
            (directory / "benchmark.stderr.log").write_text(measured.stderr, encoding="utf-8")
            measured.check_returncode()
            provenance["cases"] = summarize_results(measured.stdout, args.widths, args.modes,
                                                    args.samples, args.frames)
            provenance["status"] = "complete"
            for case in provenance["cases"]:
                summary = case["summary"]
                print(f"{case['mode']} width={case['width']} "
                      f"before_ms={summary['median_before_ms_per_frame']:.6f} "
                      f"after_ms={summary['median_after_ms_per_frame']:.6f} "
                      f"paired_speedup={summary['median_paired_speedup']:.4f}", flush=True)
        except (subprocess.CalledProcessError, ValueError, KeyError) as error:
            provenance["status"] = "failed"
            provenance["error"] = str(error)
            print(getattr(error, "stderr", "") or "", end="", flush=True)
            raise
        finally:
            result_file.write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
