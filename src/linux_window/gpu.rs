//! GPU presenter spike: draws the same scene as `draw.rs` with wgpu (Vulkan,
//! then OpenGL ES) instead of the CPU rasterizer. Every frame is a full
//! redraw of instanced quads built on the CPU from the grid snapshot; the
//! glyph atlas is one RGBA texture re-uploaded when it changes and images are
//! textures cached per decoded image. Selected through `[render] backend`;
//! see `docs/GPU_SPIKE.md`.

use super::draw::{
    builtin_cell_rects, cell_content_is_visible, cell_origin, contrasting_cursor_color,
    cursor_rectangle, ime_preedit_background_at, ime_preedit_glyph_colors, resolve_cell_colors,
    resolve_cell_underline_color, rgb_to_xrgb, rounded_f64_i32, underline_anchor_y, CURSOR_ALPHA,
    CURSOR_THICKNESS,
};
use super::ime::ImePreeditLayout;
use super::presenter::{log_presented_frames, FramePresenter, PresentReport, SceneFrame};
use crate::glyph_atlas::{GlyphAtlas, GlyphEntry, GlyphKey};
use crate::grid::cell::{CellFlags, Color, UnderlineStyle};
use crate::grid::CursorShape;
use crate::renderer::box_drawing::CellRect;
use crate::software_graphics::ImageSnapshot;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::ops::Range;
use std::sync::mpsc;
use std::sync::Arc;
use winit::window::Window;

const SHADER: &str = include_str!("gpu.wgsl");
/// Bytes per instance: three `vec4<f32>` plus a `u32` padded to 16 bytes.
const INSTANCE_STRIDE: u64 = 64;
const INITIAL_INSTANCE_CAPACITY: u64 = 4096;
/// Decoded images kept as textures between frames (kitty animations churn
/// through frames, so the cap bounds GPU memory rather than the image count).
const MAX_CACHED_IMAGES: usize = 64;
/// Adapter names of CPU rasterizers that `auto` must not prefer over the
/// tuned software path (Mesa llvmpipe/lavapipe/softpipe, Google SwiftShader).
const CPU_RASTERIZER_NAMES: [&str; 4] = ["llvmpipe", "lavapipe", "softpipe", "swiftshader"];

#[derive(Clone, Copy)]
pub(super) struct WgpuOptions {
    /// `gpu` keeps any adapter, `auto` rejects CPU rasterizers.
    pub(super) allow_cpu_adapter: bool,
    /// Smoke mode: copy the first frame back to verify visible content.
    pub(super) readback_first_frame: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum Fill {
    Solid = 0,
    MaskGlyph = 1,
    ColorGlyph = 2,
    Image = 3,
}

type Rect = ((i32, i32), (u32, u32));

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct ImageKey {
    image_id: u64,
    /// The pixel buffer's address distinguishes animation frames that share
    /// an image id; the id keeps a recycled address from matching another
    /// image. A false hit would show a stale frame, never a crash.
    pixels: usize,
    size: (u32, u32),
}

impl ImageKey {
    fn of(image: &ImageSnapshot) -> Self {
        Self {
            image_id: image.image_id(),
            pixels: Arc::as_ptr(&image.pixels) as *const u8 as usize,
            size: image.size,
        }
    }
}

enum Batch {
    /// Instances drawn with the atlas bound; images use their own texture.
    Cells(Range<u32>),
    Image {
        instance: u32,
        index: usize,
    },
}

/// CPU side of a frame: instance bytes in draw order and the batches that
/// split them wherever an image texture must be bound.
#[derive(Default)]
struct FrameBuilder {
    bytes: Vec<u8>,
    count: u32,
    batches: Vec<Batch>,
    open_batch_start: u32,
    atlas_size: (f32, f32),
}

impl FrameBuilder {
    fn reset(&mut self, atlas: &GlyphAtlas) {
        self.bytes.clear();
        self.count = 0;
        self.batches.clear();
        self.open_batch_start = 0;
        self.atlas_size = (atlas.width as f32, atlas.height as f32);
    }

    fn push(&mut self, rect: [f32; 4], uv: [f32; 4], color: [f32; 4], fill: Fill) {
        for value in rect.iter().chain(&uv).chain(&color) {
            self.bytes.extend_from_slice(&value.to_ne_bytes());
        }
        self.bytes.extend_from_slice(&(fill as u32).to_ne_bytes());
        self.bytes.extend_from_slice(&[0; 12]);
        self.count += 1;
    }

    fn close_cells(&mut self) {
        if self.open_batch_start < self.count {
            self.batches
                .push(Batch::Cells(self.open_batch_start..self.count));
        }
        self.open_batch_start = self.count;
    }

    fn rect(&mut self, origin: (i32, i32), size: (u32, u32), rgb: u32, alpha: u8) {
        if alpha == 0 || size.0 == 0 || size.1 == 0 {
            return;
        }
        self.push(
            [
                origin.0 as f32,
                origin.1 as f32,
                size.0 as f32,
                size.1 as f32,
            ],
            [0.0; 4],
            rgba(rgb, alpha),
            Fill::Solid,
        );
    }

    /// Mirror of `draw::draw_builtin_rects`, optionally clipped to the cursor.
    fn builtin_rects(
        &mut self,
        rects: &[CellRect],
        origin: (i32, i32),
        rgb: u32,
        clip: Option<Rect>,
    ) {
        for rect in rects {
            let left = i64::from(origin.0) + i64::from(rect.x);
            let top = i64::from(origin.1) + i64::from(rect.y);
            let Some(clipped) = intersect(
                (
                    left,
                    top,
                    left + i64::from(rect.width),
                    top + i64::from(rect.height),
                ),
                clip,
            ) else {
                continue;
            };
            self.rect(clipped.0, clipped.1, rgb, rect.alpha);
        }
    }

    /// Mirror of `draw::draw_cell_glyph` / `draw_glyph_in_cursor`: the glyph
    /// bitmap at its bearings, cropped (bitmap and UVs alike) to `clip`.
    fn glyph(
        &mut self,
        atlas: &GlyphAtlas,
        glyph: GlyphEntry,
        cell_origin: (i32, i32),
        rgb: u32,
        clip: Option<Rect>,
    ) {
        let Some(destination_x) = cell_origin.0.checked_add(glyph.bearing_x) else {
            return;
        };
        let Some(baseline) = rounded_f64_i32(f64::from(cell_origin.1) + f64::from(atlas.ascent))
        else {
            return;
        };
        let Some(destination_y) = baseline.checked_add(glyph.bearing_y) else {
            return;
        };
        let left = i64::from(destination_x);
        let top = i64::from(destination_y);
        let Some(((x, y), (width, height))) = intersect(
            (
                left,
                top,
                left + i64::from(glyph.pixel_w),
                top + i64::from(glyph.pixel_h),
            ),
            clip,
        ) else {
            return;
        };
        // Cropping keeps the 1:1 texel mapping, so shift the UV origin by the
        // same amount the destination moved.
        let texel_x = glyph.atlas_x as f32 + (x as f32 - destination_x as f32);
        let texel_y = glyph.atlas_y as f32 + (y as f32 - destination_y as f32);
        let u0 = texel_x / self.atlas_size.0;
        let v0 = texel_y / self.atlas_size.1;
        let u1 = (texel_x + width as f32) / self.atlas_size.0;
        let v1 = (texel_y + height as f32) / self.atlas_size.1;
        let fill = if atlas.is_color(glyph) {
            Fill::ColorGlyph
        } else {
            Fill::MaskGlyph
        };
        self.push(
            [x as f32, y as f32, width as f32, height as f32],
            [u0, v0, u1, v1],
            rgba(rgb, u8::MAX),
            fill,
        );
    }

    /// Mirror of `draw::draw_cell_underline_in_band` without a band offset.
    fn underline(
        &mut self,
        origin: (i32, i32),
        width: u32,
        cell_height: u32,
        ascent: f32,
        style: UnderlineStyle,
        color: u32,
    ) {
        let Some(anchor_y) = underline_anchor_y(origin.1, cell_height, ascent, style) else {
            return;
        };
        let Some(last_y) = i32::try_from(cell_height)
            .ok()
            .and_then(|height| height.checked_sub(1))
            .and_then(|height| origin.1.checked_add(height))
        else {
            return;
        };
        match style {
            UnderlineStyle::None => {}
            UnderlineStyle::Single => self.underline_segment(origin, 0, anchor_y, width, color),
            UnderlineStyle::Double => {
                self.underline_segment(origin, 0, anchor_y, width, color);
                if let Some(second_y) = anchor_y.checked_add(2).filter(|y| *y <= last_y) {
                    self.underline_segment(origin, 0, second_y, width, color);
                }
            }
            UnderlineStyle::Curly => {
                const OFFSETS: [i32; 6] = [0, 1, 1, 0, -1, -1];
                for x_offset in 0..width {
                    let offset = OFFSETS[x_offset as usize % OFFSETS.len()];
                    if let Some(y) = anchor_y.checked_add(offset) {
                        self.underline_segment(
                            origin,
                            x_offset,
                            y.clamp(origin.1, last_y),
                            1,
                            color,
                        );
                    }
                }
            }
            UnderlineStyle::Dotted => {
                for x_offset in (0..width).step_by(3) {
                    self.underline_segment(origin, x_offset, anchor_y, 1, color);
                }
            }
            UnderlineStyle::Dashed => {
                for x_offset in (0..width).step_by(6) {
                    self.underline_segment(
                        origin,
                        x_offset,
                        anchor_y,
                        (width - x_offset).min(4),
                        color,
                    );
                }
            }
        }
    }

    fn underline_segment(
        &mut self,
        origin: (i32, i32),
        x_offset: u32,
        y: i32,
        segment_width: u32,
        color: u32,
    ) {
        let Some(x) = i32::try_from(x_offset)
            .ok()
            .and_then(|offset| origin.0.checked_add(offset))
        else {
            return;
        };
        self.rect((x, y), (segment_width, 1), color, u8::MAX);
    }

    fn image(&mut self, index: usize, image: &ImageSnapshot) {
        let (x, y, width, height) = image.rectangle;
        if ![x, y, width, height].iter().all(|value| value.is_finite())
            || width <= 0.0
            || height <= 0.0
        {
            return;
        }
        self.close_cells();
        let instance = self.count;
        self.push(
            [x, y, width, height],
            [0.0, 0.0, 1.0, 1.0],
            [1.0; 4],
            Fill::Image,
        );
        self.batches.push(Batch::Image { instance, index });
        self.open_batch_start = self.count;
    }
}

fn rgba(rgb: u32, alpha: u8) -> [f32; 4] {
    [
        ((rgb >> 16) & 0xff) as f32 / 255.0,
        ((rgb >> 8) & 0xff) as f32 / 255.0,
        (rgb & 0xff) as f32 / 255.0,
        f32::from(alpha) / 255.0,
    ]
}

/// Clip `(left, top, right, bottom)` to an optional rectangle and convert it
/// to an origin/size pair when something is left.
fn intersect(bounds: (i64, i64, i64, i64), clip: Option<Rect>) -> Option<Rect> {
    let (mut left, mut top, mut right, mut bottom) = bounds;
    if let Some((clip_origin, clip_size)) = clip {
        left = left.max(i64::from(clip_origin.0));
        top = top.max(i64::from(clip_origin.1));
        right = right.min(i64::from(clip_origin.0) + i64::from(clip_size.0));
        bottom = bottom.min(i64::from(clip_origin.1) + i64::from(clip_size.1));
    }
    if left >= right || top >= bottom {
        return None;
    }
    Some((
        (i32::try_from(left).ok()?, i32::try_from(top).ok()?),
        (
            u32::try_from(right - left).ok()?,
            u32::try_from(bottom - top).ok()?,
        ),
    ))
}

fn has_text(cell: &crate::grid::cell::Cell) -> bool {
    cell.grapheme.is_some() || (cell.c != ' ' && cell.c != '\0')
}

/// Walk the scene in the same layer order as `draw_grid_snapshot_in_band`
/// for a full repaint, then the IME preedit like `draw_ime_preedit`.
fn build_scene(builder: &mut FrameBuilder, scene: &SceneFrame<'_>, atlas: &mut GlyphAtlas) {
    let snapshot = &scene.snapshot;
    let colors = scene.colors;
    let cell_dimensions = scene.cell_dimensions;
    let cell_size = (u32::from(cell_dimensions.0), u32::from(cell_dimensions.1));
    let images = |builder: &mut FrameBuilder, include: &dyn Fn(i32) -> bool| {
        for (index, image) in scene.images.iter().enumerate() {
            if include(image.z_index) {
                builder.image(index, image);
            }
        }
    };

    images(builder, &|z| z < i32::MIN / 2);
    for row in 0..snapshot.rows {
        for column in 0..snapshot.columns {
            let Some(cell) = snapshot.cell(row, column) else {
                continue;
            };
            if cell.flags.contains(CellFlags::WIDE_CONT)
                || (cell.bg == Color::Default && !cell.flags.contains(CellFlags::REVERSE))
            {
                continue;
            }
            let Some(origin) = cell_origin(row, column, cell_dimensions) else {
                continue;
            };
            let width = if cell.flags.contains(CellFlags::WIDE) {
                cell_size.0.saturating_mul(2)
            } else {
                cell_size.0
            };
            builder.rect(
                origin,
                (width, cell_size.1),
                resolve_cell_colors(colors, cell).background,
                u8::MAX,
            );
        }
    }
    images(builder, &|z| (i32::MIN / 2..0).contains(&z));

    for row in 0..snapshot.rows {
        for column in 0..snapshot.columns {
            let Some(cell) = snapshot.cell(row, column) else {
                continue;
            };
            if cell.flags.contains(CellFlags::WIDE_CONT) || !cell_content_is_visible(cell.flags) {
                continue;
            }
            let Some(origin) = cell_origin(row, column, cell_dimensions) else {
                continue;
            };
            let builtin = if has_text(cell) {
                builtin_cell_rects(cell, cell_size, atlas.scale_factor())
            } else {
                None
            };
            let glyph = if has_text(cell) && builtin.is_none() {
                Some(atlas.get_or_insert_cell(cell))
            } else {
                None
            };
            let underline = cell.underline_style != UnderlineStyle::None;
            if glyph.is_none() && builtin.is_none() && !underline {
                continue;
            }
            let resolved = resolve_cell_colors(colors, cell);
            if let Some(rects) = builtin {
                builder.builtin_rects(&rects, origin, resolved.foreground, None);
            }
            if let Some(glyph) = glyph {
                builder.glyph(atlas, glyph, origin, resolved.foreground, None);
            }
            if underline {
                let width = if cell.flags.contains(CellFlags::WIDE) {
                    cell_size.0.saturating_mul(2)
                } else {
                    cell_size.0
                };
                builder.underline(
                    origin,
                    width,
                    cell_size.1,
                    atlas.ascent,
                    cell.underline_style,
                    resolve_cell_underline_color(colors, cell, resolved.foreground),
                );
            }
        }
    }
    images(builder, &|z| z >= 0);

    if scene.draw_cursor() {
        build_cursor(builder, scene, atlas, cell_size);
    }
    if let Some(layout) = scene.preedit {
        build_preedit(builder, scene, atlas, layout, cell_size);
    }
    builder.close_cells();
}

fn build_cursor(
    builder: &mut FrameBuilder,
    scene: &SceneFrame<'_>,
    atlas: &mut GlyphAtlas,
    cell_size: (u32, u32),
) {
    let snapshot = &scene.snapshot;
    let colors = scene.colors;
    let cell_dimensions = scene.cell_dimensions;
    let Some(cursor) = snapshot.cursor else {
        return;
    };
    let Some(origin) = cell_origin(cursor.row, cursor.column, cell_dimensions) else {
        return;
    };
    let Some((cursor_origin, cursor_size)) = cursor_rectangle(cursor.shape, origin, cell_size)
    else {
        return;
    };
    let cursor_cell = snapshot.rendered_cell(cursor.row, cursor.column);
    let cursor_background = cursor_cell
        .map(|cell| resolve_cell_colors(colors, cell).background)
        .unwrap_or(scene.background);
    builder.rect(
        cursor_origin,
        cursor_size,
        colors
            .cursor()
            .map(|color| rgb_to_xrgb(color.0, color.1, color.2))
            .unwrap_or_else(|| contrasting_cursor_color(cursor_background)),
        if colors.cursor().is_some() {
            u8::MAX
        } else {
            CURSOR_ALPHA
        },
    );
    let Some(cursor_color) = colors
        .cursor()
        .filter(|_| cursor.shape == CursorShape::Block)
    else {
        return;
    };
    let Some(cell) = cursor_cell.filter(|cell| {
        cell_content_is_visible(cell.flags)
            && !cell.flags.contains(CellFlags::WIDE_CONT)
            && has_text(cell)
    }) else {
        return;
    };
    let column = if snapshot
        .cell(cursor.row, cursor.column)
        .is_some_and(|cell| cell.flags.contains(CellFlags::WIDE_CONT))
    {
        cursor.column.saturating_sub(1)
    } else {
        cursor.column
    };
    let Some(glyph_origin) = cell_origin(cursor.row, column, cell_dimensions) else {
        return;
    };
    let cursor_color = rgb_to_xrgb(cursor_color.0, cursor_color.1, cursor_color.2);
    let mut foreground = cursor_background;
    if foreground == cursor_color {
        foreground = scene.background;
    }
    if foreground == cursor_color {
        foreground = contrasting_cursor_color(cursor_color);
    }
    let clip = Some((cursor_origin, cursor_size));
    if let Some(rects) = builtin_cell_rects(cell, cell_size, atlas.scale_factor()) {
        builder.builtin_rects(&rects, glyph_origin, foreground, clip);
        return;
    }
    let glyph = atlas.get_or_insert_cell(cell);
    builder.glyph(atlas, glyph, glyph_origin, foreground, clip);
}

fn build_preedit(
    builder: &mut FrameBuilder,
    scene: &SceneFrame<'_>,
    atlas: &mut GlyphAtlas,
    layout: &ImePreeditLayout,
    cell_size: (u32, u32),
) {
    let snapshot = &scene.snapshot;
    let colors = scene.colors;
    let cell_dimensions = scene.cell_dimensions;
    for &(row, column) in &layout.cleared_cells {
        let Some(origin) = cell_origin(row, column, cell_dimensions) else {
            continue;
        };
        let background = snapshot
            .rendered_cell(row, column)
            .map(|cell| resolve_cell_colors(colors, cell).background)
            .unwrap_or(scene.background);
        builder.rect(origin, cell_size, background, u8::MAX);
    }
    for glyph in &layout.glyphs {
        let Some(origin) = cell_origin(glyph.row, glyph.column, cell_dimensions) else {
            continue;
        };
        let Some(width) = cell_size.0.checked_mul(u32::from(glyph.width.max(1))) else {
            continue;
        };
        let (_, background) = ime_preedit_glyph_colors(colors, snapshot, glyph);
        builder.rect(origin, (width, cell_size.1), background, u8::MAX);
    }
    for glyph in &layout.glyphs {
        if glyph.character == ' ' || glyph.character == '\0' {
            continue;
        }
        let Some(origin) = cell_origin(glyph.row, glyph.column, cell_dimensions) else {
            continue;
        };
        let (foreground, _) = ime_preedit_glyph_colors(colors, snapshot, glyph);
        let atlas_glyph = atlas.get_or_insert(GlyphKey {
            c: glyph.character,
            bold: false,
            italic: false,
        });
        builder.glyph(atlas, atlas_glyph, origin, foreground, None);
    }
    let underline_height = CURSOR_THICKNESS.min(cell_size.1);
    for glyph in &layout.glyphs {
        let Some(origin) = cell_origin(glyph.row, glyph.column, cell_dimensions) else {
            continue;
        };
        let Some(width) = cell_size.0.checked_mul(u32::from(glyph.width.max(1))) else {
            continue;
        };
        let Some(y) = cell_size
            .1
            .checked_sub(underline_height)
            .and_then(|offset| i32::try_from(offset).ok())
            .and_then(|offset| origin.1.checked_add(offset))
        else {
            continue;
        };
        let (foreground, _) = ime_preedit_glyph_colors(colors, snapshot, glyph);
        builder.rect(
            (origin.0, y),
            (width, underline_height),
            foreground,
            u8::MAX,
        );
    }
    let Some((row, column)) = layout.caret_cell else {
        return;
    };
    let Some(origin) = cell_origin(row, column, cell_dimensions) else {
        return;
    };
    let background = ime_preedit_background_at(colors, snapshot, layout, row, column);
    builder.rect(
        origin,
        (CURSOR_THICKNESS.min(cell_size.0), cell_size.1),
        colors
            .cursor()
            .map(|color| rgb_to_xrgb(color.0, color.1, color.2))
            .unwrap_or_else(|| contrasting_cursor_color(background)),
        u8::MAX,
    );
}

/// `auto` falls back to the CPU path for these: they would only add a copy
/// through the GPU API on top of the same CPU work.
pub(super) fn is_cpu_rasterizer(name: &str, device_type: wgpu::DeviceType) -> bool {
    let name = name.to_ascii_lowercase();
    device_type == wgpu::DeviceType::Cpu
        || CPU_RASTERIZER_NAMES
            .iter()
            .any(|needle| name.contains(needle))
}

fn choose_surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    // Non-sRGB 8-bit formats write the same bytes the CPU path computes.
    [
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Rgba8Unorm,
    ]
    .into_iter()
    .find(|preferred| formats.contains(preferred))
    .or_else(|| formats.first().copied())
}

struct AtlasBinding {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: (u32, u32),
}

struct CachedImage {
    bind_group: wgpu::BindGroup,
    last_used: u64,
}

pub(super) struct WgpuPresenter {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_info: wgpu::AdapterInfo,
    config: wgpu::SurfaceConfiguration,
    configured_size: Option<(u32, u32)>,
    render_format: wgpu::TextureFormat,
    readback: bool,
    pipeline: wgpu::RenderPipeline,
    scene_layout: wgpu::BindGroupLayout,
    image_layout: wgpu::BindGroupLayout,
    atlas_sampler: wgpu::Sampler,
    image_sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    uniform_size: Option<(u32, u32)>,
    instance_buffer: wgpu::Buffer,
    instance_capacity: u64,
    atlas: Option<AtlasBinding>,
    placeholder_image: wgpu::BindGroup,
    images: HashMap<ImageKey, CachedImage>,
    max_texture_dimension: u32,
    frame_index: u64,
    presented_frames: u64,
    builder: FrameBuilder,
}

impl Drop for WgpuPresenter {
    fn drop(&mut self) {
        log_presented_frames("wgpu", self.presented_frames);
    }
}

impl WgpuPresenter {
    pub(super) fn new(window: Arc<Window>, options: WgpuOptions) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            // EGL (the GL backend) binds to the window's X11/Wayland display.
            display: Some(Box::new(window.clone())),
        });
        let surface = instance
            .create_surface(window.clone())
            .map_err(|error| format!("could not create a wgpu surface: {error}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|error| format!("no wgpu adapter can present to this window: {error}"))?;
        let adapter_info = adapter.get_info();
        if !options.allow_cpu_adapter
            && is_cpu_rasterizer(&adapter_info.name, adapter_info.device_type)
        {
            return Err(format!(
                "wgpu adapter {:?} is a CPU rasterizer",
                adapter_info.name
            ));
        }
        let limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());
        let max_texture_dimension = limits.max_texture_dimension_2d;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("kokuban"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            ..Default::default()
        }))
        .map_err(|error| format!("could not create a wgpu device: {error}"))?;
        // Validation problems must not abort the terminal; they are logged and
        // the frame is simply wrong, like a software-path bug would be.
        device.on_uncaptured_error(Arc::new(|error| log::error!("wgpu: {error}")));

        let capabilities = surface.get_capabilities(&adapter);
        let surface_format = choose_surface_format(&capabilities.formats)
            .ok_or_else(|| "the wgpu surface reports no texture formats".to_string())?;
        let render_format = surface_format.remove_srgb_suffix();
        let mut config = surface
            .get_default_config(&adapter, 1, 1)
            .ok_or_else(|| "the wgpu adapter cannot present to this surface".to_string())?;
        config.format = surface_format;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.alpha_mode = if capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::Opaque)
        {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            capabilities
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };
        if render_format != surface_format {
            config.view_formats.push(render_format);
        }
        // Smoke read-back draws the same instances into an offscreen texture,
        // so it does not depend on the swapchain offering COPY_SRC (GL does
        // not) and only needs a 4-byte format to scan.
        let readback = options.readback_first_frame
            && matches!(
                render_format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kokuban cells"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kokuban scene"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                sampler_entry(2),
            ],
        });
        let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kokuban image"),
            entries: &[texture_entry(0), sampler_entry(1)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kokuban"),
            bind_group_layouts: &[Some(&scene_layout), Some(&image_layout)],
            immediate_size: 0,
        });
        let attributes =
            wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Uint32];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kokuban cells"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: INSTANCE_STRIDE,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: render_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kokuban atlas"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let image_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kokuban image"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kokuban uniforms"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instance_capacity = INITIAL_INSTANCE_CAPACITY;
        let instance_buffer = Self::create_instance_buffer(&device, instance_capacity);
        let placeholder_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kokuban placeholder"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_rgba(&queue, &placeholder_texture, (1, 1), &[255; 4]);
        let placeholder_image =
            Self::image_bind_group(&device, &image_layout, &image_sampler, &placeholder_texture);

        Ok(Self {
            window,
            surface,
            device,
            queue,
            adapter_info,
            config,
            configured_size: None,
            render_format,
            readback,
            pipeline,
            scene_layout,
            image_layout,
            atlas_sampler,
            image_sampler,
            uniform_buffer,
            uniform_size: None,
            instance_buffer,
            instance_capacity,
            atlas: None,
            placeholder_image,
            images: HashMap::new(),
            max_texture_dimension,
            frame_index: 0,
            presented_frames: 0,
            builder: FrameBuilder::default(),
        })
    }

    /// One line for the launch log: backend, adapter, driver and surface format.
    pub(super) fn describe(&self) -> String {
        format!(
            "{:?} adapter {:?} {:?}, driver {} {}, surface {:?}, readback {}",
            self.adapter_info.backend,
            self.adapter_info.name,
            self.adapter_info.device_type,
            self.adapter_info.driver,
            self.adapter_info.driver_info,
            self.config.format,
            if self.readback { "on" } else { "off" }
        )
    }

    fn create_instance_buffer(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kokuban instances"),
            size: capacity * INSTANCE_STRIDE,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn image_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        texture: &wgpu::Texture,
    ) -> wgpu::BindGroup {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kokuban image"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    fn create_rgba_texture(&self, label: &str, size: (u32, u32)) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// Upload the atlas when it grew new glyphs (`dirty`) or was rebuilt for
    /// another scale. Only this presenter clears `dirty` on Linux.
    fn sync_atlas(&mut self, atlas: &mut GlyphAtlas) -> Result<(), String> {
        let size = (atlas.width, atlas.height);
        if size.0 == 0
            || size.1 == 0
            || size.0 > self.max_texture_dimension
            || size.1 > self.max_texture_dimension
        {
            return Err(format!(
                "glyph atlas {}x{} exceeds the GPU texture limit {}",
                size.0, size.1, self.max_texture_dimension
            ));
        }
        if self
            .atlas
            .as_ref()
            .is_none_or(|binding| binding.size != size)
        {
            let texture = self.create_rgba_texture("kokuban atlas", size);
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("kokuban scene"),
                layout: &self.scene_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.atlas_sampler),
                    },
                ],
            });
            self.atlas = Some(AtlasBinding {
                texture,
                bind_group,
                size,
            });
            atlas.dirty = true;
        }
        if atlas.dirty {
            let Some(binding) = self.atlas.as_ref() else {
                return Ok(());
            };
            let required = size.0 as usize * size.1 as usize * 4;
            if atlas.rgba_pixels.len() < required {
                return Err("glyph atlas RGBA buffer is smaller than its dimensions".to_string());
            }
            write_rgba(
                &self.queue,
                &binding.texture,
                size,
                &atlas.rgba_pixels[..required],
            );
            atlas.dirty = false;
        }
        Ok(())
    }

    /// Make sure every image referenced by the batches has a texture, then
    /// drop the least recently used ones beyond the cap.
    fn sync_images(&mut self, images: &[ImageSnapshot]) {
        for batch in &self.builder.batches {
            let Batch::Image { index, .. } = batch else {
                continue;
            };
            let Some(image) = images.get(*index) else {
                continue;
            };
            let key = ImageKey::of(image);
            if let Some(cached) = self.images.get_mut(&key) {
                cached.last_used = self.frame_index;
                continue;
            }
            let (width, height) = image.size;
            let required = (u64::from(width) * u64::from(height)).saturating_mul(4);
            if width == 0
                || height == 0
                || width > self.max_texture_dimension
                || height > self.max_texture_dimension
                || usize::try_from(required).is_ok_and(|required| image.pixels.len() < required)
                || usize::try_from(required).is_err()
            {
                log::warn!(
                    "skipping image {}x{} that the GPU presenter cannot upload",
                    width,
                    height
                );
                continue;
            }
            let texture = self.create_rgba_texture("kokuban image", image.size);
            write_rgba(
                &self.queue,
                &texture,
                image.size,
                &image.pixels[..required as usize],
            );
            let bind_group = Self::image_bind_group(
                &self.device,
                &self.image_layout,
                &self.image_sampler,
                &texture,
            );
            self.images.insert(
                key,
                CachedImage {
                    bind_group,
                    last_used: self.frame_index,
                },
            );
        }
        while self.images.len() > MAX_CACHED_IMAGES {
            let Some(oldest) = self
                .images
                .iter()
                .filter(|(_, cached)| cached.last_used != self.frame_index)
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.images.remove(&oldest);
        }
    }

    fn upload_instances(&mut self) {
        let needed = u64::from(self.builder.count);
        if needed > self.instance_capacity {
            self.instance_capacity = needed.next_power_of_two();
            self.instance_buffer =
                Self::create_instance_buffer(&self.device, self.instance_capacity);
        }
        if !self.builder.bytes.is_empty() {
            self.queue
                .write_buffer(&self.instance_buffer, 0, &self.builder.bytes);
        }
    }

    fn upload_uniforms(&mut self, frame_size: (u32, u32)) {
        if self.uniform_size == Some(frame_size) {
            return;
        }
        let mut bytes = [0u8; 16];
        bytes[..4].copy_from_slice(&(frame_size.0 as f32).to_ne_bytes());
        bytes[4..8].copy_from_slice(&(frame_size.1 as f32).to_ne_bytes());
        self.queue.write_buffer(&self.uniform_buffer, 0, &bytes);
        self.uniform_size = Some(frame_size);
    }

    fn configure(&mut self, size: (u32, u32)) {
        self.config.width = size.0;
        self.config.height = size.1;
        self.surface.configure(&self.device, &self.config);
        self.configured_size = Some(size);
    }

    /// Acquire the next swapchain image, reconfiguring once after a lost or
    /// outdated surface. `None` skips this frame without failing the window.
    fn acquire(&mut self, size: (u32, u32)) -> Result<Option<wgpu::SurfaceTexture>, String> {
        for attempt in 0..2 {
            match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(texture)
                | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => return Ok(Some(texture)),
                wgpu::CurrentSurfaceTexture::Timeout => {
                    log::warn!("wgpu surface timed out; skipping this frame");
                    return Ok(None);
                }
                wgpu::CurrentSurfaceTexture::Occluded => return Ok(None),
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost
                    if attempt == 0 =>
                {
                    self.configure(size);
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    return Err("wgpu surface was lost and could not be recreated".to_string());
                }
                wgpu::CurrentSurfaceTexture::Validation => {
                    return Err("wgpu surface configuration was rejected".to_string());
                }
            }
        }
        Ok(None)
    }

    fn clear_color(background: u32) -> wgpu::Color {
        let channel = |shift: u32| f64::from((background >> shift) & 0xff) / 255.0;
        wgpu::Color {
            r: channel(16),
            g: channel(8),
            b: channel(0),
            a: 1.0,
        }
    }

    /// Copy the presented frame to a mapped buffer and look for any pixel
    /// that differs from the background. Smoke mode only.
    fn read_back_visible_content(
        &self,
        staging: &wgpu::Buffer,
        frame_size: (u32, u32),
        bytes_per_row: u32,
        background: u32,
    ) -> Result<bool, String> {
        let slice = staging.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| format!("could not wait for the GPU read-back: {error}"))?;
        receiver
            .recv()
            .map_err(|_| "GPU read-back callback never ran".to_string())?
            .map_err(|error| format!("could not map the GPU read-back buffer: {error}"))?;
        let data = slice
            .get_mapped_range()
            .map_err(|error| format!("could not read the GPU read-back buffer: {error}"))?;
        let expected = match self.render_format {
            wgpu::TextureFormat::Rgba8Unorm => [
                ((background >> 16) & 0xff) as u8,
                ((background >> 8) & 0xff) as u8,
                (background & 0xff) as u8,
            ],
            _ => [
                (background & 0xff) as u8,
                ((background >> 8) & 0xff) as u8,
                ((background >> 16) & 0xff) as u8,
            ],
        };
        let row_bytes = frame_size.0 as usize * 4;
        let visible = data
            .chunks(bytes_per_row as usize)
            .take(frame_size.1 as usize)
            .any(|row| {
                row[..row_bytes]
                    .chunks_exact(4)
                    .any(|pixel| pixel[..3] != expected)
            });
        drop(data);
        staging.unmap();
        Ok(visible)
    }
}

impl WgpuPresenter {
    /// Clear to the background and replay the frame's batches into `view`.
    fn record_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        scene: &SceneFrame<'_>,
    ) -> Result<(), String> {
        let Some(atlas_binding) = self.atlas.as_ref() else {
            return Err("GPU atlas texture missing after upload".to_string());
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kokuban frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(Self::clear_color(scene.background)),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &atlas_binding.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        let mut placeholder_bound = false;
        for batch in &self.builder.batches {
            match batch {
                Batch::Cells(range) => {
                    if !placeholder_bound {
                        pass.set_bind_group(1, &self.placeholder_image, &[]);
                        placeholder_bound = true;
                    }
                    pass.draw(0..6, range.clone());
                }
                Batch::Image { instance, index } => {
                    let Some(cached) = scene
                        .images
                        .get(*index)
                        .and_then(|image| self.images.get(&ImageKey::of(image)))
                    else {
                        continue;
                    };
                    pass.set_bind_group(1, &cached.bind_group, &[]);
                    placeholder_bound = false;
                    pass.draw(0..6, *instance..*instance + 1);
                }
            }
        }
        Ok(())
    }

    /// Smoke mode: draw the frame again into a texture that can be copied to
    /// a mappable buffer. Returns the staging buffer and its padded row size.
    fn record_readback(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &SceneFrame<'_>,
        frame_size: (u32, u32),
    ) -> Result<(wgpu::Buffer, u32), String> {
        let offscreen = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kokuban read-back target"),
            size: wgpu::Extent3d {
                width: frame_size.0,
                height: frame_size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.render_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(encoder, &view, scene)?;
        let bytes_per_row = (frame_size.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kokuban read-back"),
            size: u64::from(bytes_per_row) * u64::from(frame_size.1),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &offscreen,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: frame_size.0,
                height: frame_size.1,
                depth_or_array_layers: 1,
            },
        );
        Ok((staging, bytes_per_row))
    }
}

fn write_rgba(queue: &wgpu::Queue, texture: &wgpu::Texture, size: (u32, u32), pixels: &[u8]) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.0 * 4),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
    );
}

impl FramePresenter for WgpuPresenter {
    fn resize(&mut self, (width, height): (NonZeroU32, NonZeroU32)) -> Result<(), String> {
        let size = (width.get(), height.get());
        if size.0 > self.max_texture_dimension || size.1 > self.max_texture_dimension {
            return Err(format!(
                "window {}x{} exceeds the GPU texture limit {}",
                size.0, size.1, self.max_texture_dimension
            ));
        }
        if self.configured_size != Some(size) {
            self.configure(size);
        }
        Ok(())
    }

    fn present(
        &mut self,
        scene: SceneFrame<'_>,
        atlas: &mut GlyphAtlas,
    ) -> Result<PresentReport, String> {
        let frame_size = scene.frame_pixels();
        self.resize(scene.frame_size)?;
        self.frame_index += 1;

        // CPU work first: it may rasterize new glyphs into the atlas.
        let mut builder = std::mem::take(&mut self.builder);
        builder.reset(atlas);
        build_scene(&mut builder, &scene, atlas);
        self.builder = builder;
        self.sync_atlas(atlas)?;
        self.sync_images(&scene.images);
        self.upload_instances();
        self.upload_uniforms(frame_size);

        let Some(frame) = self.acquire(frame_size)? else {
            return Ok(PresentReport {
                visible_content: None,
            });
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.render_format),
            ..Default::default()
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kokuban frame"),
            });
        self.record_pass(&mut encoder, &view, &scene)?;
        let readback = if self.readback && scene.detect_visible_content {
            Some(self.record_readback(&mut encoder, &scene, frame_size)?)
        } else {
            None
        };
        self.queue.submit(std::iter::once(encoder.finish()));
        self.window.pre_present_notify();
        self.queue.present(frame);
        self.presented_frames += 1;
        let visible_content = match readback {
            Some((staging, bytes_per_row)) => Some(self.read_back_visible_content(
                &staging,
                frame_size,
                bytes_per_row,
                scene.background,
            )?),
            None => {
                // Let wgpu reclaim finished frames without blocking.
                let _ = self.device.poll(wgpu::PollType::Poll);
                None
            }
        };
        Ok(PresentReport { visible_content })
    }

    fn invalidate(&mut self) {
        // Every frame is a full redraw; the atlas re-uploads through `dirty`.
    }

    fn supports_partial_damage(&self) -> bool {
        false
    }

    fn backend_name(&self) -> &'static str {
        "wgpu"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_rasterizers_are_recognized_by_name_or_device_type() {
        assert!(is_cpu_rasterizer(
            "llvmpipe (LLVM 17.0.6, 256 bits)",
            wgpu::DeviceType::Cpu
        ));
        assert!(is_cpu_rasterizer("Mesa llvmpipe", wgpu::DeviceType::Other));
        assert!(is_cpu_rasterizer(
            "SwiftShader Device",
            wgpu::DeviceType::Other
        ));
        assert!(is_cpu_rasterizer("lavapipe", wgpu::DeviceType::VirtualGpu));
        assert!(!is_cpu_rasterizer(
            "Intel(R) Iris(R) Xe Graphics",
            wgpu::DeviceType::IntegratedGpu
        ));
        assert!(!is_cpu_rasterizer(
            "AMD Radeon RX 7800 XT",
            wgpu::DeviceType::DiscreteGpu
        ));
    }

    #[test]
    fn surface_format_prefers_linear_8_bit_formats() {
        use wgpu::TextureFormat::{Bgra8Unorm, Bgra8UnormSrgb, Rgba8Unorm, Rgba8UnormSrgb};
        assert_eq!(
            choose_surface_format(&[Bgra8UnormSrgb, Bgra8Unorm]),
            Some(Bgra8Unorm)
        );
        assert_eq!(
            choose_surface_format(&[Rgba8UnormSrgb, Rgba8Unorm]),
            Some(Rgba8Unorm)
        );
        assert_eq!(
            choose_surface_format(&[Rgba8UnormSrgb]),
            Some(Rgba8UnormSrgb)
        );
        assert_eq!(choose_surface_format(&[]), None);
    }

    #[test]
    fn instances_are_sixty_four_bytes_in_draw_order_and_split_around_images() {
        let mut builder = FrameBuilder {
            atlas_size: (1024.0, 1024.0),
            ..FrameBuilder::default()
        };
        builder.rect((0, 0), (8, 16), 0x00ff_8000, u8::MAX);
        builder.rect((8, 0), (8, 16), 0x0000_0000, 0);
        assert_eq!(builder.count, 1);
        assert_eq!(builder.bytes.len(), INSTANCE_STRIDE as usize);
        let color: Vec<f32> = builder.bytes[32..48]
            .chunks_exact(4)
            .map(|bytes| f32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect();
        assert_eq!(color, vec![1.0, 128.0 / 255.0, 0.0, 1.0]);
        assert_eq!(
            u32::from_ne_bytes([
                builder.bytes[48],
                builder.bytes[49],
                builder.bytes[50],
                builder.bytes[51]
            ]),
            Fill::Solid as u32
        );

        builder.close_cells();
        builder.open_batch_start = builder.count;
        builder.push([0.0; 4], [0.0, 0.0, 1.0, 1.0], [1.0; 4], Fill::Image);
        builder.batches.push(Batch::Image {
            instance: 1,
            index: 0,
        });
        builder.open_batch_start = builder.count;
        builder.rect((0, 16), (8, 16), 0x00ff_ffff, 128);
        builder.close_cells();
        let summary: Vec<(u32, u32)> = builder
            .batches
            .iter()
            .map(|batch| match batch {
                Batch::Cells(range) => (range.start, range.end),
                Batch::Image { instance, .. } => (*instance, u32::MAX),
            })
            .collect();
        assert_eq!(summary, vec![(0, 1), (1, u32::MAX), (2, 3)]);
    }

    #[test]
    fn glyphs_are_cropped_with_their_texels_inside_the_cursor() {
        let mut builder = FrameBuilder {
            atlas_size: (100.0, 100.0),
            ..FrameBuilder::default()
        };
        let atlas = crate::linux_window::test_support::test_atlas();
        let glyph = GlyphEntry {
            atlas_x: 10,
            atlas_y: 20,
            pixel_w: 8,
            pixel_h: 10,
            bearing_x: -2,
            bearing_y: -8,
            color: false,
        };
        let baseline = rounded_f64_i32(f64::from(atlas.ascent)).unwrap();
        builder.glyph(&atlas, glyph, (4, 0), 0x00ff_ffff, Some(((4, 0), (8, 16))));
        assert_eq!(builder.count, 1);
        let values: Vec<f32> = builder.bytes[..32]
            .chunks_exact(4)
            .map(|bytes| f32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect();
        // Destination clipped from x=2 to the cursor's left edge at x=4.
        assert_eq!(values[0], 4.0);
        assert_eq!(values[1], (baseline - 8) as f32);
        assert_eq!(values[2], 6.0);
        assert_eq!(values[3], 10.0);
        for (actual, expected) in values[4..8].iter().zip([12.0, 20.0, 18.0, 30.0]) {
            assert!(
                (actual - expected / 100.0).abs() < 1e-6,
                "uv {actual} should be {expected}/100"
            );
        }
    }
}
