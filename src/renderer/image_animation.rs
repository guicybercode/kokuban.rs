//! Platform-independent Kitty animation canvases and deterministic playback.
//!
//! Frames own full RGBA canvases. The image store accounts for every canvas;
//! its displayed image is only an `Arc` alias. Decode/composition scratch space
//! and render snapshots held across a mutation are temporary extra allocations.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::parser::kitty_graphics::{
    KittyAnimationControl, KittyAnimationState, KittyBlendMode, KittyFrameComposition,
    KittyFrameUpload, KittyLoopCount,
};

pub(crate) const MAX_FRAMES_PER_IMAGE: usize = 256;
pub(crate) const MAX_RETAINED_FRAMES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationError {
    Unsupported,
    ImageNotFound,
    FrameNotFound,
    InvalidDimensions,
    InvalidRectangle,
    OverlappingComposition,
    TooLarge,
    TooManyFrames,
    AllocationFailed,
}

impl AnimationError {
    pub fn response_message(self) -> &'static str {
        match self {
            Self::Unsupported => "ENOTSUP:animation is not supported by this renderer",
            Self::ImageNotFound => "ENOENT:image not found",
            Self::FrameNotFound => "ENOENT:animation frame not found",
            Self::InvalidDimensions => "EINVAL:invalid animation frame dimensions or data",
            Self::InvalidRectangle => "EINVAL:animation rectangle is outside the image",
            Self::OverlappingComposition => "EINVAL:animation composition rectangles overlap",
            Self::TooLarge => "ENOSPC:animation exceeds image cache budget",
            Self::TooManyFrames => "ENOSPC:animation frame limit exceeded",
            Self::AllocationFailed => "ENOMEM:cannot allocate animation frame",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AnimationUpdate {
    pub changed: bool,
    pub next_deadline: Option<Instant>,
}

pub(crate) struct AnimationFrame {
    pub pixels: Arc<[u8]>,
    gap: Duration,
}

/// Stored separately from static images, so an ordinary image needs no frame
/// vector, clock, or duplicate pixel allocation.
pub(crate) struct Animation {
    frames: Vec<AnimationFrame>,
    current: usize,
    displayed: usize,
    state: KittyAnimationState,
    loops: KittyLoopCount,
    completed_loops: u64,
    frame_started: Instant,
    waiting_at_end: bool,
}

impl Animation {
    pub fn new(root: Arc<[u8]>, now: Instant) -> Self {
        Self {
            frames: vec![AnimationFrame {
                pixels: root,
                gap: Duration::ZERO,
            }],
            current: 0,
            displayed: 0,
            state: KittyAnimationState::Stopped,
            loops: KittyLoopCount::Infinite,
            completed_loops: 0,
            frame_started: now,
            waiting_at_end: false,
        }
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub fn active_pixels(&self) -> &Arc<[u8]> {
        &self.frames[self.displayed].pixels
    }

    fn index(&self, frame: u32) -> Result<usize, AnimationError> {
        frame
            .checked_sub(1)
            .map(|index| index as usize)
            .filter(|&index| index < self.frames.len())
            .ok_or(AnimationError::FrameNotFound)
    }

    pub fn prepare_upload(
        &self,
        pixels: &[u8],
        width: u32,
        height: u32,
        canvas_width: u32,
        canvas_height: u32,
        params: &KittyFrameUpload,
    ) -> Result<(usize, AnimationFrame), AnimationError> {
        validate_rectangle(
            params.x,
            params.y,
            width,
            height,
            canvas_width,
            canvas_height,
        )?;
        let edited = params
            .edit_frame
            .map(|frame| self.index(frame.get()))
            .transpose()?;
        let index = edited.unwrap_or(self.frames.len());
        if index >= MAX_FRAMES_PER_IMAGE {
            return Err(AnimationError::TooManyFrames);
        }
        // The existing frame is the canvas during an edit; c/Y only select the
        // canvas of a newly appended frame.
        let base = match edited {
            Some(index) => Some(index),
            None => params
                .base_frame
                .map(|frame| self.index(frame.get()))
                .transpose()?,
        };
        let mut canvas = match base {
            Some(index) => copy_pixels(&self.frames[index].pixels)?,
            None => solid_canvas(self.frames[0].pixels.len(), params.background_rgba)?,
        };
        compose_pixels(
            pixels,
            width,
            0,
            0,
            &mut canvas,
            canvas_width,
            params.x,
            params.y,
            width,
            height,
            params.blend,
        );
        let old_gap = edited.map_or(Duration::from_millis(40), |index| self.frames[index].gap);
        Ok((
            index,
            AnimationFrame {
                pixels: Arc::from(canvas),
                gap: specified_gap(params.gap_ms).unwrap_or(old_gap),
            },
        ))
    }

    /// Reserve metadata before the image store commits cache evictions.
    pub fn reserve_frame(&mut self) -> Result<(), AnimationError> {
        self.frames
            .try_reserve_exact(1)
            .map_err(|_| AnimationError::AllocationFailed)
    }

    pub fn commit_upload(&mut self, index: usize, frame: AnimationFrame, now: Instant) {
        self.advance(now);
        if index == self.frames.len() {
            self.frames.push(frame);
            if self.waiting_at_end {
                self.current = index;
                self.frame_started = now;
                self.waiting_at_end = false;
            }
        } else {
            let changed_gap = self.frames[index].gap != frame.gap;
            self.frames[index] = frame;
            if changed_gap && index == self.current {
                self.frame_started = now;
                self.waiting_at_end = false;
            }
        }
        self.advance(now);
    }

    pub fn control(
        &mut self,
        params: &KittyAnimationControl,
        now: Instant,
    ) -> Result<(), AnimationError> {
        // Resolve every requested frame before changing the clock or any data.
        let current = params
            .current_frame
            .map(|frame| self.index(frame.get()))
            .transpose()?;
        let gap_index = params
            .gap_frame
            .map(|frame| self.index(frame.get()))
            .transpose()?;
        let gap = specified_gap(params.gap_ms);
        self.advance(now);
        if let Some(gap) = gap {
            let index = gap_index.unwrap_or(0);
            self.frames[index].gap = gap;
            if index == self.current {
                self.frame_started = now;
                self.waiting_at_end = false;
            }
        }
        if let Some(current) = current {
            self.current = current;
            self.displayed = current;
            self.frame_started = now;
            self.waiting_at_end = false;
        }
        if let Some(loops) = params.loops {
            self.loops = loops;
        }
        if let Some(state) = params.state {
            if state == KittyAnimationState::Stopped {
                self.completed_loops = 0;
            }
            if self.state != state {
                self.frame_started = now;
                self.waiting_at_end = false;
            }
            self.state = state;
        }
        self.advance(now);
        Ok(())
    }

    pub fn prepare_composition(
        &self,
        params: &KittyFrameComposition,
        canvas_width: u32,
        canvas_height: u32,
    ) -> Result<(usize, Arc<[u8]>), AnimationError> {
        let source = self.index(params.source_frame)?;
        let destination = self.index(params.destination_frame)?;
        let width = params.width.map_or(canvas_width, |width| width.get());
        let height = params.height.map_or(canvas_height, |height| height.get());
        validate_rectangle(
            params.source_x,
            params.source_y,
            width,
            height,
            canvas_width,
            canvas_height,
        )?;
        validate_rectangle(
            params.destination_x,
            params.destination_y,
            width,
            height,
            canvas_width,
            canvas_height,
        )?;
        if source == destination
            && params.source_x < params.destination_x + width
            && params.destination_x < params.source_x + width
            && params.source_y < params.destination_y + height
            && params.destination_y < params.source_y + height
        {
            return Err(AnimationError::OverlappingComposition);
        }
        let mut canvas = copy_pixels(&self.frames[destination].pixels)?;
        compose_pixels(
            &self.frames[source].pixels,
            canvas_width,
            params.source_x,
            params.source_y,
            &mut canvas,
            canvas_width,
            params.destination_x,
            params.destination_y,
            width,
            height,
            params.blend,
        );
        Ok((destination, Arc::from(canvas)))
    }

    pub fn commit_composition(&mut self, index: usize, pixels: Arc<[u8]>, now: Instant) {
        self.advance(now);
        self.frames[index].pixels = pixels;
    }

    pub fn delete_frame(&mut self, frame: u32, now: Instant) {
        if self.frames.len() == 1 {
            return;
        }
        self.advance(now);
        let index = frame.saturating_sub(1) as usize;
        let index = index.min(self.frames.len() - 1);
        self.frames.remove(index);
        // Deleting frames must release metadata too: many animations shrunk
        // back to one frame must not retain their historical peak capacities.
        self.frames.shrink_to_fit();
        let last = self.frames.len() - 1;
        if index < self.current {
            self.current -= 1;
        } else if index == self.current {
            self.current = self.current.min(last);
            self.frame_started = now;
            self.waiting_at_end = false;
        }
        if index < self.displayed {
            self.displayed -= 1;
        } else if index == self.displayed {
            self.displayed = self.current;
        }
        self.advance(now);
    }

    /// Advance using at most two walks over the bounded frame vector. Whole
    /// elapsed cycles are skipped arithmetically, including long suspensions.
    pub fn advance(&mut self, now: Instant) -> Option<Instant> {
        if self.state == KittyAnimationState::Stopped || self.waiting_at_end {
            return None;
        }
        let mut elapsed = now.saturating_duration_since(self.frame_started);
        if self.state == KittyAnimationState::Running {
            let cycle: Duration = self.frames.iter().map(|frame| frame.gap).sum();
            if cycle.is_zero() {
                // An all-gapless stream must not spin or keep waking the UI.
                self.frame_started = now;
                return None;
            }
            let remaining: Duration = self.frames[self.current..]
                .iter()
                .map(|frame| frame.gap)
                .sum();
            if elapsed >= remaining {
                self.display_last_timed_frame(self.current);
                if self.finish_loop() {
                    return None;
                }
                self.frame_started += remaining;
                elapsed -= remaining;
                self.current = 0;
                let whole_cycles = elapsed.as_nanos() / cycle.as_nanos();
                if whole_cycles > 0 {
                    if let KittyLoopCount::Finite(limit) = self.loops {
                        if whole_cycles
                            >= u128::from(
                                u64::from(limit.get()).saturating_sub(self.completed_loops),
                            )
                        {
                            self.display_last_timed_frame(0);
                            self.stop_at_end();
                            return None;
                        }
                    }
                    self.completed_loops = self
                        .completed_loops
                        .saturating_add(whole_cycles.min(u128::from(u64::MAX)) as u64);
                    // Subtract the residual duration to avoid a potentially
                    // overflowing Duration multiplication by elapsed cycles.
                    let remainder_ns = elapsed.as_nanos() % cycle.as_nanos();
                    let remainder = duration_from_nanos(remainder_ns);
                    self.frame_started += elapsed - remainder;
                    elapsed = remainder;
                }
            }
        }
        for index in self.current..self.frames.len() {
            let gap = self.frames[index].gap;
            self.current = index;
            if !gap.is_zero() {
                self.displayed = index;
                if elapsed < gap {
                    // A lone timed frame has no visible changes, even when
                    // accompanied by gapless helper/background frames.
                    // Preserve its timeline for future frame uploads, but do
                    // not request pointless redraws until one arrives.
                    return if self.state == KittyAnimationState::Running
                        && self
                            .frames
                            .iter()
                            .filter(|frame| !frame.gap.is_zero())
                            .take(2)
                            .count()
                            == 1
                    {
                        None
                    } else {
                        self.frame_started.checked_add(gap)
                    };
                }
            }
            elapsed -= gap;
            self.frame_started += gap;
        }
        // Loading animations hold the last displayable frame until data arrives.
        self.waiting_at_end = true;
        None
    }

    fn display_last_timed_frame(&mut self, start: usize) {
        if let Some(index) = (start..self.frames.len())
            .rev()
            .find(|&index| !self.frames[index].gap.is_zero())
        {
            self.displayed = index;
        }
    }

    fn finish_loop(&mut self) -> bool {
        self.completed_loops = self.completed_loops.saturating_add(1);
        if let KittyLoopCount::Finite(limit) = self.loops {
            if self.completed_loops >= u64::from(limit.get()) {
                self.stop_at_end();
                return true;
            }
        }
        false
    }

    fn stop_at_end(&mut self) {
        self.current = self.displayed;
        self.state = KittyAnimationState::Stopped;
        self.completed_loops = 0;
        self.waiting_at_end = false;
    }
}

fn duration_from_nanos(nanos: u128) -> Duration {
    Duration::new(
        (nanos / 1_000_000_000) as u64,
        (nanos % 1_000_000_000) as u32,
    )
}

fn specified_gap(gap: Option<i32>) -> Option<Duration> {
    gap.filter(|gap| *gap != 0)
        .map(|gap| Duration::from_millis(gap.max(0) as u64))
}

fn copy_pixels(pixels: &[u8]) -> Result<Vec<u8>, AnimationError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(pixels.len())
        .map_err(|_| AnimationError::AllocationFailed)?;
    copy.extend_from_slice(pixels);
    Ok(copy)
}

fn solid_canvas(len: usize, rgba: u32) -> Result<Vec<u8>, AnimationError> {
    let mut canvas = Vec::new();
    canvas
        .try_reserve_exact(len)
        .map_err(|_| AnimationError::AllocationFailed)?;
    canvas.resize(len, 0);
    if rgba != 0 {
        for pixel in canvas.chunks_exact_mut(4) {
            pixel.copy_from_slice(&rgba.to_be_bytes());
        }
    }
    Ok(canvas)
}

fn validate_rectangle(
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    canvas_width: u32,
    canvas_height: u32,
) -> Result<(), AnimationError> {
    if width == 0
        || height == 0
        || x.checked_add(width).is_none_or(|edge| edge > canvas_width)
        || y.checked_add(height)
            .is_none_or(|edge| edge > canvas_height)
    {
        return Err(AnimationError::InvalidRectangle);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compose_pixels(
    source: &[u8],
    source_stride: u32,
    source_x: u32,
    source_y: u32,
    destination: &mut [u8],
    destination_stride: u32,
    destination_x: u32,
    destination_y: u32,
    width: u32,
    height: u32,
    blend: KittyBlendMode,
) {
    let row_bytes = width as usize * 4;
    for row in 0..height {
        let source_start =
            ((source_y + row) as usize * source_stride as usize + source_x as usize) * 4;
        let destination_start = ((destination_y + row) as usize * destination_stride as usize
            + destination_x as usize)
            * 4;
        let source = &source[source_start..source_start + row_bytes];
        let destination = &mut destination[destination_start..destination_start + row_bytes];
        if blend == KittyBlendMode::Overwrite {
            destination.copy_from_slice(source);
        } else {
            for (source, destination) in source.chunks_exact(4).zip(destination.chunks_exact_mut(4))
            {
                alpha_blend(source, destination);
            }
        }
    }
}

fn alpha_blend(source: &[u8], destination: &mut [u8]) {
    let source_alpha = u32::from(source[3]);
    if source_alpha == 0 {
        return;
    }
    if source_alpha == 255 {
        destination.copy_from_slice(source);
        return;
    }
    let destination_alpha = u32::from(destination[3]);
    let output_alpha = source_alpha * 255 + destination_alpha * (255 - source_alpha);
    for channel in 0..3 {
        let value = u32::from(source[channel]) * source_alpha * 255
            + u32::from(destination[channel]) * destination_alpha * (255 - source_alpha);
        destination[channel] = ((value + output_alpha / 2) / output_alpha) as u8;
    }
    destination[3] = ((output_alpha + 127) / 255) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;

    fn numbered(value: u32) -> Option<NonZeroU32> {
        NonZeroU32::new(value)
    }

    fn timeline(now: Instant, gaps: &[u64]) -> Animation {
        let mut animation = Animation::new(Arc::from([0, 0, 0, 255]), now);
        animation.frames = gaps
            .iter()
            .enumerate()
            .map(|(index, gap)| AnimationFrame {
                pixels: Arc::from([index as u8, 0, 0, 255]),
                gap: Duration::from_millis(*gap),
            })
            .collect();
        animation
    }

    fn control(state: KittyAnimationState) -> KittyAnimationControl {
        KittyAnimationControl {
            state: Some(state),
            ..Default::default()
        }
    }

    fn append(animation: &mut Animation, value: u8, gap: i32, now: Instant) {
        let params = KittyFrameUpload {
            gap_ms: Some(gap),
            ..Default::default()
        };
        let (index, frame) = animation
            .prepare_upload(&[value, 0, 0, 255], 1, 1, 1, 1, &params)
            .unwrap();
        animation.commit_upload(index, frame, now);
    }

    #[test]
    fn root_is_gapless_new_frames_default_to_40ms_and_zero_preserves_gap() {
        let now = Instant::now();
        let mut animation = Animation::new(Arc::from([1, 2, 3, 255]), now);
        assert_eq!(animation.frames[0].gap, Duration::ZERO);
        append(&mut animation, 4, 0, now);
        assert_eq!(animation.frames[1].gap, Duration::from_millis(40));
        let params = KittyFrameUpload {
            edit_frame: numbered(2),
            gap_ms: Some(0),
            ..Default::default()
        };
        let (index, frame) = animation
            .prepare_upload(&[5, 0, 0, 255], 1, 1, 1, 1, &params)
            .unwrap();
        animation.commit_upload(index, frame, now);
        assert_eq!(animation.frames[1].gap, Duration::from_millis(40));
        let params = KittyAnimationControl {
            gap_frame: numbered(2),
            gap_ms: Some(-1),
            ..Default::default()
        };
        animation.control(&params, now).unwrap();
        assert_eq!(animation.frames[1].gap, Duration::ZERO);
    }

    #[test]
    fn loading_skips_gapless_frames_and_waits_until_a_new_frame_arrives() {
        let now = Instant::now();
        let mut animation = timeline(now, &[0, 40, 0, 20, 0]);
        animation
            .control(&control(KittyAnimationState::Loading), now)
            .unwrap();
        assert_eq!(animation.displayed, 1);
        assert_eq!(
            animation.advance(now),
            Some(now + Duration::from_millis(40))
        );
        assert_eq!(
            animation.advance(now + Duration::from_millis(40)),
            Some(now + Duration::from_millis(60))
        );
        assert_eq!(animation.displayed, 3);
        assert_eq!(animation.advance(now + Duration::from_secs(500)), None);
        assert!(animation.waiting_at_end);
        assert_eq!(animation.displayed, 3);
        append(&mut animation, 99, 10, now + Duration::from_secs(500));
        assert_eq!(animation.displayed, 5);
        assert_eq!(animation.active_pixels().as_ref(), &[99, 0, 0, 255]);
        assert_eq!(
            animation.advance(now + Duration::from_secs(500)),
            Some(now + Duration::from_millis(500_010))
        );
        assert_eq!(
            animation.advance(now + Duration::from_millis(500_010)),
            None
        );
    }

    #[test]
    fn running_uses_exact_deadlines_and_skips_a_billion_elapsed_cycles() {
        let now = Instant::now();
        let mut animation = timeline(now, &[10, 20, 0, 30]);
        animation
            .control(&control(KittyAnimationState::Running), now)
            .unwrap();
        for (elapsed, displayed, deadline) in [
            (0, 0, 10),
            (10, 1, 30),
            (30, 3, 60),
            (60, 0, 70),
            (60_000_000_031, 3, 60_000_000_060),
        ] {
            assert_eq!(
                animation.advance(now + Duration::from_millis(elapsed)),
                Some(now + Duration::from_millis(deadline))
            );
            assert_eq!(animation.displayed, displayed);
        }
    }

    #[test]
    fn finite_loops_stop_on_the_last_displayable_frame_after_suspension() {
        let now = Instant::now();
        let mut animation = timeline(now, &[10, 20, 0]);
        let params = KittyAnimationControl {
            state: Some(KittyAnimationState::Running),
            loops: Some(KittyLoopCount::Finite(NonZeroU32::new(3).unwrap())),
            ..Default::default()
        };
        animation.control(&params, now).unwrap();
        assert_eq!(
            animation.advance(now + Duration::from_millis(70)),
            Some(now + Duration::from_millis(90))
        );
        assert_eq!(animation.displayed, 1);
        assert_eq!(
            animation.advance(now + Duration::from_secs(1_000_000)),
            None
        );
        assert_eq!(animation.displayed, 1);
        assert_eq!(animation.state, KittyAnimationState::Stopped);
        assert_eq!(animation.completed_loops, 0);
    }

    #[test]
    fn whole_cycle_arithmetic_matches_frame_interval_oracle() {
        let now = Instant::now();
        for gaps in [&[0, 1, 0, 3, 2, 0][..], &[2, 0, 0, 1][..], &[0, 0, 5][..]] {
            let total: u64 = gaps.iter().sum();
            for elapsed in [0, 1, 2, 4, 5, 9, 13, 28, 102, 7_999_999] {
                let mut animation = timeline(now, gaps);
                animation
                    .control(&control(KittyAnimationState::Running), now)
                    .unwrap();
                let mut offset = elapsed % total;
                let expected = gaps
                    .iter()
                    .enumerate()
                    .find_map(|(index, gap)| {
                        if offset < *gap {
                            Some(index)
                        } else {
                            offset -= gap;
                            None
                        }
                    })
                    .unwrap();
                let next = elapsed + gaps[expected] - offset;
                let deadline = if gaps.iter().filter(|gap| **gap > 0).count() == 1 {
                    None
                } else {
                    Some(now + Duration::from_millis(next))
                };
                assert_eq!(
                    animation.advance(now + Duration::from_millis(elapsed)),
                    deadline
                );
                assert_eq!(animation.displayed, expected);
            }
        }
    }

    #[test]
    fn all_gapless_frames_have_no_timer_and_resume_when_a_timed_frame_is_added() {
        let now = Instant::now();
        let mut animation = timeline(now, &[0, 0, 0]);
        animation
            .control(&control(KittyAnimationState::Running), now)
            .unwrap();
        assert_eq!(
            animation.advance(now + Duration::from_secs(1_000_000)),
            None
        );
        assert_eq!(animation.displayed, 0);
        append(&mut animation, 33, 40, now + Duration::from_secs(1_000_000));
        assert_eq!(animation.displayed, 3);
        assert_eq!(
            animation.advance(now + Duration::from_secs(1_000_000)),
            None
        );
    }

    #[test]
    fn stop_resets_loop_counter_and_selecting_a_frame_works_when_stopped() {
        let now = Instant::now();
        let mut animation = timeline(now, &[10, 20]);
        animation
            .control(&control(KittyAnimationState::Running), now)
            .unwrap();
        animation.advance(now + Duration::from_millis(35));
        assert_eq!(animation.completed_loops, 1);
        animation
            .control(
                &KittyAnimationControl {
                    state: Some(KittyAnimationState::Stopped),
                    current_frame: numbered(2),
                    ..Default::default()
                },
                now + Duration::from_millis(35),
            )
            .unwrap();
        assert_eq!(animation.completed_loops, 0);
        assert_eq!(animation.displayed, 1);
        assert_eq!(animation.advance(now + Duration::from_secs(1000)), None);
    }

    #[test]
    fn rejected_control_preserves_clock_frame_and_pixels() {
        let now = Instant::now();
        let mut animation = timeline(now, &[10, 20]);
        animation
            .control(&control(KittyAnimationState::Running), now)
            .unwrap();
        let pixels = Arc::clone(animation.active_pixels());
        let invalid = KittyAnimationControl {
            current_frame: numbered(3),
            gap_ms: Some(999),
            ..Default::default()
        };
        assert_eq!(
            animation.control(&invalid, now + Duration::from_secs(100)),
            Err(AnimationError::FrameNotFound)
        );
        assert_eq!(animation.current, 0);
        assert_eq!(animation.frame_started, now);
        assert_eq!(animation.frames[0].gap, Duration::from_millis(10));
        assert!(Arc::ptr_eq(animation.active_pixels(), &pixels));
    }

    #[test]
    fn alpha_blend_uses_straight_rgba_and_preserves_transparency() {
        let mut pixel = [0, 0, 255, 128];
        alpha_blend(&[255, 0, 0, 128], &mut pixel);
        assert_eq!(pixel, [170, 0, 85, 192]);
        alpha_blend(&[20, 30, 40, 0], &mut pixel);
        assert_eq!(pixel, [170, 0, 85, 192]);
        alpha_blend(&[4, 5, 6, 255], &mut pixel);
        assert_eq!(pixel, [4, 5, 6, 255]);
        let mut transparent = [99, 88, 77, 0];
        alpha_blend(&[10, 20, 30, 40], &mut transparent);
        assert_eq!(transparent, [10, 20, 30, 40]);
    }

    #[test]
    fn delta_frames_use_rgba_background_or_base_and_edits_preserve_other_pixels() {
        let now = Instant::now();
        let root: Arc<[u8]> = Arc::from([1, 2, 3, 255, 4, 5, 6, 255]);
        let mut animation = Animation::new(Arc::clone(&root), now);
        let params = KittyFrameUpload {
            x: 1,
            background_rgba: 0xaabbccdd,
            blend: KittyBlendMode::Overwrite,
            ..Default::default()
        };
        let (index, frame) = animation
            .prepare_upload(&[10, 20, 30, 0], 1, 1, 2, 1, &params)
            .unwrap();
        animation.commit_upload(index, frame, now);
        assert_eq!(
            animation.frames[1].pixels.as_ref(),
            &[0xaa, 0xbb, 0xcc, 0xdd, 10, 20, 30, 0]
        );
        let params = KittyFrameUpload {
            x: 1,
            base_frame: numbered(1),
            ..Default::default()
        };
        let (index, frame) = animation
            .prepare_upload(&[10, 20, 30, 0], 1, 1, 2, 1, &params)
            .unwrap();
        animation.commit_upload(index, frame, now);
        assert_eq!(animation.frames[2].pixels.as_ref(), root.as_ref());
        let params = KittyFrameUpload {
            edit_frame: numbered(2),
            base_frame: numbered(999),
            blend: KittyBlendMode::Overwrite,
            ..Default::default()
        };
        let (index, frame) = animation
            .prepare_upload(&[50, 60, 70, 80], 1, 1, 2, 1, &params)
            .unwrap();
        animation.commit_upload(index, frame, now);
        assert_eq!(
            animation.frames[1].pixels.as_ref(),
            &[50, 60, 70, 80, 10, 20, 30, 0]
        );
        assert!(Arc::ptr_eq(&animation.frames[0].pixels, &root));
    }

    #[test]
    fn composition_uses_source_and_destination_rectangles_and_rejects_overlap() {
        let now = Instant::now();
        let root: Arc<[u8]> = Arc::from([1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255]);
        let mut animation = Animation::new(Arc::clone(&root), now);
        let params = KittyFrameComposition {
            source_frame: 1,
            destination_frame: 1,
            source_x: 0,
            destination_x: 2,
            width: numbered(1),
            height: numbered(1),
            blend: KittyBlendMode::Overwrite,
            ..Default::default()
        };
        let (index, pixels) = animation.prepare_composition(&params, 3, 1).unwrap();
        animation.commit_composition(index, pixels, now);
        assert_eq!(
            animation.active_pixels().as_ref(),
            &[1, 0, 0, 255, 2, 0, 0, 255, 1, 0, 0, 255]
        );
        let bad = KittyFrameComposition {
            destination_x: 0,
            ..params
        };
        assert!(matches!(
            animation.prepare_composition(&bad, 3, 1),
            Err(AnimationError::OverlappingComposition)
        ));
        let bad = KittyFrameComposition {
            destination_x: u32::MAX,
            ..params
        };
        assert!(matches!(
            animation.prepare_composition(&bad, 3, 1),
            Err(AnimationError::InvalidRectangle)
        ));
        let bad = KittyFrameComposition {
            source_frame: 0,
            ..params
        };
        assert!(matches!(
            animation.prepare_composition(&bad, 3, 1),
            Err(AnimationError::FrameNotFound)
        ));
        assert_eq!(root.as_ref(), &[1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255]);
    }

    #[test]
    fn deleting_root_promotes_next_frame_and_oversized_selectors_delete_last() {
        let now = Instant::now();
        let mut animation = timeline(now, &[0, 10, 20]);
        animation
            .control(
                &KittyAnimationControl {
                    current_frame: numbered(3),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        animation.delete_frame(0, now);
        assert_eq!(animation.frame_count(), 2);
        assert_eq!(animation.frames[0].pixels[0], 1);
        assert_eq!(animation.displayed, 1);
        assert_eq!(animation.active_pixels()[0], 2);
        animation.delete_frame(u32::MAX, now);
        assert_eq!(animation.frame_count(), 1);
        assert_eq!(animation.active_pixels()[0], 1);
        assert_eq!(animation.frames.capacity(), 1);
        animation.delete_frame(1, now);
        assert_eq!(animation.frame_count(), 1);
    }
}
