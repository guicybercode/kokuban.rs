#!/usr/bin/env python3
"""Compare software raster kernels against a Git revision using rustc -O.

Run on an otherwise idle host, for example:
  python3 scripts/benchmark-software-raster.py --baseline 9fb819d

This measures CPU rasterization of 120x40 cells, not terminal throughput,
presentation latency or performance relative to other terminals. No thresholds.
Both versions receive identical pixels and every output frame is compared.
"""

import argparse
from pathlib import Path
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
type Fill = fn(&mut [u32], (u32, u32), (i32, i32), (u32, u32), u32, u8);

fn measure(draw: Draw, fill: Fill, mode: &str, frames: usize) -> (f64, Vec<u32>) {
    let mut frame = vec![0x203040; 1200 * 800];
    let atlas: Vec<u8> = (0..1024 * 32).map(|i| (i * 73 % 256) as u8).collect();
    let glyph = glyph_atlas::GlyphEntry {
        atlas_x: 13, atlas_y: 5, pixel_w: 9, pixel_h: 17, bearing_x: 0, bearing_y: 0,
    };
    let start = Instant::now();
    for _ in 0..frames {
        for row in 0..40 {
            for column in 0..120 {
                let origin = (column * 10, row * 20);
                if mode == "glyphs" {
                    draw(black_box(&mut frame), (1200, 800), black_box(&atlas),
                        (1024, 32), glyph, origin, black_box(0xf0e0d0));
                } else {
                    fill(black_box(&mut frame), (1200, 800), origin, (10, 20),
                        black_box(0x123456), if mode == "opaque" { 255 } else { 180 });
                }
            }
        }
        black_box(&frame);
    }
    (start.elapsed().as_secs_f64() * 1000.0 / frames as f64, frame)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let samples: usize = args[1].parse().unwrap();
    let frames: usize = args[2].parse().unwrap();
    for mode in ["glyphs", "opaque", "alpha"] {
        let before = || measure(before::draw_glyph_a8, before::fill_rect, mode, frames);
        let after = || measure(after::draw_glyph_a8, after::fill_rect, mode, frames);
        // Warm both versions, then alternate order to reduce thermal/order bias.
        assert_eq!(before().1, after().1);
        for sample in 0..samples {
            let (old, new) = if sample % 2 == 0 {
                (before(), after())
            } else {
                let new = after();
                (before(), new)
            };
            assert_eq!(old.1, new.1, "{mode} sample {sample}");
            println!("{mode} sample={sample} before_ms={:.4} after_ms={:.4} speedup={:.3}",
                old.0, new.0, old.0 / new.0);
        }
    }
}
'''


def positive_integer(value: str) -> int:
    number = int(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return number


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, help="trusted local Git revision")
    parser.add_argument("--samples", type=positive_integer, default=5)
    parser.add_argument("--frames", type=positive_integer, default=100)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    revision = subprocess.check_output(
        ["git", "rev-parse", "--verify", "--end-of-options", f"{args.baseline}^{{commit}}"],
        cwd=root, text=True,
    ).strip()
    baseline = subprocess.check_output(
        ["git", "show", f"{revision}:src/software_raster.rs"], cwd=root,
    )
    print(f"baseline={revision}; candidate=working-tree", flush=True)
    subprocess.run(["rustc", "--version", "--verbose"], check=True)
    with tempfile.TemporaryDirectory(prefix="kokuban-raster-bench-") as temporary:
        directory = Path(temporary)
        (directory / "before.rs").write_bytes(baseline)
        (directory / "after.rs").write_bytes((root / "src/software_raster.rs").read_bytes())
        (directory / "main.rs").write_text(HARNESS, encoding="utf-8")
        executable = directory / "benchmark"
        subprocess.run(["rustc", "--edition=2021", "-O", str(directory / "main.rs"),
                        "-o", str(executable)], check=True)
        subprocess.run([str(executable), str(args.samples), str(args.frames)], check=True)


if __name__ == "__main__":
    main()
