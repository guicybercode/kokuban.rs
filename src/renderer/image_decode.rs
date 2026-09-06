pub enum ImageFormat {
    Rgba,
    Rgb,
    Png,
}

/// Decode image data for a capability query without touching the cache or GPU.
pub(super) fn probe_image_data(
    data: &[u8],
    width: u32,
    height: u32,
    format: ImageFormat,
    max_bytes: usize,
) -> bool {
    prepare_image(data, width, height, format, max_bytes).is_some()
}

pub(super) fn prepare_image(
    data: &[u8],
    width: u32,
    height: u32,
    format: ImageFormat,
    max_bytes: usize,
) -> Option<(Vec<u8>, u32, u32)> {
    match format {
        ImageFormat::Rgba => {
            let rgba_len = rgba_byte_len(width, height)?;
            if rgba_len > max_bytes || data.len() < rgba_len {
                return None;
            }
            Some((data[..rgba_len].to_vec(), width, height))
        }
        ImageFormat::Rgb => {
            let pixel_count = pixel_count(width, height)?;
            let rgb_len = pixel_count.checked_mul(3)?;
            let rgba_len = pixel_count.checked_mul(4)?;
            if data.len() < rgb_len {
                log::warn!("RGB image data too short: {} < {rgb_len}", data.len());
                return None;
            }
            if rgba_len > max_bytes {
                return None;
            }

            let mut rgba = vec![255u8; rgba_len];
            for (source, target) in data[..rgb_len]
                .chunks_exact(3)
                .zip(rgba.chunks_exact_mut(4))
            {
                target[..3].copy_from_slice(source);
            }
            Some((rgba, width, height))
        }
        ImageFormat::Png => match decode_png(data, max_bytes) {
            Some((decoded, png_width, png_height)) => {
                if width != 0 && height != 0 && (png_width != width || png_height != height) {
                    log::trace!(
                        "PNG dimensions {png_width}x{png_height} differ from declared {width}x{height}"
                    );
                }
                Some((decoded, png_width, png_height))
            }
            None => {
                log::warn!("Failed to decode PNG image data");
                None
            }
        },
    }
}

fn pixel_count(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }

    usize::try_from(u64::from(width).checked_mul(u64::from(height))?).ok()
}

pub(super) fn rgba_byte_len(width: u32, height: u32) -> Option<usize> {
    pixel_count(width, height)?.checked_mul(4)
}

pub(crate) fn decode_png(data: &[u8], max_bytes: usize) -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    decoder.set_limits(png::Limits { bytes: max_bytes });
    let mut reader = decoder.read_info().ok()?;
    let output_size = reader.output_buffer_size();
    if output_size > max_bytes {
        return None;
    }
    let mut buf = vec![0u8; output_size];
    let info = reader.next_frame(&mut buf).ok()?;
    let width = info.width;
    let height = info.height;
    let pixel_count = pixel_count(width, height)?;
    let rgba_len = rgba_byte_len(width, height)?;
    if rgba_len > max_bytes {
        return None;
    }

    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => {
            let mut rgba = vec![255u8; rgba_len];
            for i in 0..pixel_count {
                rgba[i * 4] = buf[i * 3];
                rgba[i * 4 + 1] = buf[i * 3 + 1];
                rgba[i * 4 + 2] = buf[i * 3 + 2];
            }
            rgba
        }
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = vec![0u8; rgba_len];
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
            let mut rgba = vec![255u8; rgba_len];
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

#[cfg(test)]
mod tests {
    use super::{prepare_image, ImageFormat};

    fn rgba_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("PNG header should encode");
            writer
                .write_image_data(pixels)
                .expect("PNG pixels should encode");
        }
        encoded
    }

    #[test]
    fn png_uses_embedded_dimensions_when_protocol_omits_them() {
        let pixels = [255, 0, 0, 255, 0, 255, 0, 128];
        let encoded = rgba_png(2, 1, &pixels);

        let (decoded, width, height) =
            prepare_image(&encoded, 0, 0, ImageFormat::Png, 1024).expect("PNG should decode");

        assert_eq!((width, height), (2, 1));
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn raw_formats_still_require_dimensions() {
        assert!(prepare_image(&[0; 4], 0, 1, ImageFormat::Rgba, 1024).is_none());
        assert!(prepare_image(&[0; 3], 1, 0, ImageFormat::Rgb, 1024).is_none());
    }

    #[test]
    fn rejects_overflowing_raw_dimensions() {
        assert!(prepare_image(&[], u32::MAX, u32::MAX, ImageFormat::Rgba, usize::MAX).is_none());
    }

    #[test]
    fn rejects_png_larger_than_the_configured_limit() {
        let pixels = [255; 16];
        let encoded = rgba_png(2, 2, &pixels);

        assert!(prepare_image(&encoded, 0, 0, ImageFormat::Png, 8).is_none());
    }
}
