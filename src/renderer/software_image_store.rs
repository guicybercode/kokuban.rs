use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use super::image_animation::{
    pixels_are_opaque, Animation, AnimationError, AnimationUpdate, MAX_FRAMES_PER_IMAGE,
    MAX_RETAINED_FRAMES,
};
use super::image_decode::prepare_image;
pub use super::image_decode::ImageFormat;
use crate::graphics::{next_available_image_id, ImageId};
use crate::parser::kitty_graphics::{
    KittyAnimationControl, KittyFrameComposition, KittyFrameUpload,
};

const MAX_STORED_IMAGES: usize = 4096;

pub struct StoredImage {
    pub pixels: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    /// All pixels of the currently displayed canvas have alpha 255.
    pub opaque: bool,
}

/// CPU images shared with the software renderer without copying pixel buffers.
/// Both pixels and entry count are bounded, including streams of tiny images.
pub struct ImageStore {
    images: HashMap<ImageId, StoredImage>,
    animations: HashMap<ImageId, Animation>,
    insertion_order: VecDeque<ImageId>,
    next_id: ImageId,
    total_bytes: usize,
    retained_frames: usize,
    max_bytes: usize,
    max_images: usize,
}

impl ImageStore {
    pub fn new(max_mb: usize) -> Self {
        Self {
            images: HashMap::new(),
            animations: HashMap::new(),
            insertion_order: VecDeque::new(),
            next_id: 1,
            total_bytes: 0,
            retained_frames: 0,
            max_bytes: max_mb.saturating_mul(1024 * 1024),
            max_images: MAX_STORED_IMAGES,
        }
    }

    pub fn store(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        format: ImageFormat,
        requested_id: Option<ImageId>,
    ) -> Option<ImageId> {
        if requested_id == Some(0) || self.max_images == 0 {
            return None;
        }
        let known_opaque = matches!(format, ImageFormat::Rgb);
        let (pixels, width, height) = prepare_image(data, width, height, format, self.max_bytes)?;
        let byte_size = pixels.len();
        let retained_budget = self.max_bytes.checked_sub(byte_size)?;
        let opaque = known_opaque || pixels_are_opaque(&pixels);
        let pixels = Arc::from(pixels);
        let id = requested_id.unwrap_or_else(|| self.next_id());
        if id >= self.next_id {
            self.next_id = id.wrapping_add(1).max(1);
        }

        // Credit a replacement before deciding which other images to evict.
        // Validation finishes first so rejected uploads preserve live images.
        self.remove(id);
        while self.total_bytes > retained_budget
            || self.images.len() >= self.max_images
            || self.retained_frames >= MAX_RETAINED_FRAMES
        {
            let oldest_id = self.insertion_order.front().copied()?;
            self.remove(oldest_id);
        }
        self.images.insert(
            id,
            StoredImage {
                pixels,
                width,
                height,
                opaque,
            },
        );
        self.insertion_order.push_back(id);
        // The retained budget guarantees this addition cannot overflow.
        self.total_bytes += byte_size;
        self.retained_frames += 1;
        debug_assert!(self.total_bytes <= self.max_bytes);
        debug_assert!(self.images.len() <= self.max_images);
        Some(id)
    }

    pub fn get(&self, id: ImageId) -> Option<&StoredImage> {
        self.images.get(&id)
    }

    pub fn remove(&mut self, id: ImageId) {
        if let Some(image) = self.images.remove(&id) {
            let frames = self
                .animations
                .remove(&id)
                .map_or(1, |animation| animation.frame_count());
            self.total_bytes -= image.pixels.len() * frames;
            self.retained_frames -= frames;
            self.insertion_order.retain(|stored_id| *stored_id != id);
        }
    }

    /// Append or patch a Kitty frame. Validation and canvas construction finish
    /// before eviction; an append can evict other images, never its own root.
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
        let append = params.edit_frame.is_none();
        let frame_count = self
            .animations
            .get(&id)
            .map_or(1, |animation| animation.frame_count());
        if append && frame_count >= MAX_FRAMES_PER_IMAGE {
            return Err(AnimationError::TooManyFrames);
        }
        let extra_frames = usize::from(append);
        let extra_bytes = image.pixels.len() * extra_frames;
        if image
            .pixels
            .len()
            .checked_mul(frame_count + extra_frames)
            .is_none_or(|bytes| bytes > self.max_bytes)
        {
            return Err(AnimationError::TooLarge);
        }
        let evictions = self.animation_evictions(id, extra_bytes, extra_frames)?;
        let (pixels, width, height) =
            prepare_image(data, width, height, format, image.pixels.len())
                .ok_or(AnimationError::InvalidDimensions)?;
        let mut new_animation = if self.animations.contains_key(&id) {
            None
        } else {
            Some(Animation::new(Arc::clone(&image.pixels), now))
        };
        let animation = self
            .animations
            .get(&id)
            .or(new_animation.as_ref())
            .expect("root exists");
        let (index, frame) =
            animation.prepare_upload(&pixels, width, height, image.width, image.height, params)?;
        if append {
            self.animations
                .get_mut(&id)
                .or(new_animation.as_mut())
                .expect("root exists")
                .reserve_frame()?;
        }
        for evicted in evictions {
            self.remove(evicted);
        }
        if let Some(animation) = new_animation {
            self.animations.insert(id, animation);
        }
        let animation = self.animations.get_mut(&id).expect("root exists");
        animation.commit_upload(index, frame, now);
        let image = self.images.get_mut(&id).expect("root is not evicted");
        image.pixels = Arc::clone(animation.active_pixels());
        image.opaque = animation.active_opaque();
        self.total_bytes += extra_bytes;
        self.retained_frames += extra_frames;
        debug_assert!(self.total_bytes <= self.max_bytes);
        debug_assert!(self.retained_frames <= MAX_RETAINED_FRAMES);
        Ok(index as u32 + 1)
    }

    pub fn control_animation(
        &mut self,
        id: ImageId,
        params: &KittyAnimationControl,
        now: Instant,
    ) -> Result<(), AnimationError> {
        let image = self
            .images
            .get_mut(&id)
            .ok_or(AnimationError::ImageNotFound)?;
        if let Some(animation) = self.animations.get_mut(&id) {
            animation.control(params, now)?;
            image.pixels = Arc::clone(animation.active_pixels());
            image.opaque = animation.active_opaque();
        } else {
            let mut animation = Animation::new(Arc::clone(&image.pixels), now);
            animation.control(params, now)?;
            image.pixels = Arc::clone(animation.active_pixels());
            image.opaque = animation.active_opaque();
            self.animations.insert(id, animation);
        }
        Ok(())
    }

    pub fn compose_animation_frame(
        &mut self,
        id: ImageId,
        params: &KittyFrameComposition,
        now: Instant,
    ) -> Result<(), AnimationError> {
        let image = self
            .images
            .get_mut(&id)
            .ok_or(AnimationError::ImageNotFound)?;
        if let Some(animation) = self.animations.get_mut(&id) {
            let (index, pixels) =
                animation.prepare_composition(params, image.width, image.height)?;
            animation.commit_composition(index, pixels, now);
            image.pixels = Arc::clone(animation.active_pixels());
            image.opaque = animation.active_opaque();
        } else {
            let mut animation = Animation::new(Arc::clone(&image.pixels), now);
            let (index, pixels) =
                animation.prepare_composition(params, image.width, image.height)?;
            animation.commit_composition(index, pixels, now);
            image.pixels = Arc::clone(animation.active_pixels());
            image.opaque = animation.active_opaque();
            self.animations.insert(id, animation);
        }
        Ok(())
    }

    pub fn delete_animation_frame(
        &mut self,
        id: ImageId,
        frame: u32,
        delete_last_image: bool,
        now: Instant,
    ) -> Result<(), AnimationError> {
        let image = self
            .images
            .get_mut(&id)
            .ok_or(AnimationError::ImageNotFound)?;
        let frame_count = self
            .animations
            .get(&id)
            .map_or(1, |animation| animation.frame_count());
        if frame_count == 1 {
            if delete_last_image {
                self.remove(id);
            }
            return Ok(());
        }
        let animation = self
            .animations
            .get_mut(&id)
            .expect("multiple frames require animation");
        animation.delete_frame(frame, now);
        image.pixels = Arc::clone(animation.active_pixels());
        image.opaque = animation.active_opaque();
        self.total_bytes -= image.pixels.len();
        self.retained_frames -= 1;
        Ok(())
    }

    /// Only visible images request wakeups. A hidden animation catches up in
    /// bounded work when it is displayed again; it needs no background timer.
    pub fn advance_animations(
        &mut self,
        now: Instant,
        visible: &HashSet<ImageId>,
    ) -> AnimationUpdate {
        let mut update = AnimationUpdate::default();
        for id in visible {
            let Some(animation) = self.animations.get_mut(id) else {
                continue;
            };
            let deadline = animation.advance(now);
            if let Some(deadline) = deadline {
                update.next_deadline = Some(
                    update
                        .next_deadline
                        .map_or(deadline, |current| current.min(deadline)),
                );
            }
            let image = self
                .images
                .get_mut(id)
                .expect("animations belong to retained images");
            if !Arc::ptr_eq(&image.pixels, animation.active_pixels()) {
                image.pixels = Arc::clone(animation.active_pixels());
                image.opaque = animation.active_opaque();
                update.changed = true;
            }
        }
        update
    }

    fn animation_evictions(
        &self,
        own_id: ImageId,
        extra_bytes: usize,
        extra_frames: usize,
    ) -> Result<Vec<ImageId>, AnimationError> {
        let mut bytes = self
            .total_bytes
            .checked_add(extra_bytes)
            .ok_or(AnimationError::TooLarge)?;
        let mut frames = self
            .retained_frames
            .checked_add(extra_frames)
            .ok_or(AnimationError::TooManyFrames)?;
        let mut evictions = Vec::new();
        for id in &self.insertion_order {
            if bytes <= self.max_bytes && frames <= MAX_RETAINED_FRAMES {
                break;
            }
            if *id != own_id {
                let image = &self.images[id];
                let count = self
                    .animations
                    .get(id)
                    .map_or(1, |animation| animation.frame_count());
                bytes -= image.pixels.len() * count;
                frames -= count;
                evictions.push(*id);
            }
        }
        if bytes > self.max_bytes {
            return Err(AnimationError::TooLarge);
        }
        if frames > MAX_RETAINED_FRAMES {
            return Err(AnimationError::TooManyFrames);
        }
        Ok(evictions)
    }

    pub(crate) fn image_count(&self) -> usize {
        self.images.len()
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
    use super::{ImageFormat, ImageStore, MAX_STORED_IMAGES};
    use crate::parser::kitty_graphics::{KittyAnimationState, KittyBlendMode};
    use std::collections::HashSet;
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn limited_store(max_bytes: usize) -> ImageStore {
        let mut store = ImageStore::new(1);
        store.max_bytes = max_bytes;
        store
    }

    fn insert_pixel(store: &mut ImageStore, id: u64) {
        assert_eq!(
            store.store(&[255; 4], 1, 1, ImageFormat::Rgba, Some(id)),
            Some(id)
        );
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
            &KittyFrameUpload::default(),
            now,
        )
    }

    #[test]
    fn opacity_tracks_decoded_pixels_and_the_frame_selected_by_the_clock() {
        let now = Instant::now();
        let mut store = limited_store(16);
        store
            .store(&[10, 20, 30], 1, 1, ImageFormat::Rgb, Some(1))
            .unwrap();
        assert!(
            store.get(1).unwrap().opaque,
            "RGB conversion creates an opaque canvas"
        );
        store
            .store_animation_frame(
                1,
                &[90, 80, 70, 128],
                1,
                1,
                ImageFormat::Rgba,
                &KittyFrameUpload {
                    gap_ms: Some(10),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert!(
            store.get(1).unwrap().opaque,
            "appending a frame does not change stopped playback"
        );
        store
            .control_animation(
                1,
                &KittyAnimationControl {
                    state: Some(KittyAnimationState::Running),
                    gap_ms: Some(10),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        let visible = HashSet::from([1]);
        assert!(
            store
                .advance_animations(now + Duration::from_millis(10), &visible)
                .changed
        );
        assert!(!store.get(1).unwrap().opaque);
        assert_eq!(store.get(1).unwrap().pixels[3], 128);
        assert!(
            store
                .advance_animations(now + Duration::from_millis(20), &visible)
                .changed
        );
        assert!(store.get(1).unwrap().opaque);
        assert_eq!(store.get(1).unwrap().pixels[3], 255);
    }

    #[test]
    fn opacity_is_recomputed_after_frame_edits_composition_deletion_and_replacement() {
        let now = Instant::now();
        let mut store = limited_store(12);
        insert_pixel(&mut store, 1);
        store
            .store_animation_frame(
                1,
                &[10, 20, 30, 0],
                1,
                1,
                ImageFormat::Rgba,
                &KittyFrameUpload::default(),
                now,
            )
            .unwrap();
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
        assert!(!store.get(1).unwrap().opaque);
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
        assert!(store.get(1).unwrap().opaque);
        store
            .store_animation_frame(
                1,
                &[10, 20, 30, 0],
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
        assert!(!store.get(1).unwrap().opaque);
        store.delete_animation_frame(1, 1, false, now).unwrap();
        assert!(
            !store.get(1).unwrap().opaque,
            "root promotion retains the promoted frame's transparency"
        );
        assert!(store
            .store_animation_frame(
                1,
                &[255; 4],
                1,
                1,
                ImageFormat::Rgba,
                &KittyFrameUpload {
                    base_frame: NonZeroU32::new(9),
                    ..Default::default()
                },
                now
            )
            .is_err());
        assert!(
            !store.get(1).unwrap().opaque,
            "rejected uploads cannot change opacity metadata"
        );
        store
            .store(&[10, 20, 30], 1, 1, ImageFormat::Rgb, Some(1))
            .unwrap();
        assert!(store.get(1).unwrap().opaque);
    }

    #[test]
    fn delta_opacity_accounts_for_background_pixels_and_alpha_composition() {
        let now = Instant::now();
        let mut store = limited_store(24);
        store
            .store(
                &[10, 20, 30, 255, 40, 50, 60, 255],
                2,
                1,
                ImageFormat::Rgba,
                Some(1),
            )
            .unwrap();
        store
            .store_animation_frame(
                1,
                &[255; 4],
                1,
                1,
                ImageFormat::Rgba,
                &KittyFrameUpload::default(),
                now,
            )
            .unwrap();
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
        assert!(
            !store.get(1).unwrap().opaque,
            "unfilled delta canvas pixels are transparent"
        );
        store
            .store_animation_frame(
                1,
                &[90, 80, 70, 128],
                1,
                1,
                ImageFormat::Rgba,
                &KittyFrameUpload {
                    base_frame: NonZeroU32::new(1),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        store
            .control_animation(
                1,
                &KittyAnimationControl {
                    current_frame: NonZeroU32::new(3),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert!(
            store.get(1).unwrap().opaque,
            "alpha compositing over an opaque base remains opaque"
        );
        assert_eq!(store.get(1).unwrap().pixels[3], 255);
        assert_eq!(store.get(1).unwrap().pixels[7], 255);
    }

    #[test]
    fn animation_budget_counts_root_and_all_frames_and_never_evicts_own_image() {
        let now = Instant::now();
        let mut store = limited_store(12);
        insert_pixel(&mut store, 1);
        insert_pixel(&mut store, 2);
        insert_pixel(&mut store, 3);
        assert_eq!(append_pixel(&mut store, 1, 33, now), Ok(2));
        assert!(store.get(1).is_some());
        assert!(store.get(2).is_none());
        assert!(store.get(3).is_some());
        assert_eq!(store.total_bytes, 12);
        assert_eq!(store.retained_frames, 3);
        assert_eq!(append_pixel(&mut store, 1, 44, now), Ok(3));
        assert!(store.get(3).is_none());
        assert_eq!(
            append_pixel(&mut store, 1, 55, now),
            Err(AnimationError::TooLarge)
        );
        assert_eq!(store.animations[&1].frame_count(), 3);
        assert_eq!(store.total_bytes, 12);
        assert_eq!(store.retained_frames, 3);
    }

    #[test]
    fn invalid_frame_uploads_and_compositions_preserve_entire_cache_and_snapshots() {
        let now = Instant::now();
        let mut store = limited_store(12);
        insert_pixel(&mut store, 1);
        insert_pixel(&mut store, 2);
        append_pixel(&mut store, 1, 33, now).unwrap();
        let root = Arc::clone(&store.get(1).unwrap().pixels);
        let second = Arc::clone(&store.get(2).unwrap().pixels);
        let order = store.insertion_order.clone();
        let next_id = store.next_id;
        for params in [
            KittyFrameUpload {
                x: 1,
                ..Default::default()
            },
            KittyFrameUpload {
                base_frame: NonZeroU32::new(3),
                ..Default::default()
            },
            KittyFrameUpload {
                edit_frame: NonZeroU32::new(3),
                ..Default::default()
            },
        ] {
            assert!(store
                .store_animation_frame(1, &[0; 4], 1, 1, ImageFormat::Rgba, &params, now)
                .is_err());
        }
        assert!(store
            .store_animation_frame(
                1,
                &[0; 3],
                1,
                1,
                ImageFormat::Rgba,
                &KittyFrameUpload::default(),
                now
            )
            .is_err());
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
        assert_eq!(
            store.control_animation(
                1,
                &KittyAnimationControl {
                    state: Some(KittyAnimationState::Running),
                    current_frame: NonZeroU32::new(3),
                    ..Default::default()
                },
                now
            ),
            Err(AnimationError::FrameNotFound)
        );
        assert!(Arc::ptr_eq(&root, &store.get(1).unwrap().pixels));
        assert!(Arc::ptr_eq(&second, &store.get(2).unwrap().pixels));
        assert_eq!(store.insertion_order, order);
        assert_eq!(store.next_id, next_id);
        assert_eq!(store.total_bytes, 12);
        assert_eq!(store.retained_frames, 3);
        assert_eq!(store.animations[&1].frame_count(), 2);
    }

    #[test]
    fn editing_an_active_frame_replaces_its_arc_without_growing_cache_or_mutating_snapshots() {
        let now = Instant::now();
        let mut store = limited_store(8);
        insert_pixel(&mut store, 1);
        append_pixel(&mut store, 1, 33, now).unwrap();
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
        let snapshot = Arc::clone(&store.get(1).unwrap().pixels);
        let params = KittyFrameUpload {
            edit_frame: NonZeroU32::new(2),
            blend: KittyBlendMode::Overwrite,
            ..Default::default()
        };
        assert_eq!(
            store.store_animation_frame(
                1,
                &[11, 22, 33, 44],
                1,
                1,
                ImageFormat::Rgba,
                &params,
                now
            ),
            Ok(2)
        );
        assert_eq!(snapshot.as_ref(), &[33, 0, 0, 255]);
        assert_eq!(store.get(1).unwrap().pixels.as_ref(), &[11, 22, 33, 44]);
        assert_eq!(store.total_bytes, 8);
        assert_eq!(store.retained_frames, 2);
        assert_eq!(
            store.compose_animation_frame(
                1,
                &KittyFrameComposition {
                    source_frame: 1,
                    destination_frame: 2,
                    blend: KittyBlendMode::Overwrite,
                    ..Default::default()
                },
                now
            ),
            Ok(())
        );
        assert_eq!(store.get(1).unwrap().pixels.as_ref(), &[255; 4]);
        assert_eq!(store.total_bytes, 8);
    }

    #[test]
    fn replacement_and_eviction_release_every_animation_frame_and_clock() {
        let now = Instant::now();
        let mut store = limited_store(12);
        insert_pixel(&mut store, 1);
        append_pixel(&mut store, 1, 33, now).unwrap();
        append_pixel(&mut store, 1, 44, now).unwrap();
        insert_pixel(&mut store, 1);
        assert_eq!(store.total_bytes, 4);
        assert_eq!(store.retained_frames, 1);
        assert!(!store.animations.contains_key(&1));
        append_pixel(&mut store, 1, 55, now).unwrap();
        append_pixel(&mut store, 1, 66, now).unwrap();
        insert_pixel(&mut store, 2);
        assert!(store.get(1).is_none());
        assert!(!store.animations.contains_key(&1));
        assert_eq!(store.total_bytes, 4);
        assert_eq!(store.retained_frames, 1);
    }

    #[test]
    fn animation_frame_deletion_reclaims_budget_and_only_uppercase_removes_last_image() {
        let now = Instant::now();
        let mut store = limited_store(12);
        insert_pixel(&mut store, 1);
        append_pixel(&mut store, 1, 33, now).unwrap();
        append_pixel(&mut store, 1, 44, now).unwrap();
        store.delete_animation_frame(1, 0, false, now).unwrap();
        assert_eq!(store.get(1).unwrap().pixels.as_ref(), &[33, 0, 0, 255]);
        assert_eq!(store.total_bytes, 8);
        store
            .delete_animation_frame(1, u32::MAX, true, now)
            .unwrap();
        assert!(store.get(1).is_some());
        assert_eq!(store.total_bytes, 4);
        store.delete_animation_frame(1, 1, false, now).unwrap();
        assert!(store.get(1).is_some());
        store.delete_animation_frame(1, 1, true, now).unwrap();
        assert!(store.get(1).is_none());
        assert!(store.animations.is_empty());
        assert_eq!(store.total_bytes, 0);
        assert_eq!(store.retained_frames, 0);
    }

    #[test]
    fn static_images_need_no_animation_metadata_and_failed_commands_do_not_add_it() {
        let now = Instant::now();
        let mut store = limited_store(12);
        insert_pixel(&mut store, 1);
        assert!(store.animations.is_empty());
        assert_eq!(
            store.control_animation(
                1,
                &KittyAnimationControl {
                    current_frame: NonZeroU32::new(2),
                    ..Default::default()
                },
                now
            ),
            Err(AnimationError::FrameNotFound)
        );
        assert!(store.animations.is_empty());
        assert_eq!(
            store.control_animation(9, &KittyAnimationControl::default(), now),
            Err(AnimationError::ImageNotFound)
        );
        assert_eq!(
            append_pixel(&mut store, 9, 0, now),
            Err(AnimationError::ImageNotFound)
        );
        assert!(store.animations.is_empty());
    }

    #[test]
    fn invisible_animations_request_no_wakeups_and_catch_up_when_visible() {
        let now = Instant::now();
        let mut store = limited_store(8);
        insert_pixel(&mut store, 1);
        append_pixel(&mut store, 1, 33, now).unwrap();
        store
            .control_animation(
                1,
                &KittyAnimationControl {
                    state: Some(KittyAnimationState::Running),
                    gap_ms: Some(10),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        let empty = store.advance_animations(now + Duration::from_secs(500), &HashSet::new());
        assert!(!empty.changed);
        assert_eq!(empty.next_deadline, None);
        assert_eq!(store.get(1).unwrap().pixels.as_ref(), &[255; 4]);
        let visible =
            store.advance_animations(now + Duration::from_millis(500_020), &HashSet::from([1]));
        assert!(visible.changed);
        assert_eq!(
            visible.next_deadline,
            Some(now + Duration::from_millis(500_050))
        );
        assert_eq!(store.get(1).unwrap().pixels.as_ref(), &[33, 0, 0, 255]);
        let repeated =
            store.advance_animations(now + Duration::from_millis(500_020), &HashSet::from([1]));
        assert!(!repeated.changed);
        assert_eq!(repeated.next_deadline, visible.next_deadline);
    }

    #[test]
    fn per_image_and_global_frame_caps_include_roots() {
        let now = Instant::now();
        let mut store = ImageStore::new(1);
        insert_pixel(&mut store, 1);
        for frame in 2..=super::MAX_FRAMES_PER_IMAGE {
            assert_eq!(
                append_pixel(&mut store, 1, frame as u8, now),
                Ok(frame as u32)
            );
        }
        assert_eq!(
            append_pixel(&mut store, 1, 0, now),
            Err(AnimationError::TooManyFrames)
        );
        assert_eq!(store.retained_frames, super::MAX_FRAMES_PER_IMAGE);
        for id in 2..=(super::MAX_RETAINED_FRAMES - super::MAX_FRAMES_PER_IMAGE + 1) as u64 {
            insert_pixel(&mut store, id);
        }
        assert_eq!(store.retained_frames, super::MAX_RETAINED_FRAMES);
        // Root is the oldest entry, but appending to it must evict the oldest
        // other image. Free one frame so the per-image cap permits that append.
        store
            .delete_animation_frame(1, u32::MAX, false, now)
            .unwrap();
        insert_pixel(&mut store, 9999);
        append_pixel(&mut store, 1, 44, now).unwrap();
        assert!(store.get(1).is_some());
        assert!(store.get(2).is_none());
        assert_eq!(store.retained_frames, super::MAX_RETAINED_FRAMES);
        assert_eq!(store.total_bytes, super::MAX_RETAINED_FRAMES * 4);
    }

    #[test]
    fn evicts_the_oldest_image_at_the_exact_byte_budget() {
        let mut store = limited_store(8);
        insert_pixel(&mut store, 30);
        insert_pixel(&mut store, 10);
        assert_eq!(store.total_bytes, 8);

        insert_pixel(&mut store, 20);

        assert!(store.get(30).is_none());
        assert!(store.get(10).is_some());
        assert!(store.get(20).is_some());
        assert_eq!(store.image_count(), 2);
        assert_eq!(store.total_bytes, 8);
    }

    #[test]
    fn replacement_credits_old_pixels_before_evicting_other_images() {
        let mut store = limited_store(12);
        insert_pixel(&mut store, 1);
        insert_pixel(&mut store, 2);

        assert_eq!(
            store.store(&[127; 8], 1, 2, ImageFormat::Rgba, Some(2)),
            Some(2)
        );

        assert!(store.get(1).is_some());
        let replacement = store.get(2).unwrap();
        assert_eq!((replacement.width, replacement.height), (1, 2));
        assert_eq!(replacement.pixels.as_ref(), &[127; 8]);
        assert_eq!(store.image_count(), 2);
        assert_eq!(store.total_bytes, 12);
    }

    #[test]
    fn replacement_evicts_only_necessary_entries_and_becomes_newest() {
        let mut store = limited_store(12);
        for id in 1..=3 {
            insert_pixel(&mut store, id);
        }

        assert_eq!(
            store.store(&[127; 8], 1, 2, ImageFormat::Rgba, Some(2)),
            Some(2)
        );

        assert!(store.get(1).is_none());
        assert!(store.get(3).is_some());
        assert_eq!(store.total_bytes, 12);
        insert_pixel(&mut store, 4);
        assert!(store.get(3).is_none());
        assert!(store.get(2).is_some());
        assert!(store.get(4).is_some());
    }

    #[test]
    fn tiny_images_obey_the_entry_cap_and_replacement_does_not_consume_a_slot() {
        let mut store = ImageStore::new(1);
        for id in 1..=MAX_STORED_IMAGES as u64 {
            insert_pixel(&mut store, id);
        }
        insert_pixel(&mut store, 1);
        assert_eq!(store.image_count(), MAX_STORED_IMAGES);
        assert!(store.get(2).is_some());

        insert_pixel(&mut store, MAX_STORED_IMAGES as u64 + 1);

        assert_eq!(store.image_count(), MAX_STORED_IMAGES);
        assert!(store.get(1).is_some());
        assert!(store.get(2).is_none());
        assert_eq!(store.total_bytes, MAX_STORED_IMAGES * 4);
        assert_eq!(store.insertion_order.len(), MAX_STORED_IMAGES);
    }

    #[test]
    fn rejected_uploads_preserve_pixels_accounting_and_next_id() {
        let mut store = limited_store(8);
        insert_pixel(&mut store, 1);
        let existing = Arc::clone(&store.get(1).unwrap().pixels);
        let next_id = store.next_id;
        for (pixels, width, height) in [
            (&[0; 12][..], 3, 1),
            (&[0; 3][..], 1, 1),
            (&[][..], u32::MAX, u32::MAX),
        ] {
            assert!(store
                .store(pixels, width, height, ImageFormat::Rgba, Some(1))
                .is_none());
        }

        assert!(Arc::ptr_eq(&existing, &store.get(1).unwrap().pixels));
        assert_eq!(store.next_id, next_id);
        assert_eq!(store.image_count(), 1);
        assert_eq!(store.total_bytes, 4);
    }

    #[test]
    fn zero_ids_and_zero_dimensions_are_rejected_without_allocating_ids() {
        let mut store = ImageStore::new(1);
        assert!(store
            .store(&[255; 4], 1, 1, ImageFormat::Rgba, Some(0))
            .is_none());
        assert!(store
            .store(&[255; 4], 0, 1, ImageFormat::Rgba, None)
            .is_none());
        assert!(store
            .store(&[255; 3], 1, 0, ImageFormat::Rgb, None)
            .is_none());
        assert_eq!(store.image_count(), 0);
        assert_eq!(store.next_id(), 1);
    }

    #[test]
    fn id_wrap_skips_zero_and_occupied_ids() {
        let mut store = ImageStore::new(1);
        insert_pixel(&mut store, 1);
        insert_pixel(&mut store, u64::MAX);

        assert_eq!(
            store.store(&[255; 4], 1, 1, ImageFormat::Rgba, None),
            Some(2)
        );
        assert_eq!(store.next_id(), 3);
    }

    #[test]
    fn converts_rgb_to_opaque_rgba_and_ignores_trailing_raw_bytes() {
        let mut store = ImageStore::new(1);
        let id = store
            .store(&[10, 20, 30, 40, 50, 60, 99], 2, 1, ImageFormat::Rgb, None)
            .unwrap();
        assert_eq!(
            store.get(id).unwrap().pixels.as_ref(),
            &[10, 20, 30, 255, 40, 50, 60, 255]
        );
        let id = store
            .store(&[1, 2, 3, 4, 99], 1, 1, ImageFormat::Rgba, None)
            .unwrap();
        assert_eq!(store.get(id).unwrap().pixels.as_ref(), &[1, 2, 3, 4]);
        assert_eq!(store.total_bytes, 12);
    }

    #[test]
    fn png_uses_embedded_dimensions_and_preserves_alpha() {
        let mut encoded = Vec::new();
        let pixels = [255, 0, 0, 255, 0, 255, 0, 128];
        {
            let mut encoder = png::Encoder::new(&mut encoded, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&pixels)
                .unwrap();
        }
        let mut store = ImageStore::new(1);
        let id = store.store(&encoded, 0, 0, ImageFormat::Png, None).unwrap();
        let image = store.get(id).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixels.as_ref(), &pixels);
        assert_eq!(store.total_bytes, 8);
    }

    #[test]
    fn removal_reclaims_budget_and_shared_pixels_survive_until_the_renderer_drops_them() {
        let mut store = limited_store(4);
        insert_pixel(&mut store, 1);
        let pixels = Arc::clone(&store.get(1).unwrap().pixels);
        store.remove(1);
        store.remove(1);
        assert_eq!(store.total_bytes, 0);
        assert_eq!(store.image_count(), 0);
        assert!(store.insertion_order.is_empty());
        assert_eq!(pixels.as_ref(), &[255; 4]);
        insert_pixel(&mut store, 2);
        assert_eq!(store.total_bytes, 4);
    }

    #[test]
    fn zero_budget_rejects_images_and_megabyte_conversion_does_not_overflow() {
        let mut store = ImageStore::new(0);
        assert!(store
            .store(&[255; 4], 1, 1, ImageFormat::Rgba, None)
            .is_none());
        assert_eq!(store.image_count(), 0);
        let mut huge_limit = ImageStore::new(usize::MAX);
        insert_pixel(&mut huge_limit, 1);
    }
}
