//! Compare eager fontdue loading with the Android per-glyph fallback on one font.
//! See README.md for an isolated host build; this is not part of the APK.
#[path = "../src/android_font_fallback.rs"]
mod fallback;

use fallback::FontFallback;
use fontdue::{Font, FontSettings};

fn main() {
    let mut arguments = std::env::args().skip(1);
    let mode = arguments.next().expect("mode: eager or lazy");
    let path = arguments.next().expect("path to a system font collection");
    assert!(std::fs::metadata(&path).unwrap().len() <= 32 * 1024 * 1024);
    let start = std::time::Instant::now();
    let bytes = std::fs::read(path).unwrap();
    let (eager, lazy) = match mode.as_str() {
        "eager" => (
            Some(
                Font::from_bytes(
                    bytes,
                    FontSettings {
                        scale: 36.75,
                        load_substitutions: false,
                        ..FontSettings::default()
                    },
                )
                .unwrap(),
            ),
            None,
        ),
        "lazy" => (None, Some(FontFallback::from_bytes(bytes).unwrap())),
        _ => panic!("mode must be eager or lazy"),
    };
    println!("mode={mode} font_load_ms={}", start.elapsed().as_millis());
    for character in ['ㄱ', '가'] {
        let start = std::time::Instant::now();
        let (present, metrics, pixels) = if let Some(font) = &eager {
            let (metrics, pixels) = font.rasterize(character, 36.75);
            (font.has_glyph(character), metrics, pixels)
        } else {
            let font = lazy.as_ref().unwrap();
            let metrics = font.metrics(character, 36.75).unwrap();
            let pixels = font.rasterize(character, 36.75, metrics, 1024).unwrap();
            (font.has_glyph(character), metrics, pixels)
        };
        println!(
            "glyph={character} present={present} width={} height={} coverage={} raster_us={}",
            metrics.width,
            metrics.height,
            pixels.iter().map(|pixel| u64::from(*pixel)).sum::<u64>(),
            start.elapsed().as_micros()
        );
    }
}
