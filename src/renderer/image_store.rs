use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::image_animation::{
    Animation, AnimationError, AnimationUpdate, MAX_FRAMES_PER_IMAGE, MAX_RETAINED_FRAMES,
};
pub use super::image_decode::ImageFormat;
use super::image_decode::{prepare_image, rgba_byte_len};
use crate::graphics::{next_available_image_id, ImageId};
use crate::parser::kitty_graphics::{
    KittyAnimationControl, KittyFrameComposition, KittyFrameUpload,
};

type MetalTexture = Retained<ProtocolObject<dyn MTLTexture>>;
const MAX_STORED_IMAGES: usize = 4096;

pub struct StoredImage {
    #[allow(dead_code)]
    pub id: ImageId,
    pub texture: MetalTexture,
    pub width: u32,
    pub height: u32,
    /// Texture plus every retained animation canvas. Static images retain no CPU copy.
    pub byte_size: usize,
    pub created_at: Instant,
    displayed_pixels: Option<Arc<[u8]>>,
}

pub struct ImageStore {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    images: HashMap<ImageId, StoredImage>,
    animations: HashMap<ImageId, Animation>,
    next_id: ImageId,
    total_bytes: usize,
    retained_frames: usize,
    max_bytes: usize,
}

// SAFETY: Metal device and textures are thread-safe. The objc2 bindings
// don't implement Send/Sync for protocol objects, but Apple documents Metal
// resources as being safe to use from multiple threads.
unsafe impl Send for ImageStore {}
unsafe impl Sync for ImageStore {}

impl ImageStore {
    pub fn new(device: Retained<ProtocolObject<dyn MTLDevice>>, max_mb: usize) -> Self {
        Self {
            device,
            images: HashMap::new(),
            animations: HashMap::new(),
            next_id: 1,
            total_bytes: 0,
            retained_frames: 0,
            max_bytes: max_mb.saturating_mul(1024 * 1024),
        }
    }

    /// Store image data as a Metal texture. Returns the assigned ID.
    /// If `requested_id` is Some, use that ID (for Kitty protocol).
    pub fn store(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        format: ImageFormat,
        requested_id: Option<ImageId>,
    ) -> Option<ImageId> {
        let (rgba_data, actual_width, actual_height) =
            prepare_image(data, width, height, format, self.max_bytes)?;

        self.store_rgba(&rgba_data, actual_width, actual_height, requested_id)
    }

    fn store_rgba(
        &mut self,
        rgba_data: &[u8],
        width: u32,
        height: u32,
        requested_id: Option<ImageId>,
    ) -> Option<ImageId> {
        self.store_rgba_with(
            rgba_data,
            width,
            height,
            requested_id,
            |store, width, height, data| store.create_texture(width, height, data),
        )
    }

    fn store_rgba_with<F>(
        &mut self,
        rgba_data: &[u8],
        width: u32,
        height: u32,
        requested_id: Option<ImageId>,
        create_texture: F,
    ) -> Option<ImageId>
    where
        F: FnOnce(&Self, u32, u32, &[u8]) -> Option<MetalTexture>,
    {
        if requested_id == Some(0) {
            return None;
        }
        let expected_size = rgba_byte_len(width, height)?;
        if rgba_data.len() < expected_size {
            log::warn!(
                "RGBA data too short: {} < {} ({}x{})",
                rgba_data.len(),
                expected_size,
                width,
                height
            );
            return None;
        }

        if expected_size > self.max_bytes {
            log::warn!(
                "Image is larger than the configured cache: {expected_size} > {} bytes",
                self.max_bytes
            );
            return None;
        }

        let retained_budget = self.max_bytes - expected_size;
        let texture = create_texture(self, width, height, rgba_data)?;
        let id = requested_id.unwrap_or_else(|| self.next_id());

        // If using a requested ID that's higher than next_id, advance next_id
        if let Some(req_id) = requested_id {
            if req_id >= self.next_id {
                self.next_id = req_id.wrapping_add(1).max(1);
            }
        }

        // Remove old image with same ID if it exists
        self.remove(id);

        while self.total_bytes > retained_budget
            || self.images.len() >= MAX_STORED_IMAGES
            || self.retained_frames >= MAX_RETAINED_FRAMES
        {
            self.evict_lru();
        }

        self.images.insert(
            id,
            StoredImage {
                id,
                texture,
                width,
                height,
                byte_size: expected_size,
                created_at: Instant::now(),
                displayed_pixels: None,
            },
        );
        self.total_bytes += expected_size;
        self.retained_frames += 1;
        debug_assert!(self.total_bytes <= self.max_bytes);

        log::trace!(
            "Stored image id={id} size={width}x{height} bytes={expected_size} total={}MB",
            self.total_bytes / (1024 * 1024)
        );

        Some(id)
    }

    fn create_texture(&self, width: u32, height: u32, rgba_data: &[u8]) -> Option<MetalTexture> {
        unsafe {
            let desc =
                MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                    MTLPixelFormat::RGBA8Unorm,
                    width as usize,
                    height as usize,
                    false,
                );
            desc.setUsage(MTLTextureUsage::ShaderRead);

            let texture = self.device.newTextureWithDescriptor(&desc)?;

            let region = MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize {
                    width: width as usize,
                    height: height as usize,
                    depth: 1,
                },
            };

            let bytes_per_row = (width * 4) as usize;
            let bytes_ptr = std::ptr::NonNull::new(rgba_data.as_ptr() as *mut std::ffi::c_void)?;
            texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region,
                0,
                bytes_ptr,
                bytes_per_row,
            );

            Some(texture)
        }
    }

    pub fn remove(&mut self, id: ImageId) {
        if let Some(img) = self.images.remove(&id) {
            self.total_bytes -= img.byte_size;
            self.retained_frames -= self.animations.remove(&id).map_or(1, |a| a.frame_count());
            log::trace!("Removed image id={id}");
        }
    }

    pub fn get(&self, id: ImageId) -> Option<&StoredImage> {
        self.images.get(&id)
    }

    /// Stage shared playback state before allocating a replacement texture or
    /// evicting other images. Published textures stay immutable while a Metal
    /// command buffer (or a synchronized terminal scene) may still use them.
    #[allow(clippy::too_many_arguments)]
    pub fn store_animation_frame(
        &mut self,
        id: ImageId,
        data: &[u8],
        width: u32,
        height: u32,
        format: ImageFormat,
        params: &KittyFrameUpload,
        now: Instant,
    ) -> Result<u32, AnimationError> {
        let image = self.images.get(&id).ok_or(AnimationError::ImageNotFound)?;
        let frame_count = self.animations.get(&id).map_or(1, |a| a.frame_count());
        let append = params.edit_frame.is_none();
        if append && frame_count >= MAX_FRAMES_PER_IMAGE {
            return Err(AnimationError::TooManyFrames);
        }
        let canvas_bytes =
            rgba_byte_len(image.width, image.height).ok_or(AnimationError::InvalidDimensions)?;
        self.animation_evictions(id, frame_count + usize::from(append))?;
        let (pixels, width, height) = prepare_image(data, width, height, format, canvas_bytes)
            .ok_or(AnimationError::InvalidDimensions)?;
        let (mut animation, displayed) = self.prepare_animation(id, now)?;
        let (index, frame) =
            animation.prepare_upload(&pixels, width, height, image.width, image.height, params)?;
        if append {
            animation.reserve_frame()?;
        }
        animation.commit_upload(index, frame, now);
        self.commit_animation(id, animation, &displayed)?;
        Ok(index as u32 + 1)
    }

    pub fn control_animation(
        &mut self,
        id: ImageId,
        params: &KittyAnimationControl,
        now: Instant,
    ) -> Result<(), AnimationError> {
        let (mut animation, displayed) = self.prepare_animation(id, now)?;
        animation.control(params, now)?;
        self.commit_animation(id, animation, &displayed)
    }

    pub fn compose_animation_frame(
        &mut self,
        id: ImageId,
        params: &KittyFrameComposition,
        now: Instant,
    ) -> Result<(), AnimationError> {
        let image = self.images.get(&id).ok_or(AnimationError::ImageNotFound)?;
        let (mut animation, displayed) = self.prepare_animation(id, now)?;
        let (index, pixels) = animation.prepare_composition(params, image.width, image.height)?;
        animation.commit_composition(index, pixels, now);
        self.commit_animation(id, animation, &displayed)
    }

    pub fn delete_animation_frame(
        &mut self,
        id: ImageId,
        frame: u32,
        delete_last_image: bool,
        now: Instant,
    ) -> Result<(), AnimationError> {
        if !self.images.contains_key(&id) {
            return Err(AnimationError::ImageNotFound);
        }
        if self.animations.get(&id).map_or(1, |a| a.frame_count()) == 1 {
            if delete_last_image {
                self.remove(id);
            }
            return Ok(());
        }
        let (mut animation, displayed) = self.prepare_animation(id, now)?;
        animation.delete_frame(frame, now);
        self.commit_animation(id, animation, &displayed)
    }

    /// Hidden images need no timer. Only a changed displayed canvas allocates
    /// and uploads a texture; ordinary terminal redraws reuse existing textures.
    pub fn advance_animations(
        &mut self,
        now: Instant,
        visible: &HashSet<ImageId>,
    ) -> AnimationUpdate {
        let mut update = AnimationUpdate::default();
        for id in visible {
            let Some(animation) = self.animations.get(id) else {
                continue;
            };
            let mut animation = animation.clone();
            let displayed = Arc::clone(
                self.images[id]
                    .displayed_pixels
                    .as_ref()
                    .expect("animated image"),
            );
            let mut deadline = animation.advance(now);
            let changed = !Arc::ptr_eq(&displayed, animation.active_pixels());
            if self.commit_animation(*id, animation, &displayed).is_ok() {
                update.changed |= changed;
            } else {
                // Preserve the previous frame on GPU allocation failure and
                // retry without requesting an already expired deadline.
                deadline = now.checked_add(Duration::from_millis(16));
            }
            if let Some(deadline) = deadline {
                update.next_deadline = Some(
                    update
                        .next_deadline
                        .map_or(deadline, |old| old.min(deadline)),
                );
            }
        }
        update
    }

    fn prepare_animation(
        &self,
        id: ImageId,
        now: Instant,
    ) -> Result<(Animation, Arc<[u8]>), AnimationError> {
        let image = self.images.get(&id).ok_or(AnimationError::ImageNotFound)?;
        if let Some(animation) = self.animations.get(&id) {
            return Ok((
                animation.clone(),
                Arc::clone(image.displayed_pixels.as_ref().expect("animated image")),
            ));
        }
        self.animation_evictions(id, 1)?;
        let pixels = Self::read_texture(image)?;
        Ok((Animation::new(Arc::clone(&pixels), now), pixels))
    }

    fn read_texture(image: &StoredImage) -> Result<Arc<[u8]>, AnimationError> {
        let byte_len =
            rgba_byte_len(image.width, image.height).ok_or(AnimationError::InvalidDimensions)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(byte_len)
            .map_err(|_| AnimationError::AllocationFailed)?;
        pixels.resize(byte_len, 0);
        // Textures are uploaded once by the CPU and are only read by the GPU.
        // Reading back the root needs no GPU-write synchronization.
        unsafe {
            image.texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::new(pixels.as_mut_ptr().cast()).expect("nonempty canvas"),
                image.width as usize * 4,
                MTLRegion {
                    origin: MTLOrigin { x: 0, y: 0, z: 0 },
                    size: MTLSize {
                        width: image.width as usize,
                        height: image.height as usize,
                        depth: 1,
                    },
                },
                0,
            );
        }
        Ok(Arc::from(pixels))
    }

    fn commit_animation(
        &mut self,
        id: ImageId,
        animation: Animation,
        displayed: &Arc<[u8]>,
    ) -> Result<(), AnimationError> {
        self.commit_animation_with(id, animation, displayed, |store, width, height, pixels| {
            store.create_texture(width, height, pixels)
        })
    }

    fn commit_animation_with<F>(
        &mut self,
        id: ImageId,
        animation: Animation,
        displayed: &Arc<[u8]>,
        create_texture: F,
    ) -> Result<(), AnimationError>
    where
        F: FnOnce(&Self, u32, u32, &[u8]) -> Option<MetalTexture>,
    {
        let evictions = self.animation_evictions(id, animation.frame_count())?;
        let image = self.images.get(&id).ok_or(AnimationError::ImageNotFound)?;
        let texture = if Arc::ptr_eq(displayed, animation.active_pixels()) {
            None
        } else {
            Some(
                create_texture(self, image.width, image.height, animation.active_pixels())
                    .ok_or(AnimationError::AllocationFailed)?,
            )
        };
        for evicted in evictions {
            self.remove(evicted);
        }
        let image = self.images.get_mut(&id).expect("root is never evicted");
        self.total_bytes -= image.byte_size;
        self.retained_frames -= self.animations.get(&id).map_or(1, |a| a.frame_count());
        image.byte_size = animation.active_pixels().len() * (animation.frame_count() + 1);
        image.displayed_pixels = Some(Arc::clone(animation.active_pixels()));
        if let Some(texture) = texture {
            image.texture = texture;
        }
        self.total_bytes += image.byte_size;
        self.retained_frames += animation.frame_count();
        self.animations.insert(id, animation);
        debug_assert!(self.total_bytes <= self.max_bytes);
        debug_assert!(self.retained_frames <= MAX_RETAINED_FRAMES);
        Ok(())
    }

    fn animation_evictions(
        &self,
        own_id: ImageId,
        new_frames: usize,
    ) -> Result<Vec<ImageId>, AnimationError> {
        let image = self
            .images
            .get(&own_id)
            .ok_or(AnimationError::ImageNotFound)?;
        let own_bytes = rgba_byte_len(image.width, image.height)
            .and_then(|bytes| bytes.checked_mul(new_frames + 1))
            .filter(|bytes| *bytes <= self.max_bytes)
            .ok_or(AnimationError::TooLarge)?;
        let old_frames = self.animations.get(&own_id).map_or(1, |a| a.frame_count());
        let mut bytes = (self.total_bytes - image.byte_size)
            .checked_add(own_bytes)
            .ok_or(AnimationError::TooLarge)?;
        let mut frames = self.retained_frames - old_frames + new_frames;
        if bytes <= self.max_bytes && frames <= MAX_RETAINED_FRAMES {
            return Ok(Vec::new());
        }
        let mut candidates: Vec<_> = self
            .images
            .values()
            .filter(|image| image.id != own_id)
            .collect();
        candidates.sort_unstable_by_key(|image| image.created_at);
        let mut evictions = Vec::new();
        for candidate in candidates {
            if bytes <= self.max_bytes && frames <= MAX_RETAINED_FRAMES {
                break;
            }
            bytes -= candidate.byte_size;
            frames -= self
                .animations
                .get(&candidate.id)
                .map_or(1, |a| a.frame_count());
            evictions.push(candidate.id);
        }
        if frames > MAX_RETAINED_FRAMES {
            return Err(AnimationError::TooManyFrames);
        }
        Ok(evictions)
    }

    pub(crate) fn image_count(&self) -> usize {
        self.images.len()
    }

    fn evict_lru(&mut self) {
        if let Some((&oldest_id, _)) = self.images.iter().min_by_key(|(_, img)| img.created_at) {
            log::trace!("Evicting image id={oldest_id} (LRU)");
            self.remove(oldest_id);
        }
    }

    /// Assign a non-zero image ID that is not currently in the cache.
    pub fn next_id(&mut self) -> ImageId {
        let images = &self.images;
        next_available_image_id(&mut self.next_id, |id| images.contains_key(&id))
    }
}

#[cfg(test)]
mod tests {
    use super::{AnimationError, KittyAnimationControl, KittyFrameComposition, KittyFrameUpload};
    use super::{ImageFormat, ImageStore};
    use crate::parser::kitty_graphics::{KittyAnimationState, KittyBlendMode};
    use objc2_metal::MTLCreateSystemDefaultDevice;
    use std::collections::HashSet;
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn image_store_with_byte_limit(max_bytes: usize) -> Option<ImageStore> {
        let device = MTLCreateSystemDefaultDevice()?;
        let mut store = ImageStore::new(device, 1);
        store.max_bytes = max_bytes;
        Some(store)
    }

    fn pixels(store: &ImageStore, id: u64) -> Arc<[u8]> {
        ImageStore::read_texture(store.get(id).unwrap()).unwrap()
    }

    fn append_pixel(
        store: &mut ImageStore,
        id: u64,
        value: u8,
        now: Instant,
    ) -> Result<u32, AnimationError> {
        store.store_animation_frame(
            id,
            &[value, 0, 0, 255],
            1,
            1,
            ImageFormat::Rgba,
            &KittyFrameUpload {
                gap_ms: Some(10),
                ..Default::default()
            },
            now,
        )
    }

    #[test]
    fn metal_animation_advances_visible_frames_and_keeps_published_textures_immutable() {
        let Some(mut store) = image_store_with_byte_limit(12) else {
            eprintln!("skipping Metal animation test: no device is available");
            return;
        };
        let now = Instant::now();
        store
            .store(&[10, 0, 0, 255], 1, 1, ImageFormat::Rgba, Some(7))
            .unwrap();
        assert!(store.get(7).unwrap().displayed_pixels.is_none());
        let original_texture = store.get(7).unwrap().texture.clone();
        assert_eq!(append_pixel(&mut store, 7, 20, now), Ok(2));
        assert!(std::ptr::eq(
            &*original_texture,
            &*store.get(7).unwrap().texture
        ));
        assert_eq!(
            store.total_bytes, 12,
            "two CPU canvases plus the GPU texture"
        );
        store
            .control_animation(
                7,
                &KittyAnimationControl {
                    state: Some(KittyAnimationState::Running),
                    gap_ms: Some(10),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(
            store.advance_animations(now, &HashSet::new()).next_deadline,
            None
        );
        let visible = HashSet::from([7]);
        let first = store.advance_animations(now, &visible);
        assert!(!first.changed);
        assert_eq!(first.next_deadline, Some(now + Duration::from_millis(10)));
        let second = store.advance_animations(now + Duration::from_millis(10), &visible);
        assert!(second.changed);
        assert_eq!(&*pixels(&store, 7), &[20, 0, 0, 255]);
        assert!(!std::ptr::eq(
            &*original_texture,
            &*store.get(7).unwrap().texture
        ));
        let current = store.get(7).unwrap().texture.clone();
        assert!(
            !store
                .advance_animations(now + Duration::from_millis(11), &visible)
                .changed
        );
        assert!(std::ptr::eq(&*current, &*store.get(7).unwrap().texture));
        let old_image = super::StoredImage {
            id: 7,
            texture: original_texture,
            width: 1,
            height: 1,
            byte_size: 4,
            created_at: now,
            displayed_pixels: None,
        };
        assert_eq!(
            &*ImageStore::read_texture(&old_image).unwrap(),
            &[10, 0, 0, 255]
        );
        store.remove(7);
        assert_eq!(store.total_bytes, 0);
        assert_eq!(store.retained_frames, 0);
        assert_eq!(
            store
                .advance_animations(now + Duration::from_secs(1), &visible)
                .next_deadline,
            None
        );
    }

    #[test]
    fn metal_animation_selection_edits_composition_and_delete_update_texture_pixels() {
        let Some(mut store) = image_store_with_byte_limit(12) else {
            return;
        };
        let now = Instant::now();
        store
            .store(&[10, 0, 0, 255], 1, 1, ImageFormat::Rgba, Some(1))
            .unwrap();
        append_pixel(&mut store, 1, 20, now).unwrap();
        store
            .control_animation(
                1,
                &KittyAnimationControl {
                    current_frame: NonZeroU32::new(2),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(&*pixels(&store, 1), &[20, 0, 0, 255]);
        store
            .store_animation_frame(
                1,
                &[30, 0, 0, 128],
                1,
                1,
                ImageFormat::Rgba,
                &KittyFrameUpload {
                    edit_frame: NonZeroU32::new(2),
                    blend: KittyBlendMode::Overwrite,
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(&*pixels(&store, 1), &[30, 0, 0, 128]);
        store
            .compose_animation_frame(
                1,
                &KittyFrameComposition {
                    source_frame: 1,
                    destination_frame: 2,
                    blend: KittyBlendMode::Overwrite,
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(&*pixels(&store, 1), &[10, 0, 0, 255]);
        store.delete_animation_frame(1, 2, false, now).unwrap();
        assert_eq!(store.total_bytes, 8);
        assert_eq!(store.retained_frames, 1);
        assert_eq!(&*pixels(&store, 1), &[10, 0, 0, 255]);
        store.delete_animation_frame(1, 1, false, now).unwrap();
        assert_eq!(store.image_count(), 1);
        store.delete_animation_frame(1, 1, true, now).unwrap();
        assert_eq!(store.image_count(), 0);
        assert_eq!(store.total_bytes, 0);
    }

    #[test]
    fn metal_animation_allocation_failure_preserves_cache_playback_and_id() {
        let Some(mut store) = image_store_with_byte_limit(12) else {
            return;
        };
        let now = Instant::now();
        for id in 1..=2 {
            store
                .store(&[id as u8, 0, 0, 255], 1, 1, ImageFormat::Rgba, Some(id))
                .unwrap();
        }
        let old_texture = store.get(1).unwrap().texture.clone();
        let next_id = store.next_id;
        let (mut candidate, displayed) = store.prepare_animation(1, now).unwrap();
        let (index, frame) = candidate
            .prepare_upload(&[30, 0, 0, 255], 1, 1, 1, 1, &KittyFrameUpload::default())
            .unwrap();
        candidate.commit_upload(index, frame, now);
        candidate
            .control(
                &KittyAnimationControl {
                    current_frame: NonZeroU32::new(2),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(
            store.commit_animation_with(1, candidate, &displayed, |cache, _, _, _| {
                assert_eq!(
                    cache.image_count(),
                    2,
                    "eviction must wait for GPU allocation"
                );
                None
            }),
            Err(AnimationError::AllocationFailed)
        );
        assert_eq!(store.total_bytes, 8);
        assert_eq!(store.retained_frames, 2);
        assert_eq!(store.image_count(), 2);
        assert_eq!(store.next_id, next_id);
        assert!(store.animations.is_empty());
        assert!(std::ptr::eq(&*old_texture, &*store.get(1).unwrap().texture));
        assert_eq!(&*pixels(&store, 1), &[1, 0, 0, 255]);
    }

    #[test]
    fn metal_animation_budget_evicts_other_images_and_static_replacement_reclaims_frames() {
        let Some(mut store) = image_store_with_byte_limit(12) else {
            return;
        };
        let now = Instant::now();
        for id in 1..=2 {
            store
                .store(&[id as u8, 0, 0, 255], 1, 1, ImageFormat::Rgba, Some(id))
                .unwrap();
        }
        assert_eq!(append_pixel(&mut store, 1, 20, now), Ok(2));
        assert!(store.get(2).is_none());
        assert_eq!(store.total_bytes, 12);
        assert_eq!(
            append_pixel(&mut store, 1, 30, now),
            Err(AnimationError::TooLarge)
        );
        assert_eq!(store.total_bytes, 12);
        assert_eq!(store.retained_frames, 2);
        store
            .store(&[40, 0, 0, 255], 1, 1, ImageFormat::Rgba, Some(1))
            .unwrap();
        assert_eq!(store.total_bytes, 4);
        assert_eq!(store.retained_frames, 1);
        assert!(store.animations.is_empty());
        assert_eq!(&*pixels(&store, 1), &[40, 0, 0, 255]);
    }

    #[test]
    fn metal_animation_invalid_commands_preserve_displayed_frame_and_budget() {
        let Some(mut store) = image_store_with_byte_limit(16) else {
            return;
        };
        let now = Instant::now();
        store
            .store(&[10, 0, 0, 255], 1, 1, ImageFormat::Rgba, Some(1))
            .unwrap();
        append_pixel(&mut store, 1, 20, now).unwrap();
        let texture = store.get(1).unwrap().texture.clone();
        assert_eq!(
            store.control_animation(
                1,
                &KittyAnimationControl {
                    current_frame: NonZeroU32::new(3),
                    state: Some(KittyAnimationState::Running),
                    ..Default::default()
                },
                now
            ),
            Err(AnimationError::FrameNotFound)
        );
        assert_eq!(
            store.store_animation_frame(
                1,
                &[0; 4],
                1,
                1,
                ImageFormat::Rgba,
                &KittyFrameUpload {
                    x: 1,
                    ..Default::default()
                },
                now
            ),
            Err(AnimationError::InvalidRectangle)
        );
        assert_eq!(
            store.compose_animation_frame(
                1,
                &KittyFrameComposition {
                    source_frame: 1,
                    destination_frame: 1,
                    ..Default::default()
                },
                now
            ),
            Err(AnimationError::OverlappingComposition)
        );
        assert_eq!(store.total_bytes, 12);
        assert_eq!(store.retained_frames, 2);
        assert!(std::ptr::eq(&*texture, &*store.get(1).unwrap().texture));
        assert_eq!(
            store
                .advance_animations(now + Duration::from_secs(1), &HashSet::from([1]))
                .next_deadline,
            None
        );
    }

    #[test]
    fn metal_animation_frame_limits_bound_tiny_image_streams_and_evict_whole_animations() {
        let Some(mut store) = image_store_with_byte_limit(1024 * 1024) else {
            return;
        };
        let now = Instant::now();
        for id in 1..=16 {
            store
                .store(&[10, 0, 0, 255], 1, 1, ImageFormat::Rgba, Some(id))
                .unwrap();
            for frame in 2..=super::MAX_FRAMES_PER_IMAGE {
                assert_eq!(append_pixel(&mut store, id, 20, now), Ok(frame as u32));
            }
        }
        assert_eq!(store.retained_frames, super::MAX_RETAINED_FRAMES);
        assert_eq!(
            append_pixel(&mut store, 16, 30, now),
            Err(AnimationError::TooManyFrames)
        );
        assert_eq!(store.retained_frames, super::MAX_RETAINED_FRAMES);
        set_creation_order(&mut store, &(1..=16).collect::<Vec<_>>());
        store
            .store(&[40, 0, 0, 255], 1, 1, ImageFormat::Rgba, Some(17))
            .unwrap();
        assert!(store.get(1).is_none());
        assert!(!store.animations.contains_key(&1));
        assert_eq!(store.image_count(), 16);
        assert_eq!(store.retained_frames, 15 * super::MAX_FRAMES_PER_IMAGE + 1);
        assert_eq!(
            store.total_bytes,
            15 * (super::MAX_FRAMES_PER_IMAGE + 1) * 4 + 4
        );
        assert_eq!(&*pixels(&store, 17), &[40, 0, 0, 255]);
    }

    fn set_creation_order(store: &mut ImageStore, image_ids: &[u64]) {
        let base = Instant::now()
            .checked_sub(Duration::from_secs(image_ids.len() as u64 + 1))
            .expect("test timestamp should be representable");
        for (offset, image_id) in image_ids.iter().enumerate() {
            store
                .images
                .get_mut(image_id)
                .expect("test image should exist")
                .created_at = base + Duration::from_secs(offset as u64);
        }
    }

    #[test]
    fn replacement_that_fits_after_credit_preserves_other_images() {
        let Some(mut store) = image_store_with_byte_limit(12) else {
            eprintln!("skipping Metal cache test: no device is available");
            return;
        };
        assert_eq!(
            store.store(&[255; 4], 1, 1, ImageFormat::Rgba, Some(1)),
            Some(1)
        );
        assert_eq!(
            store.store(&[127; 4], 1, 1, ImageFormat::Rgba, Some(2)),
            Some(2)
        );
        set_creation_order(&mut store, &[1, 2]);

        assert_eq!(
            store.store(&[63; 8], 1, 2, ImageFormat::Rgba, Some(2)),
            Some(2)
        );

        assert_eq!(store.image_count(), 2);
        assert_eq!(store.total_bytes, 12);
        assert!(store.get(1).is_some());
        assert_eq!(
            store.get(2).map(|image| (image.width, image.height)),
            Some((1, 2))
        );
    }

    #[test]
    fn replacement_evicts_only_the_required_lru_entry() {
        let Some(mut store) = image_store_with_byte_limit(12) else {
            eprintln!("skipping Metal cache test: no device is available");
            return;
        };
        for image_id in 1..=3 {
            assert_eq!(
                store.store(
                    &[image_id as u8; 4],
                    1,
                    1,
                    ImageFormat::Rgba,
                    Some(image_id),
                ),
                Some(image_id)
            );
        }
        set_creation_order(&mut store, &[1, 2, 3]);

        assert_eq!(
            store.store(&[31; 8], 1, 2, ImageFormat::Rgba, Some(3)),
            Some(3)
        );

        assert_eq!(store.image_count(), 2);
        assert_eq!(store.total_bytes, 12);
        assert!(store.get(1).is_none());
        assert!(store.get(2).is_some());
        assert_eq!(
            store.get(3).map(|image| (image.width, image.height)),
            Some((1, 2))
        );
    }

    #[test]
    fn failed_texture_creation_preserves_cache_and_next_id() {
        let Some(mut store) = image_store_with_byte_limit(12) else {
            eprintln!("skipping Metal cache test: no device is available");
            return;
        };
        assert_eq!(
            store.store(&[255; 4], 1, 1, ImageFormat::Rgba, Some(1)),
            Some(1)
        );
        assert_eq!(
            store.store(&[127; 4], 1, 1, ImageFormat::Rgba, Some(2)),
            Some(2)
        );
        set_creation_order(&mut store, &[1, 2]);
        let first_created_at = store.get(1).unwrap().created_at;
        let second_created_at = store.get(2).unwrap().created_at;
        let next_id = store.next_id;

        let result = store.store_rgba_with(&[63; 8], 1, 2, None, |cache, _, _, _| {
            assert_eq!(cache.image_count(), 2);
            assert_eq!(cache.total_bytes, 8);
            None
        });

        assert_eq!(result, None);
        assert_eq!(store.image_count(), 2);
        assert_eq!(store.total_bytes, 8);
        assert_eq!(store.next_id, next_id);
        assert_eq!(store.get(1).unwrap().created_at, first_created_at);
        assert_eq!(store.get(2).unwrap().created_at, second_created_at);
        assert_eq!(
            store.get(2).map(|image| (image.width, image.height)),
            Some((1, 1))
        );
    }
}
