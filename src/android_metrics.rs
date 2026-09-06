//! Opt-in timing evidence without logging input text or terminal contents.
use std::time::Instant;

pub(crate) struct FrameMetrics {
    enabled: bool,
    epoch: Instant,
    frame: u64,
    input: Option<Instant>,
    output_after_input: bool,
    geometry: Option<Geometry>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Geometry {
    bounds: [u32; 4],
    cell: [u32; 2],
    grid: [u16; 2],
    scroll_offset: usize,
}

impl FrameMetrics {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            epoch: Instant::now(),
            frame: 0,
            input: None,
            output_after_input: false,
            geometry: None,
        }
    }

    pub(crate) fn input(&mut self) {
        if self.enabled {
            self.input = Some(Instant::now());
            self.output_after_input = false;
        }
    }

    pub(crate) fn output(&mut self) {
        if self.enabled && self.input.is_some() {
            self.output_after_input = true;
        }
    }

    pub(crate) fn presented(&mut self, render_started: Instant) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        self.frame += 1;
        let latency = if self.output_after_input {
            self.output_after_input = false;
            self.input
                .take()
                .map(|input| now.duration_since(input).as_micros())
        } else {
            None
        };
        log::info!(
            "metric frame={} monotonic_us={} render_us={} input_to_output_present_us={}",
            self.frame,
            now.duration_since(self.epoch).as_micros(),
            now.duration_since(render_started).as_micros(),
            latency.map_or_else(|| "null".into(), |value| value.to_string())
        );
    }

    pub(crate) fn suspend(&mut self) {
        self.input = None;
        self.output_after_input = false;
        self.geometry = None;
    }

    /// Record only changed geometry after a successful presentation. Coordinates
    /// are native surface pixels; no cell contents or selection text are logged.
    pub(crate) fn geometry(
        &mut self,
        bounds: [u32; 4],
        cell: [u32; 2],
        grid: [u16; 2],
        scroll_offset: usize,
    ) {
        if !self.enabled {
            return;
        }
        let geometry = Geometry {
            bounds,
            cell,
            grid,
            scroll_offset,
        };
        if self.geometry == Some(geometry) {
            return;
        }
        self.geometry = Some(geometry);
        log::info!(
            "metric geometry left={} top={} right={} bottom={} cell_width={} cell_height={} columns={} rows={} scroll_offset={}",
            bounds[0], bounds[1], bounds[2], bounds[3], cell[0], cell[1], grid[0], grid[1], scroll_offset
        );
    }
}
