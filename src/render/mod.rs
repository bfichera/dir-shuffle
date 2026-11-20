//! Turning files into pixels (DESIGN.md §6.1).
//!
//! The renderer is chosen by file extension. PDFs go through PDFium, which renders page 1
//! at exactly the target size. Common raster formats use `image` plus `fast_image_resize`.
//! Anything else is shown as plain text if it looks like text (`text`), and otherwise gets
//! a text card (path, size, mtime), drawn by the UI.

pub mod kitty;
pub mod pdf;
pub mod raster;
pub mod text;

use std::path::Path;
use std::time::SystemTime;

use anyhow::Result;
use image::RgbaImage;

pub trait Renderer: Send + Sync {
    /// Render `path` to fit inside `target_px` (width, height), keeping the aspect ratio.
    /// One side matches the target exactly.
    fn render(&self, path: &Path, target_px: (u32, u32)) -> Result<RgbaImage>;
}

/// Raised when a renderer's backend is missing (e.g. PDFium could not be loaded).
#[derive(Debug, Clone)]
pub struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unavailable {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Pdf,
    Raster,
}

/// Extensions that `image` can decode with the formats compiled in. AVIF is not listed:
/// the `image` crate only decodes it with the native dav1d feature.
const RASTER_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "jfif", "gif", "bmp", "dib", "tif", "tiff", "webp", "tga", "pnm",
    "pbm", "pgm", "ppm", "pam", "qoi", "ico", "hdr", "exr", "ff",
];

pub fn kind_for_ext(ext: &str) -> Option<Kind> {
    let e = ext.to_ascii_lowercase();
    if e == "pdf" {
        Some(Kind::Pdf)
    } else if RASTER_EXTS.contains(&e.as_str()) {
        Some(Kind::Raster)
    } else {
        None
    }
}

pub fn renderer_for(kind: Kind) -> &'static dyn Renderer {
    match kind {
        Kind::Pdf => &pdf::PdfRenderer,
        Kind::Raster => &raster::RasterRenderer,
    }
}

/// Scale `(w, h)` to fit inside `target`, keeping the aspect ratio (up or down).
pub fn fit(w: u32, h: u32, target: (u32, u32)) -> (u32, u32) {
    let (tw, th) = (target.0.max(1), target.1.max(1));
    if w == 0 || h == 0 {
        return (tw, th);
    }
    let sx = tw as f64 / w as f64;
    let sy = th as f64 / h as f64;
    if sx <= sy {
        (tw, ((h as f64 * sx).round() as u32).clamp(1, th))
    } else {
        (((w as f64 * sy).round() as u32).clamp(1, tw), th)
    }
}

/// Composite onto white, so transparent plots look the same as PDFs (which PDFium
/// renders on white) whatever the terminal background is.
pub fn flatten_on_white(img: &mut RgbaImage) {
    for p in img.pixels_mut() {
        let a = p[3] as u32;
        if a == 255 {
            continue;
        }
        for c in 0..3 {
            p[c] = ((p[c] as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
        }
        p[3] = 255;
    }
}

// ---------------------------------------------------------------------------
// Text cards
// ---------------------------------------------------------------------------

pub fn human_size(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// `YYYY-MM-DD HH:MM:SS UTC` without a date library (civil-from-days, H. Hinnant).
pub fn format_time(t: SystemTime) -> String {
    let secs = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Lines of a text card: path, size, modification time, and an optional note.
pub fn text_card(path: &Path, note: Option<&str>) -> Vec<String> {
    let mut lines = vec![path.display().to_string(), String::new()];
    match std::fs::metadata(path) {
        Ok(md) => {
            lines.push(format!("size      {}", human_size(md.len())));
            if let Ok(t) = md.modified() {
                lines.push(format!("modified  {}", format_time(t)));
            }
        }
        Err(e) => lines.push(format!("cannot stat: {e}")),
    }
    if let Some(n) = note {
        lines.push(String::new());
        lines.push(n.to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn fit_keeps_aspect_and_touches_one_side() {
        assert_eq!(fit(200, 100, (100, 100)), (100, 50));
        assert_eq!(fit(100, 200, (100, 100)), (50, 100));
        assert_eq!(fit(10, 10, (300, 200)), (200, 200));
        assert_eq!(fit(1000, 1, (100, 100)), (100, 1));
    }

    #[test]
    fn time_formatting() {
        assert_eq!(
            format_time(SystemTime::UNIX_EPOCH),
            "1970-01-01 00:00:00 UTC"
        );
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(951_782_400 + 3_661);
        assert_eq!(format_time(t), "2000-02-29 01:01:01 UTC");
    }

    #[test]
    fn sizes_and_kinds() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(kind_for_ext("PDF"), Some(Kind::Pdf));
        assert_eq!(kind_for_ext("jpeg"), Some(Kind::Raster));
        assert_eq!(kind_for_ext("txt"), None);
        assert_eq!(kind_for_ext(""), None);
    }

    #[test]
    fn flatten() {
        let mut img = RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        flatten_on_white(&mut img);
        assert_eq!(img.get_pixel(0, 0).0, [255, 255, 255, 255]);
        let mut img = RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        flatten_on_white(&mut img);
        assert_eq!(img.get_pixel(0, 0).0, [0, 0, 0, 255]);
    }
}
