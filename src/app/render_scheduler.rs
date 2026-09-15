//! macOS frame scheduling: frames run on the main dispatch queue only when
//! requested, at most once per frame interval, instead of a repeating timer
//! waking the process 60 times per second while idle.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Upper bound on frame rate, matching the previous 60 Hz timer.
const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

#[repr(C)]
struct DispatchQueue {
    _private: [u8; 0],
}

extern "C" {
    // `dispatch_get_main_queue()` is a header macro for this libSystem symbol.
    static _dispatch_main_q: DispatchQueue;
    fn dispatch_async_f(
        queue: *const DispatchQueue,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
    fn dispatch_after_f(
        when: u64,
        queue: *const DispatchQueue,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
    fn dispatch_time(when: u64, delta: i64) -> u64;
}

const DISPATCH_TIME_NOW: u64 = 0;

pub(super) struct RenderScheduler {
    dirty: Arc<AtomicBool>,
    scheduled: AtomicBool,
    last_frame: Mutex<Option<Instant>>,
    frame: Box<dyn Fn() + Send + Sync>,
}

impl RenderScheduler {
    /// `frame` runs on the main thread; it renders when `dirty` is set.
    pub(super) fn new(dirty: Arc<AtomicBool>, frame: Box<dyn Fn() + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            dirty,
            scheduled: AtomicBool::new(false),
            last_frame: Mutex::new(None),
            frame,
        })
    }

    /// Mark content changed and schedule one frame; safe from any thread.
    pub(super) fn request_render(self: &Arc<Self>) {
        self.dirty.store(true, Ordering::Relaxed);
        self.request_frame();
    }

    /// Schedule a frame without marking content changed, for window-title,
    /// shutdown and other main-thread work that the frame callback performs.
    pub(super) fn request_frame(self: &Arc<Self>) {
        if !self.try_claim_schedule() {
            return;
        }
        let context = Arc::into_raw(Arc::clone(self)) as *mut c_void;
        // SAFETY: the main queue lives for the process; `run_scheduled_frame`
        // takes back exactly the reference leaked into `context`.
        unsafe { dispatch_async_f(&_dispatch_main_q, context, run_scheduled_frame) };
    }

    /// Returns true when the caller must dispatch; concurrent requests coalesce.
    fn try_claim_schedule(&self) -> bool {
        self.scheduled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn run_on_main_thread(self: Arc<Self>) {
        let now = Instant::now();
        let delay = {
            let last_frame = self.last_frame.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            frame_delay(*last_frame, now)
        };
        if !delay.is_zero() {
            let delta = i64::try_from(delay.as_nanos()).unwrap_or(i64::MAX);
            let context = Arc::into_raw(self) as *mut c_void;
            // SAFETY: as in `request_frame`; the schedule stays claimed until it runs.
            unsafe {
                dispatch_after_f(
                    dispatch_time(DISPATCH_TIME_NOW, delta),
                    &_dispatch_main_q,
                    context,
                    run_scheduled_frame,
                );
            }
            return;
        }

        *self.last_frame.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(now);
        // Release before the frame so requests made while rendering schedule again.
        self.scheduled.store(false, Ordering::Release);
        (self.frame)();
        // Animations and unavailable drawables leave content dirty: retry next interval.
        if self.dirty.load(Ordering::Relaxed) {
            self.request_frame();
        }
    }
}

extern "C" fn run_scheduled_frame(context: *mut c_void) {
    // SAFETY: `context` came from `Arc::into_raw` in `request_frame` or
    // `run_on_main_thread` and is consumed exactly once here.
    let scheduler = unsafe { Arc::from_raw(context as *const RenderScheduler) };
    scheduler.run_on_main_thread();
}

/// Time to wait so frames start at least one interval apart.
fn frame_delay(last_frame: Option<Instant>, now: Instant) -> Duration {
    last_frame.map_or(Duration::ZERO, |last| {
        FRAME_INTERVAL.saturating_sub(now.saturating_duration_since(last))
    })
}

#[cfg(test)]
mod tests {
    use super::{frame_delay, RenderScheduler, FRAME_INTERVAL};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn frames_start_at_least_one_interval_apart() {
        let now = Instant::now();
        assert_eq!(frame_delay(None, now), Duration::ZERO);
        assert_eq!(frame_delay(Some(now), now), FRAME_INTERVAL);
        assert_eq!(
            frame_delay(Some(now), now + Duration::from_millis(10)),
            FRAME_INTERVAL - Duration::from_millis(10)
        );
        assert_eq!(frame_delay(Some(now), now + FRAME_INTERVAL), Duration::ZERO);
        assert_eq!(frame_delay(Some(now), now + Duration::from_secs(1)), Duration::ZERO);
    }

    #[test]
    fn concurrent_requests_claim_one_schedule_until_released() {
        let scheduler = RenderScheduler::new(Arc::new(AtomicBool::new(false)), Box::new(|| {}));
        let claims: usize = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| (0..1000).filter(|_| scheduler.try_claim_schedule()).count()))
                .collect();
            handles.into_iter().map(|handle| handle.join().unwrap()).sum()
        });
        assert_eq!(claims, 1);

        scheduler.scheduled.store(false, Ordering::Release);
        assert!(scheduler.try_claim_schedule());
        assert!(!scheduler.try_claim_schedule());
    }
}
