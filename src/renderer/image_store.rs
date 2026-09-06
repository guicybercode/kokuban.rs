use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::*;
use std::collections::HashMap;
use std::time::Instant;

use crate::graphics::{next_available_image_id, ImageId};
use super::image_decode::{prepare_image, rgba_byte_len};
pub use super::image_decode::ImageFormat;

type MetalTexture = Retained<ProtocolObject<dyn MTLTexture>>;

pub struct StoredImage {
    #[allow(dead_code)]
    pub id: ImageId,
    pub texture: MetalTexture,
    pub width: u32,
    pub height: u32,
    pub byte_size: usize,
    pub created_at: Instant,
}

pub struct ImageStore {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    images: HashMap<ImageId, StoredImage>,
    next_id: ImageId,
    total_bytes: usize,
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
            next_id: 1,
            total_bytes: 0,
            max_bytes: max_mb * 1024 * 1024,
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
        if let Some(old) = self.images.remove(&id) {
            self.total_bytes -= old.byte_size;
        }

        while self.total_bytes > retained_budget && !self.images.is_empty() {
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
            },
        );
        self.total_bytes += expected_size;
        debug_assert!(self.total_bytes <= self.max_bytes);

        log::trace!(
            "Stored image id={id} size={width}x{height} bytes={expected_size} total={}MB",
            self.total_bytes / (1024 * 1024)
        );

        Some(id)
    }

    fn create_texture(
        &self,
        width: u32,
        height: u32,
        rgba_data: &[u8],
    ) -> Option<MetalTexture> {
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
            let bytes_ptr =
                std::ptr::NonNull::new(rgba_data.as_ptr() as *mut std::ffi::c_void)?;
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
            log::trace!("Removed image id={id}");
        }
    }

    pub fn get(&self, id: ImageId) -> Option<&StoredImage> {
        self.images.get(&id)
    }

    pub(crate) fn image_count(&self) -> usize {
        self.images.len()
    }

    fn evict_lru(&mut self) {
        if let Some((&oldest_id, _)) = self
            .images
            .iter()
            .min_by_key(|(_, img)| img.created_at)
        {
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
    use super::{ImageFormat, ImageStore};
    use objc2_metal::MTLCreateSystemDefaultDevice;
    use std::time::{Duration, Instant};

    fn image_store_with_byte_limit(max_bytes: usize) -> Option<ImageStore> {
        let device = MTLCreateSystemDefaultDevice()?;
        let mut store = ImageStore::new(device, 1);
        store.max_bytes = max_bytes;
        Some(store)
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

        let result = store.store_rgba_with(
            &[63; 8],
            1,
            2,
            None,
            |cache, _, _, _| {
                assert_eq!(cache.image_count(), 2);
                assert_eq!(cache.total_bytes, 8);
                None
            },
        );

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
