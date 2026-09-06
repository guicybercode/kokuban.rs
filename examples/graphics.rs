//! Run inside Kokuban: cargo run --example graphics -- kitty|sixel|animate|stream|photo.png
use base64::Engine;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

fn kitty_frame(output: &mut impl Write, png: &[u8]) -> io::Result<()> {
    kitty_upload(output, "a=T,f=100,i=42,q=2,C=1,c=40,r=12", png)
}

fn kitty_upload(output: &mut impl Write, control: &str, png: &[u8]) -> io::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    let chunks = encoded.as_bytes().chunks(4096);
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let more = u8::from(index + 1 < count);
        if index == 0 {
            write!(output, "\x1b_G{control},m={more};")?;
        } else {
            write!(output, "\x1b_Gm={more};")?;
        }
        output.write_all(chunk)?;
        output.write_all(b"\x1b\\")?;
    }
    output.flush()
}

fn pattern(frame: u32) -> io::Result<Vec<u8>> {
    let mut pixels = vec![0; 160 * 90 * 4];
    for y in 0..90u32 {
        for x in 0..160u32 {
            let index = (y * 160 + x) as usize * 4;
            let moving = (x + frame * 3) % 160 < 30;
            pixels[index..index + 4].copy_from_slice(&[
                if moving { 255 } else { (x * 255 / 159) as u8 },
                (y * 255 / 89) as u8,
                if moving { 32 } else { 180 },
                255,
            ]);
        }
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 160, 90);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&pixels)?;
    }
    Ok(bytes)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args_os().nth(1).unwrap_or_else(|| "kitty".into());
    let mut output = io::stdout().lock();
    match mode.to_str() {
        Some("sixel") => {
            // Three colored bands, 120 x 36 native pixels, no external tools.
            output.write_all(b"\x1bPq\"1;1;120;36#1;2;100;0;0#2;2;0;100;0#3;2;0;0;100")?;
            for color in 1..=3 {
                for _ in 0..2 {
                    write!(output, "#{color}!120~-")?;
                }
            }
            output.write_all(b"\x1b\\\r\nSixel complete\r\n")?;
        }
        Some("stream") => {
            // Replaces a static Kitty image at 30 FPS; does not claim support
            // for Kitty's separate frame/animation-control protocol.
            let start = Instant::now();
            output.write_all(b"\x1b[s")?;
            for frame in 0..120 {
                output.write_all(b"\x1b[u")?;
                kitty_frame(&mut output, &pattern(frame)?)?;
                let deadline = Duration::from_secs_f64(f64::from(frame + 1) / 30.0);
                std::thread::sleep(deadline.saturating_sub(start.elapsed()));
            }
            output.write_all(b"\x1b[u\x1b[12B\r\nStream complete (120 frames)\r\n")?;
        }
        Some("animate") => {
            kitty_frame(&mut output, &pattern(0)?)?;
            output.write_all(b"\x1b_Ga=a,i=42,r=1,z=40,v=4,q=2\x1b\\")?;
            for frame in 1..30 {
                kitty_upload(&mut output, "a=f,f=100,i=42,z=40,q=2", &pattern(frame)?)?;
            }
            // The terminal owns playback after this process exits (three loops).
            output.write_all(b"\x1b_Ga=a,i=42,s=3,q=2\x1b\\\x1b[12B\r\nNative animation: 30 frames, 3 loops\r\n")?;
        }
        Some("kitty") => {
            kitty_frame(&mut output, &pattern(0)?)?;
            output.write_all(b"\x1b[12B\r\nKitty PNG complete\r\n")?;
        }
        _ => {
            let mut bytes = Vec::new();
            std::fs::File::open(mode)?
                .take(50 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 50 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err("expected a PNG file no larger than 50 MiB".into());
            }
            kitty_frame(&mut output, &bytes)?;
            output.write_all(b"\x1b[12B\r\n")?;
        }
    }
    output.flush()?;
    Ok(())
}
