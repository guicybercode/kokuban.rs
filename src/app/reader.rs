//! macOS side of the shared PTY reader: frames go through the render
//! scheduler, attention and the Dock bounce are applied on the main thread,
//! OSC 52 text waits for NSPasteboard there too.

use super::render_scheduler::RenderScheduler;
use super::snapshot_then_lock;
use crate::glyph_atlas::GlyphAtlas;
use crate::layout::PaneId;
use crate::pane::cleanup::PaneCleanup;
use crate::pane::pane::Pane;
use crate::pane::PaneTree;
pub(super) use crate::pane_reader::{run_reader, PaneReaderHost, ReaderShared, ReaderWake};
use std::sync::{Arc, Mutex, MutexGuard};

pub(super) struct MacReaderHost {
    pub(super) atlas: Arc<Mutex<GlyphAtlas>>,
    pub(super) render_scheduler: Arc<RenderScheduler>,
    pub(super) pane_cleanup: PaneCleanup,
    /// Text waiting to reach NSPasteboard from the main thread.
    pub(super) clipboard_write: Arc<Mutex<Option<String>>>,
}

impl PaneReaderHost for MacReaderHost {
    fn cell_size_then_lock<'a>(
        &self,
        tree: &'a Mutex<PaneTree>,
    ) -> ((f32, f32), MutexGuard<'a, PaneTree>) {
        // Lock atlas FIRST (canonical order: atlas → tree → image_store).
        // Keep it locked until the tree snapshot belongs to the same metric epoch.
        snapshot_then_lock(self.atlas.as_ref(), tree, |atlas| {
            (atlas.cell_width, atlas.cell_height)
        })
    }

    fn request_frame(&self) {
        self.render_scheduler.request_render();
    }

    // The frame callback observes `should_close` and the relaid-out tree.
    fn pane_closed(&self, _id: PaneId) {}

    // The frame callback applies pending mailbox titles.
    fn title_changed(&self) {}

    // `apply_pending_user_attention` runs in the frame callback.
    fn user_attention(&self) {}

    fn clipboard_write(&self, text: String) {
        // NSPasteboard is touched from the main thread frame callback.
        if let Ok(mut pending) = self.clipboard_write.lock() {
            *pending = Some(text);
        }
    }

    fn pane_retired(&self, pane: Pane) {
        self.pane_cleanup.retire(pane);
    }
}
