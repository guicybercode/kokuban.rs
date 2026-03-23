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
pub fn decode_sixel(data: &[u8]) -> Result<SixelImage, SixelError> {
    let mut decoder = SixelDecoder::new();
    decoder.decode(data)?;
    Ok(decoder.finish())
}

#[derive(Debug, thiserror::Error)]
pub enum SixelError {
    #[error("invalid sixel data")]
    InvalidData,
    #[error("image too large")]
    TooLarge,
}

struct SixelDecoder {
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    max_x: u32,
    current_color: u16,
    palette: Vec<(u8, u8, u8)>,
    pixels: Vec<u8>, // RGBA, width×height
    allocated_height: u32,
}

impl SixelDecoder {
    fn new() -> Self {
        // Initialize with a default VGA palette (first 16 colors)
        let mut palette = vec![(0u8, 0u8, 0u8); 256];
        let default_colors: [(u8, u8, u8); 16] = [
            (0, 0, 0), (205, 49, 49), (13, 188, 121), (229, 229, 16),
            (36, 114, 200), (188, 63, 188), (17, 168, 205), (229, 229, 229),
            (102, 102, 102), (241, 76, 76), (35, 209, 139), (245, 245, 67),
            (59, 142, 234), (214, 112, 214), (41, 184, 219), (255, 255, 255),
        ];
        for (i, &c) in default_colors.iter().enumerate() {
            palette[i] = c;
        }

        Self {
            width: 0,
            height: 0,
            cursor_x: 0,
            cursor_y: 0,
            max_x: 0,
            current_color: 0,
            palette,
            pixels: Vec::new(),
            allocated_height: 0,
        }
    }

    fn decode(&mut self, data: &[u8]) -> Result<(), SixelError> {
        let mut i = 0;

        // Parse optional raster attributes: "Pan;Pad;Ph;Pv" after 'q'
        // Skip any numeric params and ';' before sixel data starts
        i = self.parse_raster_attributes(data, i);

        while i < data.len() {
            let b = data[i];
            match b {
                // Color introducer
                b'#' => {
                    i += 1;
                    i = self.parse_color(data, i)?;
                }
                // Carriage return (go to start of current sixel band)
                b'$' => {
                    self.cursor_x = 0;
                    i += 1;
                }
                // Newline (move down 6 pixels, go to start)
                b'-' => {
                    self.cursor_x = 0;
                    self.cursor_y += 6;
                    i += 1;
                }
                // Repeat introducer
                b'!' => {
                    i += 1;
                    let (count, next_i) = parse_number(data, i);
                    i = next_i;
                    if i < data.len() && data[i] >= 0x3F && data[i] <= 0x7E {
                        let sixel_bits = data[i] - 0x3F;
                        for _ in 0..count {
                            self.put_sixel(sixel_bits);
                        }
                        i += 1;
                    }
                }
                // Sixel data characters (0x3F to 0x7E)
                0x3F..=0x7E => {
                    let sixel_bits = b - 0x3F;
                    self.put_sixel(sixel_bits);
                    i += 1;
                }
                _ => {
                    i += 1; // Skip unknown bytes
                }
            }
        }

        Ok(())
    }

    fn parse_raster_attributes(&mut self, data: &[u8], mut i: usize) -> usize {
        // Format: "Pan;Pad;Ph;Pv" where Ph=pixel width, Pv=pixel height
        let mut params = Vec::new();
        let mut current = 0u32;
        let mut has_digit = false;

        while i < data.len() {
            match data[i] {
                b'0'..=b'9' => {
                    current = current.saturating_mul(10).saturating_add((data[i] - b'0') as u32);
                    has_digit = true;
                    i += 1;
                }
                b';' => {
                    params.push(if has_digit { current } else { 0 });
                    current = 0;
                    has_digit = false;
                    i += 1;
                }
                _ => {
                    if has_digit {
                        params.push(current);
                    }
                    break;
                }
            }
        }

        // params[2] = width hint, params[3] = height hint
        if params.len() >= 4 {
            let w = params[2].min(4096);
            let h = params[3].min(4096);
            if w > 0 && h > 0 {
                self.width = w;
                self.height = h;
                self.ensure_size(w, h);
            }
        }

        i
    }

    fn parse_color(&mut self, data: &[u8], mut i: usize) -> Result<usize, SixelError> {
        // Parse: N[;type;v1;v2;v3]
        let (color_num, next_i) = parse_number(data, i);
        i = next_i;

        if color_num > 255 {
            return Ok(i);
        }

        if i < data.len() && data[i] == b';' {
            // Color definition
            i += 1;
            let (color_type, next_i) = parse_number(data, i);
            i = next_i;
            if i < data.len() && data[i] == b';' { i += 1; }
            let (v1, next_i) = parse_number(data, i);
            i = next_i;
            if i < data.len() && data[i] == b';' { i += 1; }
            let (v2, next_i) = parse_number(data, i);
            i = next_i;
            if i < data.len() && data[i] == b';' { i += 1; }
            let (v3, next_i) = parse_number(data, i);
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

    fn put_sixel(&mut self, bits: u8) {
        let x = self.cursor_x;
        let y = self.cursor_y;

        // Ensure we have enough space
        let needed_w = x + 1;
        let needed_h = y + 6;
        if needed_w > self.width || needed_h > self.allocated_height {
            let new_w = needed_w.max(self.width);
            let new_h = needed_h.max(self.allocated_height);
            self.ensure_size(new_w, new_h);
        }

        let (r, g, b) = self.palette[self.current_color as usize];

        // Each bit in `bits` (6 bits) maps to a vertical pixel
        for bit in 0..6u32 {
            if bits & (1 << bit) != 0 {
                let py = y + bit;
                if py < self.allocated_height {
                    let idx = ((py * self.width + x) * 4) as usize;
                    if idx + 3 < self.pixels.len() {
                        self.pixels[idx] = r;
                        self.pixels[idx + 1] = g;
                        self.pixels[idx + 2] = b;
                        self.pixels[idx + 3] = 255;
                    }
                }
            }
        }

        self.cursor_x += 1;
        if self.cursor_x > self.max_x {
            self.max_x = self.cursor_x;
        }
        let bottom = y + 6;
        if bottom > self.height {
            self.height = bottom;
        }
    }

    fn ensure_size(&mut self, w: u32, h: u32) {
        // Cap to prevent OOM
        let w = w.min(4096);
        let h = h.min(4096);

        if w == self.width && h <= self.allocated_height {
            return;
        }

        let new_w = w.max(self.width);
        let new_h = h.max(self.allocated_height);
        let new_size = (new_w * new_h * 4) as usize;
        let mut new_pixels = vec![0u8; new_size];

        // Copy existing data if width changed
        if self.width > 0 && self.allocated_height > 0 {
            let copy_w = self.width.min(new_w);
            let copy_h = self.allocated_height.min(new_h);
            for row in 0..copy_h {
                let src_start = (row * self.width * 4) as usize;
                let dst_start = (row * new_w * 4) as usize;
                let len = (copy_w * 4) as usize;
                if src_start + len <= self.pixels.len() && dst_start + len <= new_pixels.len() {
                    new_pixels[dst_start..dst_start + len]
                        .copy_from_slice(&self.pixels[src_start..src_start + len]);
                }
            }
        }

        self.width = new_w;
        self.allocated_height = new_h;
        self.pixels = new_pixels;
    }

    fn finish(mut self) -> SixelImage {
        // Trim to actual used dimensions
        let final_w = self.max_x.max(1).min(self.width);
        let final_h = self.height.max(1).min(self.allocated_height);

        if final_w == self.width {
            self.pixels.truncate((final_w * final_h * 4) as usize);
            return SixelImage {
                width: final_w,
                height: final_h,
                pixels: self.pixels,
            };
        }

        // Need to compact rows if width shrank
        let mut out = vec![0u8; (final_w * final_h * 4) as usize];
        for row in 0..final_h {
            let src = (row * self.width * 4) as usize;
            let dst = (row * final_w * 4) as usize;
            let len = (final_w * 4) as usize;
            if src + len <= self.pixels.len() {
                out[dst..dst + len].copy_from_slice(&self.pixels[src..src + len]);
            }
        }

        SixelImage {
            width: final_w,
            height: final_h,
            pixels: out,
        }
    }
}

fn parse_number(data: &[u8], mut i: usize) -> (u32, usize) {
    let mut n = 0u32;
    while i < data.len() && data[i].is_ascii_digit() {
        n = n.saturating_mul(10).saturating_add((data[i] - b'0') as u32);
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
        if t < 0.0 { t += 1.0; }
        if t > 1.0 { t -= 1.0; }
        if t < 1.0 / 6.0 { return p + (q - p) * 6.0 * t; }
        if t < 1.0 / 2.0 { return q; }
        if t < 2.0 / 3.0 { return p + (q - p) * (2.0 / 3.0 - t) * 6.0; }
        p
    }

    let r = (hue_to_rgb(p, q, tr) * 255.0) as u8;
    let g = (hue_to_rgb(p, q, tg) * 255.0) as u8;
    let b = (hue_to_rgb(p, q, tb) * 255.0) as u8;
    (r, g, b)
}
