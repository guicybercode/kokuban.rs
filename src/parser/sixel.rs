/// Decoded Sixel image as RGBA pixels.
#[derive(Debug, Clone)]
pub struct SixelImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA
}

/// Decode a Sixel data stream into an RGBA image.
///
/// The `data` parameter is the raw bytes between DCS q ... ST,
/// starting after the 'q' character.
#[cfg(test)]
fn decode_sixel(data: &[u8]) -> Result<SixelImage, SixelError> {
    decode_sixel_with_limits(data, SixelLimits::default())
}

pub(crate) fn decode_sixel_with_byte_limit(
    data: &[u8],
    max_rgba_bytes: usize,
) -> Result<SixelImage, SixelError> {
    let mut limits = SixelLimits::default();
    limits.max_rgba_bytes = limits.max_rgba_bytes.min(max_rgba_bytes);
    limits.max_operations = limits
        .max_operations
        .min((limits.max_rgba_bytes / 4) as u64);
    decode_sixel_with_limits(data, limits)
}

fn decode_sixel_with_limits(data: &[u8], limits: SixelLimits) -> Result<SixelImage, SixelError> {
    let max_rgba_bytes = limits.max_rgba_bytes;
    let mut decoder = SixelDecoder::with_limits(limits);
    decoder.decode(data)?;
    let image = decoder.finish()?;
    if image.pixels.capacity() > max_rgba_bytes {
        return Err(SixelError::TooLarge);
    }
    Ok(image)
}

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_DIMENSION: u32 = 4096;
pub(crate) const MAX_RGBA_BYTES: usize = 32 * 1024 * 1024;
const MAX_REPEAT: u32 = 32_766;
const MAX_OPERATIONS: u64 = (MAX_RGBA_BYTES / 4) as u64;
// Geometric growth normally touches only a few multiples of the final image.
// Four times the image budget accommodates normal growth while tightly
// bounding streams that repeatedly resize near the limit.
const MAX_ALLOCATION_WORK_MULTIPLIER: u64 = 4;
const MIN_ALLOCATION_WORK_BYTES: u64 = 64 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SixelError {
    #[error("invalid sixel data")]
    InvalidData,
    #[error("image too large")]
    TooLarge,
}

#[derive(Clone, Copy)]
struct SixelLimits {
    max_input_bytes: usize,
    max_dimension: u32,
    max_rgba_bytes: usize,
    max_repeat: u32,
    max_operations: u64,
}

impl Default for SixelLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: MAX_INPUT_BYTES,
            max_dimension: MAX_DIMENSION,
            max_rgba_bytes: MAX_RGBA_BYTES,
            max_repeat: MAX_REPEAT,
            max_operations: MAX_OPERATIONS,
        }
    }
}

struct SixelDecoder {
    width: u32,
    allocated_height: u32,
    cursor_x: u32,
    cursor_y: u32,
    max_x: u32,
    max_y: u32,
    raster_width: u32,
    raster_height: u32,
    current_color: u16,
    palette: Vec<(u8, u8, u8)>,
    pixels: Vec<u8>, // RGBA, width×height
    operations: u64,
    allocation_work: u64,
    limits: SixelLimits,
    #[cfg(test)]
    reallocations: usize,
}

impl SixelDecoder {
    fn with_limits(limits: SixelLimits) -> Self {
        // Initialize with a default VGA palette (first 16 colors)
        let mut palette = vec![(0u8, 0u8, 0u8); 256];
        let default_colors: [(u8, u8, u8); 16] = [
            (0, 0, 0),
            (205, 49, 49),
            (13, 188, 121),
            (229, 229, 16),
            (36, 114, 200),
            (188, 63, 188),
            (17, 168, 205),
            (229, 229, 229),
            (102, 102, 102),
            (241, 76, 76),
            (35, 209, 139),
            (245, 245, 67),
            (59, 142, 234),
            (214, 112, 214),
            (41, 184, 219),
            (255, 255, 255),
        ];
        for (i, &c) in default_colors.iter().enumerate() {
            palette[i] = c;
        }

        Self {
            width: 0,
            allocated_height: 0,
            cursor_x: 0,
            cursor_y: 0,
            max_x: 0,
            max_y: 0,
            raster_width: 0,
            raster_height: 0,
            current_color: 0,
            palette,
            pixels: Vec::new(),
            operations: 0,
            allocation_work: 0,
            limits,
            #[cfg(test)]
            reallocations: 0,
        }
    }

    fn decode(&mut self, data: &[u8]) -> Result<(), SixelError> {
        if data.len() > self.limits.max_input_bytes {
            return Err(SixelError::TooLarge);
        }

        let mut i = 0;
        let mut raster_attributes_allowed = true;

        while i < data.len() {
            let b = data[i];
            match b {
                // Raster attributes: "Pan;Pad;Ph;Pv".
                b'"' => {
                    i += 1;
                    i = self.parse_raster_attributes(data, i, raster_attributes_allowed)?;
                    raster_attributes_allowed = false;
                }
                // Color introducer
                b'#' => {
                    raster_attributes_allowed = false;
                    i += 1;
                    i = self.parse_color(data, i)?;
                }
                // Carriage return (go to start of current sixel band)
                b'$' => {
                    raster_attributes_allowed = false;
                    self.cursor_x = 0;
                    i += 1;
                }
                // Newline (move down 6 pixels, go to start)
                b'-' => {
                    raster_attributes_allowed = false;
                    self.cursor_x = 0;
                    self.cursor_y = self
                        .cursor_y
                        .checked_add(6)
                        .filter(|&y| y <= self.limits.max_dimension)
                        .ok_or(SixelError::TooLarge)?;
                    i += 1;
                }
                // Repeat introducer
                b'!' => {
                    raster_attributes_allowed = false;
                    i += 1;
                    let count_start = i;
                    let (count, next_i) = parse_bounded_number(data, i)?;
                    if next_i == count_start {
                        return Err(SixelError::InvalidData);
                    }
                    i = next_i;
                    if count == 0 {
                        return Err(SixelError::InvalidData);
                    }
                    if count > self.limits.max_repeat {
                        return Err(SixelError::TooLarge);
                    }
                    let sixel = data.get(i).copied().ok_or(SixelError::InvalidData)?;
                    if !(0x3F..=0x7E).contains(&sixel) {
                        return Err(SixelError::InvalidData);
                    }

                    self.put_sixels(sixel - 0x3F, count)?;
                    i += 1;
                }
                // Sixel data characters (0x3F to 0x7E)
                0x3F..=0x7E => {
                    raster_attributes_allowed = false;
                    let sixel_bits = b - 0x3F;
                    self.put_sixels(sixel_bits, 1)?;
                    i += 1;
                }
                _ => {
                    raster_attributes_allowed = false;
                    i += 1; // Skip unknown bytes
                }
            }
        }

        Ok(())
    }

    fn parse_raster_attributes(
        &mut self,
        data: &[u8],
        mut i: usize,
        apply: bool,
    ) -> Result<usize, SixelError> {
        // Format: "Pan;Pad;Ph;Pv" where Ph=pixel width, Pv=pixel height
        let mut params = [0u32; 4];
        let mut param_count = 0usize;

        loop {
            let (value, next_i) = parse_saturating_number(data, i);
            if param_count < params.len() {
                params[param_count] = value;
            }
            param_count += 1;
            i = next_i;

            if data.get(i) != Some(&b';') {
                break;
            }
            i += 1;
        }

        // params[2] = width hint, params[3] = height hint. Additional
        // parameters are ignored, as on DEC-compatible implementations.
        if apply && param_count >= 3 {
            let raster_width = params[2];
            let raster_height = if param_count >= 4 { params[3] } else { 0 };
            if raster_width > 0 || raster_height > 0 {
                self.checked_rgba_len(raster_width.max(1), raster_height.max(1))?;
            }
            self.raster_width = raster_width;
            self.raster_height = raster_height;
        }

        Ok(i)
    }

    fn parse_color(&mut self, data: &[u8], mut i: usize) -> Result<usize, SixelError> {
        // Parse: N[;type;v1;v2;v3]
        let (color_num, next_i) = parse_bounded_number(data, i)?;
        i = next_i;

        if color_num > 255 {
            return Ok(i);
        }

        if i < data.len() && data[i] == b';' {
            // Color definition
            i += 1;
            let (color_type, next_i) = parse_bounded_number(data, i)?;
            i = next_i;
            if i < data.len() && data[i] == b';' {
                i += 1;
            }
            let (v1, next_i) = parse_bounded_number(data, i)?;
            i = next_i;
            if i < data.len() && data[i] == b';' {
                i += 1;
            }
            let (v2, next_i) = parse_bounded_number(data, i)?;
            i = next_i;
            if i < data.len() && data[i] == b';' {
                i += 1;
            }
            let (v3, next_i) = parse_bounded_number(data, i)?;
            i = next_i;

            match color_type {
                2 => {
                    // RGB (percentages 0-100)
                    let r = ((v1 as f32 / 100.0) * 255.0) as u8;
                    let g = ((v2 as f32 / 100.0) * 255.0) as u8;
                    let b = ((v3 as f32 / 100.0) * 255.0) as u8;
                    self.palette[color_num as usize] = (r, g, b);
                }
                1 => {
                    // HLS to RGB conversion
                    let (r, g, b) = hls_to_rgb(v1, v2, v3);
                    self.palette[color_num as usize] = (r, g, b);
                }
                _ => {}
            }
        }

        self.current_color = color_num as u16;
        Ok(i)
    }

    fn put_sixels(&mut self, bits: u8, count: u32) -> Result<(), SixelError> {
        let needed_w = self
            .cursor_x
            .checked_add(count)
            .ok_or(SixelError::TooLarge)?;
        let band_bottom = self.cursor_y.checked_add(6).ok_or(SixelError::TooLarge)?;
        let painted_height = if bits == 0 {
            0
        } else {
            u8::BITS - bits.leading_zeros()
        };
        let painted_bottom = self
            .cursor_y
            .checked_add(painted_height)
            .ok_or(SixelError::TooLarge)?;
        let needed_h = if self.raster_height > self.cursor_y
            && self.raster_height < band_bottom
            && painted_bottom <= self.raster_height
        {
            self.raster_height
        } else {
            band_bottom
        };
        let operations = self
            .operations
            .checked_add(u64::from(count))
            .filter(|&operations| operations <= self.limits.max_operations)
            .ok_or(SixelError::TooLarge)?;

        // Validate every budget before allocating or expanding the repeat.
        self.checked_rgba_len(needed_w.max(1), needed_h.max(1))?;
        self.ensure_size(needed_w.max(1), needed_h.max(1))?;

        let (r, g, b) = self.palette[self.current_color as usize];
        for x in self.cursor_x..needed_w {
            for bit in 0..6u32 {
                if bits & (1 << bit) == 0 {
                    continue;
                }

                let py = self.cursor_y + bit;
                let idx =
                    usize::try_from((u64::from(py) * u64::from(self.width) + u64::from(x)) * 4)
                        .map_err(|_| SixelError::TooLarge)?;
                self.pixels[idx] = r;
                self.pixels[idx + 1] = g;
                self.pixels[idx + 2] = b;
                self.pixels[idx + 3] = 255;
            }
        }

        self.cursor_x = needed_w;
        self.max_x = self.max_x.max(needed_w);
        self.max_y = self.max_y.max(needed_h);
        self.operations = operations;
        Ok(())
    }

    fn ensure_size(&mut self, required_w: u32, required_h: u32) -> Result<(), SixelError> {
        if required_w <= self.width && required_h <= self.allocated_height {
            return Ok(());
        }

        // Allocation capacity is not part of the logical canvas. Preserve
        // spare capacity on the axis that is not growing whenever it still
        // fits the byte budget; alternating width/height growth is common in
        // Sixel streams and must not force a reallocation for every band.
        let exact_w = required_w.max(self.max_x).max(1);
        let exact_h = required_h.max(self.max_y).max(1);
        self.checked_rgba_len(exact_w, exact_h)?;

        let width_grows = required_w > self.width;
        let height_grows = required_h > self.allocated_height;
        let max_pixels = (self.limits.max_rgba_bytes / 4) as u64;

        let (new_w, new_h) = match (width_grows, height_grows) {
            (true, false) => {
                let desired_w = grow_dimension(self.width, exact_w, self.limits.max_dimension);
                fit_grown_axis(
                    desired_w,
                    self.allocated_height,
                    exact_w,
                    exact_h,
                    max_pixels,
                )
            }
            (false, true) => {
                let desired_h =
                    grow_dimension(self.allocated_height, exact_h, self.limits.max_dimension);
                let (new_h, new_w) =
                    fit_grown_axis(desired_h, self.width, exact_h, exact_w, max_pixels);
                (new_w, new_h)
            }
            (true, true) => {
                let grown_w = grow_dimension(self.width, exact_w, self.limits.max_dimension);
                let grown_h =
                    grow_dimension(self.allocated_height, exact_h, self.limits.max_dimension);
                if self.checked_rgba_len(grown_w, grown_h).is_ok() {
                    (grown_w, grown_h)
                } else {
                    (exact_w, exact_h)
                }
            }
            (false, false) => return Ok(()),
        };
        let new_size = self.checked_rgba_len(new_w, new_h)?;
        let copy_width = self.max_x.min(self.width).min(new_w);
        let copy_height = self.max_y.min(self.allocated_height).min(new_h);
        let copied_bytes = u64::from(copy_width)
            .checked_mul(u64::from(copy_height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(SixelError::TooLarge)?;
        let zeroed_bytes = u64::try_from(new_size).map_err(|_| SixelError::TooLarge)?;
        let allocation_work_limit = u64::try_from(self.limits.max_rgba_bytes)
            .ok()
            .and_then(|bytes| bytes.checked_mul(MAX_ALLOCATION_WORK_MULTIPLIER))
            .map(|bytes| bytes.max(MIN_ALLOCATION_WORK_BYTES))
            .ok_or(SixelError::TooLarge)?;
        let next_allocation_work = self
            .allocation_work
            .checked_add(zeroed_bytes)
            .and_then(|work| work.checked_add(copied_bytes))
            .filter(|&work| work <= allocation_work_limit)
            .ok_or(SixelError::TooLarge)?;

        // Charge both initialization and copying before requesting memory, so
        // rejected resize churn performs no part of the over-budget work.
        let mut new_pixels = Vec::new();
        new_pixels
            .try_reserve_exact(new_size)
            .map_err(|_| SixelError::TooLarge)?;
        new_pixels.resize(new_size, 0);

        if copy_width > 0 && copy_height > 0 {
            let row_len =
                usize::try_from(u64::from(copy_width) * 4).map_err(|_| SixelError::TooLarge)?;
            for row in 0..copy_height {
                let src_start = usize::try_from(u64::from(row) * u64::from(self.width) * 4)
                    .map_err(|_| SixelError::TooLarge)?;
                let dst_start = usize::try_from(u64::from(row) * u64::from(new_w) * 4)
                    .map_err(|_| SixelError::TooLarge)?;
                new_pixels[dst_start..dst_start + row_len]
                    .copy_from_slice(&self.pixels[src_start..src_start + row_len]);
            }
        }

        self.width = new_w;
        self.allocated_height = new_h;
        self.pixels = new_pixels;
        self.allocation_work = next_allocation_work;
        #[cfg(test)]
        {
            self.reallocations += 1;
        }
        Ok(())
    }

    fn checked_rgba_len(&self, width: u32, height: u32) -> Result<usize, SixelError> {
        if width == 0
            || height == 0
            || width > self.limits.max_dimension
            || height > self.limits.max_dimension
        {
            return Err(SixelError::TooLarge);
        }

        let bytes = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .filter(|&bytes| bytes <= self.limits.max_rgba_bytes as u64)
            .ok_or(SixelError::TooLarge)?;
        usize::try_from(bytes).map_err(|_| SixelError::TooLarge)
    }

    fn finish(mut self) -> Result<SixelImage, SixelError> {
        if self.max_x == 0 && self.max_y == 0 && self.raster_width == 0 && self.raster_height == 0 {
            return Err(SixelError::InvalidData);
        }

        let final_w = self.max_x.max(self.raster_width).max(1);
        let final_h = self.max_y.max(self.raster_height).max(1);
        self.ensure_size(final_w, final_h)?;
        let final_len = self.checked_rgba_len(final_w, final_h)?;

        if final_w != self.width {
            let source_width = self.width;
            let row_len =
                usize::try_from(u64::from(final_w) * 4).map_err(|_| SixelError::TooLarge)?;
            for row in 0..final_h {
                let source = usize::try_from(u64::from(row) * u64::from(source_width) * 4)
                    .map_err(|_| SixelError::TooLarge)?;
                let target = usize::try_from(u64::from(row) * u64::from(final_w) * 4)
                    .map_err(|_| SixelError::TooLarge)?;
                self.pixels.copy_within(source..source + row_len, target);
            }
        }
        self.pixels.truncate(final_len);
        self.pixels.shrink_to_fit();

        Ok(SixelImage {
            width: final_w,
            height: final_h,
            pixels: self.pixels,
        })
    }
}

fn grow_dimension(current: u32, required: u32, maximum: u32) -> u32 {
    if required <= current {
        return current;
    }
    current.max(1).saturating_mul(2).max(required).min(maximum)
}

/// Fit a geometrically grown axis while retaining as much spare capacity as
/// possible on the other axis. Both minimums are known to fit together.
fn fit_grown_axis(
    desired_grown: u32,
    preserved: u32,
    minimum_grown: u32,
    minimum_preserved: u32,
    max_pixels: u64,
) -> (u32, u32) {
    if u64::from(desired_grown) * u64::from(preserved) <= max_pixels {
        return (desired_grown, preserved);
    }

    let preserved_for_desired = (max_pixels / u64::from(desired_grown)) as u32;
    if preserved_for_desired >= minimum_preserved {
        return (desired_grown, preserved_for_desired.min(preserved));
    }

    let grown = ((max_pixels / u64::from(minimum_preserved)) as u32)
        .min(desired_grown)
        .max(minimum_grown);
    let retained = ((max_pixels / u64::from(grown)) as u32)
        .min(preserved)
        .max(minimum_preserved);
    (grown, retained)
}

fn parse_bounded_number(data: &[u8], mut i: usize) -> Result<(u32, usize), SixelError> {
    let mut n = 0u32;
    while i < data.len() && data[i].is_ascii_digit() {
        n = n
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(data[i] - b'0')))
            .ok_or(SixelError::TooLarge)?;
        i += 1;
    }
    Ok((n, i))
}

fn parse_saturating_number(data: &[u8], mut i: usize) -> (u32, usize) {
    let mut n = 0u32;
    while i < data.len() && data[i].is_ascii_digit() {
        n = n
            .saturating_mul(10)
            .saturating_add(u32::from(data[i] - b'0'));
        i += 1;
    }
    (n, i)
}

fn hls_to_rgb(h: u32, l: u32, s: u32) -> (u8, u8, u8) {
    let h = (h % 360) as f32;
    let l = (l.min(100) as f32) / 100.0;
    let s = (s.min(100) as f32) / 100.0;

    if s == 0.0 {
        let v = (l * 255.0) as u8;
        return (v, v, v);
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;

    let hk = h / 360.0;
    let tr = hk + 1.0 / 3.0;
    let tg = hk;
    let tb = hk - 1.0 / 3.0;

    fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            return p + (q - p) * 6.0 * t;
        }
        if t < 1.0 / 2.0 {
            return q;
        }
        if t < 2.0 / 3.0 {
            return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
        }
        p
    }

    let r = (hue_to_rgb(p, q, tr) * 255.0) as u8;
    let g = (hue_to_rgb(p, q, tg) * 255.0) as u8;
    let b = (hue_to_rgb(p, q, tb) * 255.0) as u8;
    (r, g, b)
}

#[cfg(test)]
mod tests {
    use super::{
        decode_sixel, decode_sixel_with_byte_limit, SixelDecoder, SixelError, SixelImage,
        SixelLimits, MAX_ALLOCATION_WORK_MULTIPLIER,
    };

    fn limits(max_dimension: u32, max_rgba_bytes: usize, max_operations: u64) -> SixelLimits {
        SixelLimits {
            max_input_bytes: 1024,
            max_dimension,
            max_rgba_bytes,
            max_repeat: 32_766,
            max_operations,
        }
    }

    fn decode_with_limits(data: &[u8], limits: SixelLimits) -> Result<SixelImage, SixelError> {
        let mut decoder = SixelDecoder::with_limits(limits);
        decoder.decode(data)?;
        decoder.finish()
    }

    fn pixel(image: &SixelImage, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let offset = ((y * image.width + x) * 4) as usize;
        (
            image.pixels[offset],
            image.pixels[offset + 1],
            image.pixels[offset + 2],
            image.pixels[offset + 3],
        )
    }

    fn assert_image_invariants(image: &SixelImage, limits: SixelLimits) {
        assert!(image.width > 0);
        assert!(image.height > 0);
        assert!(image.width <= limits.max_dimension);
        assert!(image.height <= limits.max_dimension);
        let expected =
            usize::try_from(u64::from(image.width) * u64::from(image.height) * 4).unwrap();
        assert_eq!(image.pixels.len(), expected);
        assert!(expected <= limits.max_rgba_bytes);
    }

    #[test]
    fn decodes_minimal_and_repeated_sixels() {
        let single = decode_sixel(b"~").unwrap();
        assert_eq!((single.width, single.height), (1, 6));
        assert_eq!(single.pixels.len(), 24);
        for y in 0..6 {
            assert_eq!(pixel(&single, 0, y), (0, 0, 0, 255));
        }

        let repeated = decode_sixel(b"!3~").unwrap();
        let literal = decode_sixel(b"~~~").unwrap();
        assert_eq!((repeated.width, repeated.height), (3, 6));
        assert_eq!(repeated.pixels, literal.pixels);
    }

    #[test]
    fn rejects_repeat_before_it_can_exceed_budgets() {
        let small = limits(8, 8 * 6 * 4, 8);
        let exact = decode_with_limits(b"!8~", small).unwrap();
        assert_eq!((exact.width, exact.height), (8, 6));

        assert_eq!(
            decode_with_limits(b"!9~", small).unwrap_err(),
            SixelError::TooLarge
        );
        assert_eq!(
            decode_sixel(b"!4294967295~").unwrap_err(),
            SixelError::TooLarge
        );
        assert_eq!(
            decode_with_limits(b"!4~$!4~$~", small).unwrap_err(),
            SixelError::TooLarge
        );
    }

    #[test]
    fn rejects_incomplete_or_zero_repeats() {
        for data in [b"".as_slice(), b"!", b"!12", b"!~", b"!0~"] {
            assert_eq!(decode_sixel(data).unwrap_err(), SixelError::InvalidData);
        }
    }

    #[test]
    fn raster_attributes_preserve_a_minimum_canvas() {
        let small = limits(12, 12 * 12 * 4, 32);
        let image = decode_with_limits(b"\"1;1;4;12@", small).unwrap();
        assert_eq!((image.width, image.height), (4, 12));
        assert_eq!(image.pixels.len(), 4 * 12 * 4);
        assert_eq!(pixel(&image, 0, 0), (0, 0, 0, 255));
        assert_eq!(pixel(&image, 3, 11), (0, 0, 0, 0));

        let larger_than_hint = decode_with_limits(b"\"1;1;2;6~~~", small).unwrap();
        assert_eq!((larger_than_hint.width, larger_than_hint.height), (3, 6));

        let raster_only = decode_with_limits(b"\"1;1;4;12", small).unwrap();
        assert_eq!((raster_only.width, raster_only.height), (4, 12));
        assert!(raster_only.pixels.iter().all(|&component| component == 0));
    }

    #[test]
    fn only_quoted_prefix_is_treated_as_raster_attributes() {
        let image = decode_sixel(b"1;1;4;12~").unwrap();
        assert_eq!((image.width, image.height), (1, 6));

        let late = decode_sixel(b"~\"1;1;4096;2048").unwrap();
        assert_eq!((late.width, late.height), (1, 6));

        let late_huge = decode_sixel(b"~\"1;1;42949672960;42949672960").unwrap();
        assert_eq!((late_huge.width, late_huge.height), (1, 6));

        let omitted_height = decode_sixel(b"\"1;1;4~").unwrap();
        assert_eq!((omitted_height.width, omitted_height.height), (4, 6));

        let extra_parameter = decode_sixel(b"\"1;1;4;6;999~").unwrap();
        assert_eq!((extra_parameter.width, extra_parameter.height), (4, 6));

        let extra_huge = decode_sixel(b"\"1;1;4;6;42949672960~").unwrap();
        assert_eq!((extra_huge.width, extra_huge.height), (4, 6));
    }

    #[test]
    fn rejects_raster_attributes_outside_exact_limits() {
        let small = limits(12, 4 * 12 * 4, 32);
        let exact = decode_with_limits(b"\"1;1;4;12", small).unwrap();
        assert_image_invariants(&exact, small);

        for data in [
            b"\"1;1;5;12".as_slice(),
            b"\"1;1;4;13",
            b"\"1;1;13;1",
            b"\"1;1;42949672960;1",
        ] {
            assert_eq!(
                decode_with_limits(data, small).unwrap_err(),
                SixelError::TooLarge
            );
        }
    }

    #[test]
    fn rejects_dynamic_width_and_height_instead_of_clamping() {
        let small = limits(12, 12 * 12 * 4, 64);
        let exact_width = decode_with_limits(b"!12~", small).unwrap();
        assert_eq!((exact_width.width, exact_width.height), (12, 6));
        assert_eq!(
            decode_with_limits(b"!13~", small).unwrap_err(),
            SixelError::TooLarge
        );

        let exact_height = decode_with_limits(b"~-~", small).unwrap();
        assert_eq!((exact_height.width, exact_height.height), (1, 12));
        assert_eq!(
            decode_with_limits(b"~-~-~", small).unwrap_err(),
            SixelError::TooLarge
        );
    }

    #[test]
    fn geometric_growth_preserves_existing_colors() {
        let image = decode_sixel(b"#1;2;100;0;0@#2;2;0;100;0~-#3;2;0;0;100@").unwrap();
        assert_eq!((image.width, image.height), (2, 12));
        assert_eq!(pixel(&image, 0, 0), (255, 0, 0, 255));
        assert_eq!(pixel(&image, 1, 0), (0, 255, 0, 255));
        assert_eq!(pixel(&image, 0, 6), (0, 0, 255, 255));
        assert_eq!(pixel(&image, 1, 6), (0, 0, 0, 0));
    }

    #[test]
    fn allocation_capacity_never_rejects_a_valid_logical_canvas() {
        let repeat_limits = limits(18, 12 * 12 * 4, 18);
        let repeated = decode_with_limits(b"!6~-!6~-!6~", repeat_limits).unwrap();
        assert_eq!((repeated.width, repeated.height), (6, 18));
        assert_image_invariants(&repeated, repeat_limits);

        let stride_limits = limits(18, 9 * 18 * 4, 27);
        let sequential =
            decode_with_limits(b"~~~~~~~~~-~~~~~~~~~-~~~~~~~~~", stride_limits).unwrap();
        assert_eq!((sequential.width, sequential.height), (9, 18));
        assert_image_invariants(&sequential, stride_limits);
    }

    #[test]
    fn alternating_axis_growth_uses_bounded_reallocations() {
        let mut data = b"?!1~".to_vec();
        for repeat in 2..=600 {
            data.extend_from_slice(b"-?");
            data.push(b'!');
            data.extend_from_slice(repeat.to_string().as_bytes());
            data.push(b'~');
        }

        let limits = SixelLimits::default();
        let mut decoder = SixelDecoder::with_limits(limits);
        decoder.decode(&data).unwrap();

        assert_eq!((decoder.max_x, decoder.max_y), (601, 3_600));
        assert!(decoder.reallocations <= 24, "{}", decoder.reallocations);
        assert!(decoder.pixels.len() <= limits.max_rgba_bytes);
        assert!(decoder.pixels.capacity() <= limits.max_rgba_bytes);

        let image = decoder.finish().unwrap();
        assert_eq!((image.width, image.height), (601, 3_600));
        assert_image_invariants(&image, limits);
    }

    #[test]
    fn allocation_work_budget_rejects_near_limit_resize_churn() {
        let mut data = b"?!1~".to_vec();
        for row in 2..=592u32 {
            data.extend_from_slice(b"-?");
            data.push(b'!');
            data.extend_from_slice((4 * row - 1).to_string().as_bytes());
            data.push(b'~');
        }

        let limits = SixelLimits::default();
        let work_limit =
            u64::try_from(limits.max_rgba_bytes).unwrap() * MAX_ALLOCATION_WORK_MULTIPLIER;
        let mut decoder = SixelDecoder::with_limits(limits);

        assert_eq!(decoder.decode(&data).unwrap_err(), SixelError::TooLarge);
        assert!(decoder.max_y < 592 * 6);
        assert!(decoder.reallocations < 32, "{}", decoder.reallocations);
        assert!(decoder.allocation_work <= work_limit);
        assert!(decoder.pixels.len() <= limits.max_rgba_bytes);
        assert!(decoder.pixels.capacity() <= limits.max_rgba_bytes);
    }

    #[test]
    fn raster_height_allows_a_partial_final_sixel_band() {
        let partial_limits = limits(13, 13 * 4, 1);
        let image = decode_with_limits(b"\"1;1;1;13--@", partial_limits).unwrap();
        assert_eq!((image.width, image.height), (1, 13));
        assert_eq!(pixel(&image, 0, 12), (0, 0, 0, 255));

        assert_eq!(
            decode_with_limits(b"\"1;1;1;13--~", partial_limits).unwrap_err(),
            SixelError::TooLarge
        );
    }

    #[test]
    fn caller_byte_budget_is_applied_before_canvas_allocation() {
        let exact = decode_sixel_with_byte_limit(b"\"1;1;4;4", 4 * 4 * 4).unwrap();
        assert_eq!((exact.width, exact.height), (4, 4));
        assert!(exact.pixels.capacity() <= 4 * 4 * 4);

        assert_eq!(
            decode_sixel_with_byte_limit(b"\"1;1;4;4", 4 * 4 * 4 - 1).unwrap_err(),
            SixelError::TooLarge
        );
    }

    #[test]
    fn production_budget_accepts_4k_but_rejects_larger_canvases() {
        let decoder = SixelDecoder::with_limits(SixelLimits::default());
        assert!(decoder.checked_rgba_len(3840, 2160).is_ok());
        assert!(decoder.checked_rgba_len(4096, 2048).is_ok());
        assert_eq!(
            decoder.checked_rgba_len(4096, 2049).unwrap_err(),
            SixelError::TooLarge
        );
    }

    #[test]
    fn enforces_input_limit_and_valid_output_invariants() {
        let mut tiny_input = limits(8, 8 * 8 * 4, 16);
        tiny_input.max_input_bytes = 4;
        assert_eq!(
            decode_with_limits(b"?????", tiny_input).unwrap_err(),
            SixelError::TooLarge
        );

        for data in [
            b"".as_slice(),
            b"?",
            b"~",
            b"!3~",
            b"\"1;1;2;6~",
            b"#1;2;100;0;0~",
            &[0xff, b'$', b'?', b'-', b'@'],
        ] {
            if let Ok(image) = decode_with_limits(data, limits(8, 8 * 8 * 4, 16)) {
                assert_image_invariants(&image, limits(8, 8 * 8 * 4, 16));
            }
        }
    }
}
