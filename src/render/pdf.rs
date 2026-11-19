//! PDF previews through a prebuilt PDFium shared library (DESIGN.md §6.1).
//!
//! The library is bound once, at the first PDF render, and shared by all workers. Lookup
//! order, first hit wins:
//! 1. `$DIR_SHUFFLE_PDFIUM_DIR`
//! 2. the copy bundled in the wheel, relative to the canonicalized executable (`uv tool`
//!    symlinks the binary): `<prefix>/share/dir-shuffle/` next to `<prefix>/bin/`
//! 3. the system library search path
//!
//! If none of these load, PDFs fall back to text cards and the reason is reported once.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use image::RgbaImage;
use pdfium_render::prelude::*;

use super::{Renderer, Unavailable};

pub const ENV_DIR: &str = "DIR_SHUFFLE_PDFIUM_DIR";

static PDFIUM: OnceLock<std::result::Result<Pdfium, String>> = OnceLock::new();
/// Where the library was loaded from (for diagnostics).
static SOURCE: OnceLock<String> = OnceLock::new();
/// Serializes whole renders. pdfium-render's `thread_safe` feature only locks single FFI
/// calls, so without this, calls on different documents could interleave.
static RENDER_LOCK: Mutex<()> = Mutex::new(());

/// Directories where a bundled PDFium may live, relative to the real executable.
pub fn bundled_dirs() -> Vec<PathBuf> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let Some(bin) = exe.parent() else {
        return Vec::new();
    };
    let mut v = Vec::new();
    if let Some(prefix) = bin.parent() {
        v.push(prefix.join("share").join("dir-shuffle"));
        v.push(prefix.join("lib"));
    }
    v.push(bin.to_path_buf());
    v
}

/// Library paths to try, in order, with a description for error messages.
pub fn candidates() -> Vec<(String, PathBuf)> {
    let mut v = Vec::new();
    if let Some(dir) = std::env::var_os(ENV_DIR).filter(|s| !s.is_empty()) {
        let dir = PathBuf::from(dir);
        // Accept either the directory or the library file itself.
        let lib = if dir.is_file() {
            dir
        } else {
            Pdfium::pdfium_platform_library_name_at_path(&dir)
        };
        v.push((format!("${ENV_DIR}"), lib));
    }
    for d in bundled_dirs() {
        v.push((
            "bundled".to_string(),
            Pdfium::pdfium_platform_library_name_at_path(&d),
        ));
    }
    v
}

/// pdfium-render's load errors are verbose `Debug` dumps; keep the innermost message.
fn one_line(e: &PdfiumError) -> String {
    let dbg = format!("{e:?}");
    match dbg
        .split("source: \"")
        .nth(1)
        .and_then(|r| r.split('"').next())
    {
        Some(msg) => msg.to_string(),
        None => dbg.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

fn bind() -> std::result::Result<Pdfium, String> {
    let mut tried = Vec::new();
    for (what, lib) in candidates() {
        if !lib.exists() {
            if what.starts_with('$') {
                tried.push(format!("{what}: {} not found", lib.display()));
            }
            continue;
        }
        match Pdfium::bind_to_library(&lib) {
            Ok(b) => {
                let _ = SOURCE.set(format!("{what}: {}", lib.display()));
                return Ok(Pdfium::new(b));
            }
            Err(e) => tried.push(format!("{}: {}", lib.display(), one_line(&e))),
        }
    }
    match Pdfium::bind_to_system_library() {
        Ok(b) => {
            let _ = SOURCE.set("system library".to_string());
            Ok(Pdfium::new(b))
        }
        Err(e) => {
            tried.push(format!("system library: {}", one_line(&e)));
            Err(format!("PDFium not loaded ({})", tried.join("; ")))
        }
    }
}

/// The shared PDFium instance, binding it on first use.
pub fn pdfium() -> std::result::Result<&'static Pdfium, &'static str> {
    PDFIUM.get_or_init(bind).as_ref().map_err(|e| e.as_str())
}

/// One-line PDFium status: where it was loaded from, or why it failed.
pub fn status() -> String {
    match pdfium() {
        Ok(_) => format!(
            "PDFium loaded ({})",
            SOURCE.get().map_or("?", |s| s.as_str())
        ),
        Err(e) => e.to_string(),
    }
}

pub struct PdfRenderer;

impl Renderer for PdfRenderer {
    fn render(&self, path: &Path, target_px: (u32, u32)) -> Result<RgbaImage> {
        let pdfium = pdfium().map_err(|e| Unavailable(e.to_string()))?;
        let (tw, th) = (target_px.0.max(1) as i32, target_px.1.max(1) as i32);
        let _guard = RENDER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let doc = pdfium
            .load_pdf_from_file(path, None)
            .with_context(|| format!("opening {}", path.display()))?;
        let page = doc.pages().first().context("PDF has no pages")?;
        // Render straight at the target size. `scale_page_to_display_size` would rotate
        // landscape pages, which is wrong for plots, so we turn rotation off again.
        let cfg = PdfRenderConfig::new()
            .scale_page_to_display_size(tw, th)
            .rotate_if_landscape(PdfPageRenderRotation::None, false)
            .render_form_data(false);
        let bitmap = page.render_with_config(&cfg).context("rendering page 1")?;
        let img = bitmap.as_image().context("converting bitmap")?;
        drop(bitmap);
        drop(page);
        drop(doc);
        Ok(img.into_rgba8())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page landscape PDF (300×150 pt) with a black square in the lower left.
    pub fn tiny_pdf() -> Vec<u8> {
        let content = b"0 0 0 rg 10 10 50 50 re f";
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 150] /Contents 4 0 R >>".to_string(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                std::str::from_utf8(content).unwrap()
            ),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn renders_page_one_at_target_size() {
        if let Err(e) = pdfium() {
            eprintln!("skipping: {e}");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.pdf");
        std::fs::write(&p, tiny_pdf()).unwrap();
        let img = PdfRenderer.render(&p, (200, 200)).unwrap();
        // Landscape, not rotated, fitted exactly to the width.
        assert_eq!(img.dimensions(), (200, 100));
        // The black square sits bottom-left; the top right is white.
        assert_eq!(img.get_pixel(15, 85).0[..3], [0, 0, 0]);
        assert_eq!(img.get_pixel(190, 5).0[..3], [255, 255, 255]);
        // Height-limited target.
        let img = PdfRenderer.render(&p, (1000, 50)).unwrap();
        assert_eq!(img.dimensions(), (100, 50));

        std::fs::write(&p, b"%PDF-1.4 garbage").unwrap();
        assert!(PdfRenderer.render(&p, (10, 10)).is_err());
    }

    #[test]
    fn parallel_renders_are_safe() {
        if pdfium().is_err() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.pdf");
        std::fs::write(&p, tiny_pdf()).unwrap();
        std::thread::scope(|s| {
            for i in 0..8 {
                let p = &p;
                s.spawn(move || {
                    for j in 0..5 {
                        let w = 50 + i * 10 + j;
                        let img = PdfRenderer.render(p, (w, 1000)).unwrap();
                        assert_eq!(img.width(), w);
                    }
                });
            }
        });
    }
}
