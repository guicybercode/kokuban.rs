//! Headless production decoder benchmark; excludes PTY, rendering and compositor costs.
//! Run: cargo run --release --example terminal-throughput -- [MiB per sample] [samples]
#![allow(dead_code)]

#[path = "../src/graphics.rs"]
mod graphics;
#[path = "../src/grid/mod.rs"]
mod grid;
#[path = "../src/parser/mod.rs"]
mod parser;
#[path = "../src/terminal_decoder.rs"]
mod terminal_decoder;

// The parser's regression tests also exercise the keyboard response encoder.
#[cfg(test)]
mod input {
    pub mod keyboard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/input/keyboard.rs"
        ));
    }
}

use std::hint::black_box;
use std::time::Instant;

use grid::Grid;
use parser::ansi::GraphicsSupport;
use terminal_decoder::TerminalDecoder;

fn decode(decoder: &mut TerminalDecoder, grid: &mut Grid, input: &[u8]) {
    for chunk in input.chunks(16 * 1024) {
        let mut remaining = chunk;
        while !remaining.is_empty() {
            let step = decoder.feed_until_event(black_box(remaining), grid);
            remaining = &remaining[step.consumed..];
            black_box(step.events);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() > 2 {
        return Err("usage: terminal-throughput [MiB per sample] [samples]".into());
    }
    let mib = args
        .first()
        .map(|s| s.parse::<usize>())
        .transpose()?
        .unwrap_or(16);
    let samples = args
        .get(1)
        .map(|s| s.parse::<usize>())
        .transpose()?
        .unwrap_or(5);
    if mib == 0 || samples == 0 {
        return Err("MiB per sample and samples must be positive".into());
    }
    let target_bytes = mib.checked_mul(1024 * 1024).ok_or("sample size overflow")?;
    let workloads: &[(&str, &[u8])] = &[
        ("ascii", b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\r\n"),
        ("short-lines", b"x\r\n"),
        ("ansi", b"\x1b[32mINFO\x1b[0m building module \x1b[1;34mkokuban\x1b[0m: 0123456789 abcdefghijklmnopqrstuvwxyz\r\n"),
        ("unicode", "日本語 terminal λ café 日本語 terminal λ café 日本語 terminal λ café\r\n".as_bytes()),
    ];
    println!(
        "decoder only; 120x40; scrollback=10000; chunks=16384; MiB/sample={mib}; samples={samples}"
    );
    println!("workload,median_MiB_s,min_MiB_s,max_MiB_s");
    for &(name, line) in workloads {
        let repetitions = target_bytes.div_ceil(line.len());
        let input = line.repeat(repetitions);
        let mut rates = Vec::with_capacity(samples);
        for _ in 0..samples {
            let mut decoder = TerminalDecoder::new(GraphicsSupport {
                kitty: false,
                sixel: false,
            });
            let mut grid = Grid::new(120, 40, 10_000);
            // Fill history and warm the same code/data paths before timing.
            decode(&mut decoder, &mut grid, &input);
            let start = Instant::now();
            decode(&mut decoder, &mut grid, &input);
            let elapsed = start.elapsed().as_secs_f64();
            black_box(&grid);
            rates.push(input.len() as f64 / (1024.0 * 1024.0) / elapsed);
        }
        rates.sort_by(f64::total_cmp);
        let middle = samples / 2;
        let median = if samples % 2 == 0 {
            (rates[middle - 1] + rates[middle]) / 2.0
        } else {
            rates[middle]
        };
        println!(
            "{name},{median:.2},{:.2},{:.2}",
            rates[0],
            rates[samples - 1]
        );
    }
    Ok(())
}
