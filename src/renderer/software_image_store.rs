use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::image_decode::prepare_image;
pub use super::image_decode::ImageFormat;
use crate::graphics::{next_available_image_id, ImageId};

const MAX_STORED_IMAGES: usize = 4096;

pub struct StoredImage {
    pub pixels: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
}

/// CPU images shared with the software renderer without copying pixel buffers.
/// Both pixels and entry count are bounded, including streams of tiny images.
pub struct ImageStore {
    images: HashMap<ImageId, StoredImage>,
    insertion_order: VecDeque<ImageId>,
    next_id: ImageId,
    total_bytes: usize,
    max_bytes: usize,
    max_images: usize,
}

impl ImageStore {
    pub fn new(max_mb: usize) -> Self {
        Self {
            images: HashMap::new(),
            insertion_order: VecDeque::new(),
            next_id: 1,
            total_bytes: 0,
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
        let (pixels, width, height) = prepare_image(data, width, height, format, self.max_bytes)?;
        let byte_size = pixels.len();
        let retained_budget = self.max_bytes.checked_sub(byte_size)?;
        let pixels = Arc::from(pixels);
        let id = requested_id.unwrap_or_else(|| self.next_id());
        if id >= self.next_id {
            self.next_id = id.wrapping_add(1).max(1);
        }

        // Credit a replacement before deciding which other images to evict.
        // Validation finishes first so rejected uploads preserve live images.
        self.remove(id);
        while self.total_bytes > retained_budget || self.images.len() >= self.max_images {
            let oldest_id = self.insertion_order.front().copied()?;
            self.remove(oldest_id);
        }
        self.images.insert(
            id,
            StoredImage {
                pixels,
                width,
                height,
            },
        );
        self.insertion_order.push_back(id);
        // The retained budget guarantees this addition cannot overflow.
        self.total_bytes += byte_size;
        debug_assert!(self.total_bytes <= self.max_bytes);
        debug_assert!(self.images.len() <= self.max_images);
        Some(id)
    }

    pub fn get(&self, id: ImageId) -> Option<&StoredImage> {
        self.images.get(&id)
    }

    pub fn remove(&mut self, id: ImageId) {
        if let Some(image) = self.images.remove(&id) {
            self.total_bytes -= image.pixels.len();
            self.insertion_order.retain(|stored_id| *stored_id != id);
        }
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
    use super::{ImageFormat, ImageStore, MAX_STORED_IMAGES};
    use std::sync::Arc;

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
