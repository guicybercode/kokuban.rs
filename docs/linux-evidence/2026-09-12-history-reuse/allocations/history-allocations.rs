#![allow(dead_code)]
#[path = "../src/graphics.rs"] mod graphics;
#[path = "../src/grid/mod.rs"] mod grid;
#[path = "../src/parser/mod.rs"] mod parser;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
struct CountAlloc;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for CountAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) { System.dealloc(ptr, layout); }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(size, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, size)
    }
}
#[global_allocator] static ALLOCATOR: CountAlloc = CountAlloc;
fn line(grid: &mut grid::Grid) {
    grid.put_ascii(b"x");
    grid.carriage_return();
    grid.newline();
}
fn main() {
    let mut grid = grid::Grid::new(80, 24, 1000);
    for _ in 0..1100 { line(&mut grid); }
    assert_eq!(grid.scrollback_len(), 1000);
    ENABLED.store(true, Ordering::Relaxed);
    for _ in 0..10000 { line(&mut grid); }
    ENABLED.store(false, Ordering::Relaxed);
    assert_eq!(grid.scrollback_len(), 1000);
    println!("lines=10000 cols=80 history=1000 allocations={} requested_bytes={}",
        ALLOCATIONS.load(Ordering::Relaxed), BYTES.load(Ordering::Relaxed));
}
