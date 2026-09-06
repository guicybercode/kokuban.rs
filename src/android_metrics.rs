//! Opt-in timing evidence without logging input text or terminal contents.
use std::time::Instant;

pub(crate) struct FrameMetrics {
    enabled: bool,
    epoch: Instant,
    frame: u64,
    input: Option<Instant>,
    output_after_input: bool,
}

impl FrameMetrics {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            epoch: Instant::now(),
            frame: 0,
            input: None,
            output_after_input: false,
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
    }
}
