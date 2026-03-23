use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::*;
use std::collections::HashMap;
use std::time::Instant;

pub type ImageId = u32;

pub struct StoredImage {
    #[allow(dead_code)]
    pub id: ImageId,
    pub texture: Retained<ProtocolObject<dyn MTLTexture>>,
    pub width: u32,
    pub height: u32,
    pub byte_size: usize,
    pub created_at: Instant,
}

pub enum ImageFormat {
    Rgba,
    Rgb,
    Png,
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
        if width == 0 || height == 0 {
            return None;
        }

        // Convert to RGBA
        let rgba_data = match format {
            ImageFormat::Rgba => data.to_vec(),
            ImageFormat::Rgb => {
                let pixel_count = (width * height) as usize;
                if data.len() < pixel_count * 3 {
                    log::warn!("RGB image data too short: {} < {}", data.len(), pixel_count * 3);
                    return None;
                }
                let mut rgba = vec![255u8; pixel_count * 4];
                for i in 0..pixel_count {
                    rgba[i * 4] = data[i * 3];
                    rgba[i * 4 + 1] = data[i * 3 + 1];
                    rgba[i * 4 + 2] = data[i * 3 + 2];
                    // alpha = 255 (already set)
                }
                rgba
            }
            ImageFormat::Png => {
                match decode_png(data) {
                    Some((decoded, pw, ph)) => {
                        if pw != width || ph != height {
                            log::trace!("PNG dimensions {pw}x{ph} differ from declared {width}x{height}");
                            // Use the actual PNG dimensions
                            return self.store_rgba(&decoded, pw, ph, requested_id);
                        }
                        decoded
                    }
                    None => {
                        log::warn!("Failed to decode PNG image data");
                        return None;
                    }
                }
            }
        };

        self.store_rgba(&rgba_data, width, height, requested_id)
    }

    fn store_rgba(
        &mut self,
        rgba_data: &[u8],
        width: u32,
        height: u32,
        requested_id: Option<ImageId>,
    ) -> Option<ImageId> {
        let expected_size = (width * height * 4) as usize;
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

        // Evict if needed
        while self.total_bytes + expected_size > self.max_bytes && !self.images.is_empty() {
            self.evict_lru();
        }

        let texture = self.create_texture(width, height, rgba_data)?;
        let id = requested_id.unwrap_or_else(|| {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            id
        });

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
    ) -> Option<Retained<ProtocolObject<dyn MTLTexture>>> {
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

    pub fn contains(&self, id: ImageId) -> bool {
        self.images.contains_key(&id)
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

    /// Assign a new unique ID (for Sixel images which don't have their own IDs)
    pub fn next_id(&mut self) -> ImageId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }
}

fn decode_png(data: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let width = info.width;
    let height = info.height;

    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => {
            let pixel_count = (width * height) as usize;
            let mut rgba = vec![255u8; pixel_count * 4];
            for i in 0..pixel_count {
                rgba[i * 4] = buf[i * 3];
                rgba[i * 4 + 1] = buf[i * 3 + 1];
                rgba[i * 4 + 2] = buf[i * 3 + 2];
            }
            rgba
        }
        png::ColorType::GrayscaleAlpha => {
            let pixel_count = (width * height) as usize;
            let mut rgba = vec![0u8; pixel_count * 4];
            for i in 0..pixel_count {
                let gray = buf[i * 2];
                let alpha = buf[i * 2 + 1];
                rgba[i * 4] = gray;
                rgba[i * 4 + 1] = gray;
                rgba[i * 4 + 2] = gray;
                rgba[i * 4 + 3] = alpha;
            }
            rgba
        }
        png::ColorType::Grayscale => {
            let pixel_count = (width * height) as usize;
            let mut rgba = vec![255u8; pixel_count * 4];
            for i in 0..pixel_count {
                let gray = buf[i];
                rgba[i * 4] = gray;
                rgba[i * 4 + 1] = gray;
                rgba[i * 4 + 2] = gray;
            }
            rgba
        }
        png::ColorType::Indexed => {
            log::warn!("Indexed PNG not supported");
            return None;
        }
    };

    Some((rgba, width, height))
}
