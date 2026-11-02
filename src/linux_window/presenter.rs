//! Frame presentation for the Linux window. `present_frame` builds a
//! [`SceneFrame`] (grid snapshot, images, IME preedit) and hands it to a
//! [`FramePresenter`], which owns the surface and decides how pixels reach the
//! window: the softbuffer CPU path that shipped before the GPU spike, or the
//! wgpu presenter in `gpu.rs` when the crate is built with `--features gpu`
//! and `[render] backend` selects it.

use super::damage::{frame_damage, scene_for_buffer_age, PresentedScene, MAX_PRESENTED_SCENES};
use super::draw::{draw_grid_snapshot_in_band, draw_ime_preedit, GridSnapshot};
#[cfg(feature = "gpu")]
use super::gpu::{WgpuOptions, WgpuPresenter};
use super::ime::ImePreeditLayout;
use crate::config::RenderBackend;
use crate::glyph_atlas::GlyphAtlas;
use crate::software_graphics::ImageSnapshot;
use crate::terminal_colors::TerminalColors;
use softbuffer::{Context, Surface};
use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::Arc;
use winit::window::Window;

/// Everything one frame needs, computed once by `present_frame` under the
/// grid and graphics locks. The snapshot and images are owned so the software
/// presenter can keep them as damage history without copying.
pub(super) struct SceneFrame<'a> {
    pub(super) snapshot: GridSnapshot,
    pub(super) images: Vec<ImageSnapshot>,
    pub(super) preedit: Option<&'a ImePreeditLayout>,
    pub(super) cell_dimensions: (u16, u16),
    pub(super) colors: &'a TerminalColors,
    /// Default background as `0x00RRGGBB`; the frame is cleared to it.
    pub(super) background: u32,
    pub(super) frame_size: (NonZeroU32, NonZeroU32),
    /// Smoke mode (`KOKUBAN_EXIT_AFTER_FIRST_FRAME`) asks whether any pixel
    /// differs from the background; ordinary frames skip the scan.
    pub(super) detect_visible_content: bool,
}

impl SceneFrame<'_> {
    /// The terminal cursor hides while an IME preedit draws its own caret.
    pub(super) fn draw_cursor(&self) -> bool {
        self.preedit.is_none()
    }

    pub(super) fn has_overlays(&self) -> bool {
        !self.images.is_empty() || self.preedit.is_some()
    }

    pub(super) fn frame_pixels(&self) -> (u32, u32) {
        (self.frame_size.0.get(), self.frame_size.1.get())
    }
}

pub(super) struct PresentReport {
    /// `Some` when the presenter could inspect the presented pixels, `None`
    /// when it cannot know (GPU surface without read-back).
    pub(super) visible_content: Option<bool>,
}

pub(super) trait FramePresenter {
    /// Match the surface to the window before drawing; cheap when unchanged.
    fn resize(&mut self, size: (NonZeroU32, NonZeroU32)) -> Result<(), String>;
    /// Draw and present one frame. The atlas is mutable because glyphs are
    /// rasterized lazily while the scene is walked.
    fn present(
        &mut self,
        scene: SceneFrame<'_>,
        atlas: &mut GlyphAtlas,
    ) -> Result<PresentReport, String>;
    /// Forget any retained frame history: the palette or the atlas changed,
    /// so unchanged cells no longer match what the buffers hold.
    fn invalidate(&mut self);
    fn supports_partial_damage(&self) -> bool;
    fn backend_name(&self) -> &'static str;
}

type SoftwareSurface = Surface<Arc<Window>, Arc<Window>>;

/// The CPU path: softbuffer surface, buffer-age damage bands and `draw.rs`.
/// Output is byte-identical to the pre-spike `present_frame`.
pub(super) struct SoftwarePresenter {
    // Drop the surface and its display connection before releasing the window.
    surface: SoftwareSurface,
    _context: Context<Arc<Window>>,
    window: Arc<Window>,
    presented_scenes: VecDeque<PresentedScene>,
    frames: u64,
}

impl SoftwarePresenter {
    pub(super) fn new(window: Arc<Window>) -> Result<Self, String> {
        let context = Context::new(window.clone())
            .map_err(|error| format!("could not initialize the software renderer: {error}"))?;
        let surface = Surface::new(&context, window.clone())
            .map_err(|error| format!("could not create the software-rendering surface: {error}"))?;
        Ok(Self {
            surface,
            _context: context,
            window,
            presented_scenes: VecDeque::new(),
            frames: 0,
        })
    }
}

impl Drop for SoftwarePresenter {
    fn drop(&mut self) {
        log_presented_frames("software", self.frames);
    }
}

/// `scripts/compare-render-backends.py` reads this line to normalize CPU time
/// per presented frame.
pub(super) fn log_presented_frames(backend: &str, frames: u64) {
    log::info!("Linux renderer {backend}: presented {frames} frames");
}

impl FramePresenter for SoftwarePresenter {
    fn resize(&mut self, (width, height): (NonZeroU32, NonZeroU32)) -> Result<(), String> {
        self.surface
            .resize(width, height)
            .map_err(|error| format!("could not resize the software-rendering surface: {error}"))
    }

    fn present(
        &mut self,
        scene: SceneFrame<'_>,
        atlas: &mut GlyphAtlas,
    ) -> Result<PresentReport, String> {
        let frame_size = scene.frame_pixels();
        let width = frame_size.0;
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|error| format!("could not acquire the software-rendering buffer: {error}"))?;
        let has_overlays = scene.has_overlays();
        let damage = frame_damage(
            scene_for_buffer_age(&self.presented_scenes, buffer.age()),
            &scene.snapshot,
            frame_size,
            scene.cell_dimensions,
            has_overlays,
            atlas,
        );
        if !damage.is_empty() {
            let start = damage.start as usize * width as usize;
            let end = damage.end as usize * width as usize;
            buffer[start..end].fill(scene.background);
            draw_grid_snapshot_in_band(
                &mut buffer,
                frame_size,
                atlas,
                scene.colors,
                scene.cell_dimensions,
                &scene.snapshot,
                scene.draw_cursor(),
                &scene.images,
                damage,
            );
        }
        if let Some(layout) = scene.preedit {
            draw_ime_preedit(
                &mut buffer,
                frame_size,
                atlas,
                scene.colors,
                scene.cell_dimensions,
                &scene.snapshot,
                layout,
            );
        }
        let visible_content = scene
            .detect_visible_content
            .then(|| buffer.iter().any(|&pixel| pixel != scene.background));
        self.window.pre_present_notify();
        buffer
            .present()
            .map_err(|error| format!("could not present a Linux frame: {error}"))?;
        self.presented_scenes.push_front(PresentedScene {
            snapshot: scene.snapshot,
            frame_size,
            cell_dimensions: scene.cell_dimensions,
            has_overlays,
        });
        self.presented_scenes.truncate(MAX_PRESENTED_SCENES);
        self.frames += 1;
        Ok(PresentReport { visible_content })
    }

    fn invalidate(&mut self) {
        self.presented_scenes.clear();
    }

    fn supports_partial_damage(&self) -> bool {
        true
    }

    fn backend_name(&self) -> &'static str {
        "software"
    }
}

/// Pick the presenter for `[render] backend` / `KOKUBAN_RENDER`.
///
/// `auto` only keeps wgpu when it found a real GPU; CPU rasterizers (llvmpipe,
/// lavapipe, SwiftShader) would be slower than the tuned software path and are
/// what CI offers, so they fall back. `gpu` keeps whatever adapter exists,
/// which is how the manual GPU workflow exercises the pipeline on Mesa.
/// Without the `gpu` Cargo feature, `gpu` fails fast and `auto` is software.
pub(super) fn create_presenter(
    backend: RenderBackend,
    window: Arc<Window>,
    readback_first_frame: bool,
) -> Result<Box<dyn FramePresenter>, String> {
    #[cfg(not(feature = "gpu"))]
    let _ = readback_first_frame;
    let (presenter, reason): (Box<dyn FramePresenter>, String) = match backend {
        RenderBackend::Software => (
            Box::new(SoftwarePresenter::new(window)?),
            "configured".to_string(),
        ),
        #[cfg(not(feature = "gpu"))]
        RenderBackend::Gpu => {
            return Err(
                "kokuban was built without the gpu feature; rebuild with --features gpu to use \
                 [render] backend = \"gpu\""
                    .to_string(),
            );
        }
        #[cfg(not(feature = "gpu"))]
        RenderBackend::Auto => (
            Box::new(SoftwarePresenter::new(window)?),
            "built without the gpu feature".to_string(),
        ),
        #[cfg(feature = "gpu")]
        RenderBackend::Gpu => {
            let presenter = WgpuPresenter::new(
                window,
                WgpuOptions {
                    allow_cpu_adapter: true,
                    readback_first_frame,
                },
            )
            .map_err(|error| {
                format!(
                    "could not initialize the GPU renderer requested by [render] backend = \
                     \"gpu\": {error}"
                )
            })?;
            let description = presenter.describe();
            (Box::new(presenter), description)
        }
        #[cfg(feature = "gpu")]
        RenderBackend::Auto => match WgpuPresenter::new(
            window.clone(),
            WgpuOptions {
                allow_cpu_adapter: false,
                readback_first_frame,
            },
        ) {
            Ok(presenter) => {
                let description = presenter.describe();
                (Box::new(presenter), description)
            }
            Err(error) => (
                Box::new(SoftwarePresenter::new(window)?),
                format!("fallback: {error}"),
            ),
        },
    };
    log::info!(
        "Linux renderer: {} ([render] backend = {}, partial damage {}; {reason})",
        presenter.backend_name(),
        backend.as_str(),
        if presenter.supports_partial_damage() {
            "on"
        } else {
            "off"
        }
    );
    Ok(presenter)
}
