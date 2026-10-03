#![allow(dead_code)]
#[path="../candidate/src/graphics.rs"] mod graphics;
#[path="../candidate/src/grid/mod.rs"] mod grid;
#[path="../candidate/src/parser/mod.rs"] mod parser;
#[path="../candidate/src/terminal_decoder.rs"] mod terminal_decoder;
#[cfg(test)]
#[path="../candidate/src/input/keyboard.rs"] pub(crate) mod profile_keyboard;
#[cfg(test)]
mod input { pub(crate) use crate::profile_keyboard as keyboard; }
use std::{hint::black_box, time::Instant};
fn decode(decoder: &mut terminal_decoder::TerminalDecoder, grid: &mut grid::Grid, input: &[u8]) {
    for chunk in input.chunks(16384) {
        let mut remaining=chunk;
        while !remaining.is_empty() {
            let step=decoder.feed_until_event(black_box(remaining),grid);
            remaining=&remaining[step.consumed..]; black_box(step.events);
        }
    }
}
fn main() {
    let args:Vec<_>=std::env::args().collect();
    let input=std::fs::read(&args[1]).unwrap();
    let rounds:usize=args[2].parse().unwrap();
    let mut decoder=terminal_decoder::TerminalDecoder::new(parser::ansi::GraphicsSupport {kitty:false,sixel:false});
    let mut grid=grid::Grid::new(80,24,10000);
    if args[3]=="alternate" {grid.enter_alt_screen();}
    decode(&mut decoder,&mut grid,&input);
    let mut values=Vec::new();
    for _ in 0..rounds {
        let start=Instant::now(); decode(&mut decoder,&mut grid,&input);
        values.push(input.len() as f64 /1048576.0 /start.elapsed().as_secs_f64());
        black_box(&grid);
    }
    println!("{values:?}");
}
