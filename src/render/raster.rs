//! Raster previews: decode with `image`, resize with `fast_image_resize`.

use std::path::Path;

use anyhow::{Context, Result};
use fast_image_resize::{
    images::Image as FrImage, FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer,
};
use image::RgbaImage;

use super::{fit, flatten_on_white, Renderer};

pub struct RasterRenderer;

impl Renderer for RasterRenderer {
    fn render(&self, path: &Path, target_px: (u32, u32)) -> Result<RgbaImage> {
        let img = image::ImageReader::open(path)
            .with_context(|| format!("opening {}", path.display()))?
            .with_guessed_format()
            .with_context(|| format!("reading {}", path.display()))?
            .decode()
            .with_context(|| format!("decoding {}", path.display()))?;
        let src = img.into_rgba8();
        let mut out = resize_rgba(&src, target_px)?;
        flatten_on_white(&mut out);
        Ok(out)
    }
}

/// Resize to fit `target_px` (up or down), with alpha-correct filtering.
pub fn resize_rgba(src: &RgbaImage, target_px: (u32, u32)) -> Result<RgbaImage> {
    let (w, h) = fit(src.width(), src.height(), target_px);
    if (w, h) == src.dimensions() {
        return Ok(src.clone());
    }
    let mut dst = FrImage::new(w, h, PixelType::U8x4);
    // Shrinking: a box-like filter is fast and alias-free. Enlarging: Catmull-Rom stays sharp.
    let alg = if w < src.width() {
        ResizeAlg::Convolution(FilterType::Bilinear)
    } else {
        ResizeAlg::Convolution(FilterType::CatmullRom)
    };
    Resizer::new()
        .resize(
            src,
            &mut dst,
            &ResizeOptions::new().resize_alg(alg).use_alpha(true),
        )
        .context("resizing image")?;
    RgbaImage::from_raw(w, h, dst.into_vec()).context("resized buffer has the wrong size")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_png_to_fit() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.png");
        let img = RgbaImage::from_fn(400, 200, |x, _| image::Rgba([(x % 256) as u8, 0, 0, 128]));
        img.save(&p).unwrap();
        let out = RasterRenderer.render(&p, (100, 100)).unwrap();
        assert_eq!(out.dimensions(), (100, 50));
        assert!(out.pixels().all(|p| p[3] == 255));
        let out = RasterRenderer.render(&p, (800, 800)).unwrap();
        assert_eq!(out.dimensions(), (800, 400));
        // Wrong extension: the format is sniffed from content.
        let q = tmp.path().join("b.jpg");
        std::fs::copy(&p, &q).unwrap();
        assert!(RasterRenderer.render(&q, (50, 50)).is_ok());
        // Garbage fails cleanly.
        std::fs::write(&q, b"not an image").unwrap();
        assert!(RasterRenderer.render(&q, (50, 50)).is_err());
    }
}
