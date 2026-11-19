//! Kitty graphics protocol with Unicode placeholders.
//!
//! ratatui-image detects the protocol and the cell size, and renders the sixel, iTerm2 and
//! halfblock fallbacks. Kitty is encoded here instead because the app has to control the
//! image lifecycle. ratatui-image never deletes images it has sent, so a session that
//! scrolls through hundreds of previews would keep piling them up in terminal memory.
//! This module keeps exactly one image alive in the terminal: when the selection changes,
//! the old image is deleted (`a=d,d=I`) and the new one is sent from the cache.
//!
//! The image is shown through a virtual placement (`U=1`) and placeholder cells
//! (U+10EEEE, with the image id in the foreground colour and row/column diacritics). These
//! are ordinary buffer cells to ratatui, so redrawing them as text removes the image and
//! nothing stale can stay on screen.

use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicU32, Ordering};

use image::RgbaImage;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::Color;

const PLACEHOLDER: char = '\u{10EEEE}';
const CHUNK: usize = 3072; // raw bytes per chunk, i.e. 4096 base64 characters

static NEXT_ID: AtomicU32 = AtomicU32::new(0);

/// A fresh image id. Ids use 24 bits (so they fit the RGB foreground colour exactly) and
/// start at a per-process offset, to avoid clashing with other programs' images.
fn next_id() -> u32 {
    let base = (std::process::id().wrapping_mul(2_654_435_761)) & 0x00FF_FFFF;
    loop {
        let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let id = (base.wrapping_add(n)) & 0x00FF_FFFF;
        if id != 0 {
            return id;
        }
    }
}

/// A protocol-ready kitty image: the full transmit sequence plus its size in cells.
#[derive(Debug, Clone)]
pub struct KittyImage {
    pub id: u32,
    pub cols: u16,
    pub rows: u16,
    pub transmit: String,
}

impl KittyImage {
    /// Encode `img` for display in whole cells. The image is padded with transparent pixels
    /// to a multiple of the cell size, so the terminal never stretches it. With `compress`,
    /// the RGBA data is zlib-deflated (`o=z`). Otherwise it is sent as PNG (`f=100`), which
    /// every kitty-protocol terminal supports.
    pub fn encode(img: &RgbaImage, cell_px: (u16, u16), compress: bool) -> KittyImage {
        let (cw, ch) = (cell_px.0.max(1) as u32, cell_px.1.max(1) as u32);
        let cols = img.width().div_ceil(cw).clamp(1, DIACRITICS.len() as u32);
        let rows = img.height().div_ceil(ch).clamp(1, DIACRITICS.len() as u32);
        let (w, h) = (cols * cw, rows * ch);
        let padded;
        let img = if img.dimensions() == (w, h) {
            img
        } else {
            let mut p = RgbaImage::new(w, h);
            image::imageops::replace(&mut p, img, 0, 0);
            padded = p;
            &padded
        };
        let id = next_id();
        let (fmt, payload) = if compress {
            ("f=32,o=z", zlib(img.as_raw()))
        } else {
            ("f=100", png(img))
        };
        let mut s = String::with_capacity(payload.len() * 4 / 3 + payload.len() / CHUNK * 16 + 128);
        let n = payload.len().div_ceil(CHUNK).max(1);
        let mut chunks: Vec<&[u8]> = payload.chunks(CHUNK).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        for (i, chunk) in chunks.into_iter().enumerate() {
            let more = u8::from(i + 1 < n);
            if i == 0 {
                let _ = write!(
                    s,
                    "\x1b_Ga=T,U=1,q=2,i={id},{fmt},s={w},v={h},c={cols},r={rows},m={more};"
                );
            } else {
                let _ = write!(s, "\x1b_Gm={more};");
            }
            base64_into(chunk, &mut s);
            s.push_str("\x1b\\");
        }
        KittyImage {
            id,
            cols: cols as u16,
            rows: rows as u16,
            transmit: s,
        }
    }

    pub fn byte_size(&self) -> usize {
        self.transmit.len() + std::mem::size_of::<Self>()
    }

    /// Fill `area` (top-left aligned, clipped) with placeholder cells for this image. Every
    /// cell carries its full row and column diacritics, so partial redraws stay correct.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let max = DIACRITICS.len() as u16;
        let w = area.width.min(self.cols).min(max);
        let h = area.height.min(self.rows).min(max);
        let fg = Color::Rgb((self.id >> 16) as u8, (self.id >> 8) as u8, self.id as u8);
        let mut sym = String::with_capacity(16);
        for y in 0..h {
            for x in 0..w {
                if let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                    sym.clear();
                    sym.push(PLACEHOLDER);
                    sym.push(DIACRITICS[y as usize]);
                    sym.push(DIACRITICS[x as usize]);
                    cell.set_symbol(&sym)
                        .set_fg(fg)
                        .set_diff_option(CellDiffOption::ForcedWidth(
                            std::num::NonZeroU16::new(1).unwrap(),
                        ));
                }
            }
        }
    }
}

/// Delete an image and free its data in the terminal.
pub fn delete_sequence(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}

/// Tracks the single image that currently lives in the terminal.
///
/// Each frame calls `before_draw` with the image the frame shows. A new image is sent
/// before the frame's placeholder cells are written, and the old one is deleted only in
/// `after_draw`, once its cells have been overwritten. That way there's no blank flash
/// between two previews.
#[derive(Debug, Default)]
pub struct KittyState {
    shown: Option<u32>,
    retire: Option<u32>,
}

impl KittyState {
    pub fn before_draw(
        &mut self,
        img: Option<&KittyImage>,
        out: &mut impl std::io::Write,
    ) -> std::io::Result<()> {
        let want = img.map(|i| i.id);
        if self.shown == want {
            return Ok(());
        }
        if let Some(i) = img {
            out.write_all(i.transmit.as_bytes())?;
        }
        if let Some(old) = std::mem::replace(&mut self.shown, want) {
            // Two changes before a draw: the earlier pending one never became visible.
            if let Some(stale) = self.retire.replace(old) {
                out.write_all(delete_sequence(stale).as_bytes())?;
            }
        }
        Ok(())
    }

    pub fn after_draw(&mut self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        if let Some(old) = self.retire.take() {
            out.write_all(delete_sequence(old).as_bytes())?;
            out.flush()?;
        }
        Ok(())
    }

    /// Delete everything we have shown (on exit).
    pub fn clear(&mut self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        for id in [self.shown.take(), self.retire.take()]
            .into_iter()
            .flatten()
        {
            out.write_all(delete_sequence(id).as_bytes())?;
        }
        out.flush()
    }
}

fn zlib(raw: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(raw).expect("writing into a Vec cannot fail");
    enc.finish().expect("writing into a Vec cannot fail")
}

fn png(img: &RgbaImage) -> Vec<u8> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let mut out = Vec::new();
    // RGBA, so the padding to whole cells stays transparent.
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .expect("PNG encoding into a Vec cannot fail");
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, appended to `out`.
pub fn base64_into(data: &[u8], out: &mut String) {
    out.reserve(data.len().div_ceil(3) * 4);
    let (chunks, r) = data.as_chunks::<3>();
    for c in chunks {
        let n = (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
        for shift in [18, 12, 6, 0] {
            out.push(B64[(n >> shift & 63) as usize] as char);
        }
    }
    if !r.is_empty() {
        let n = (r[0] as u32) << 16 | (r.get(1).copied().unwrap_or(0) as u32) << 8;
        out.push(B64[(n >> 18 & 63) as usize] as char);
        out.push(B64[(n >> 12 & 63) as usize] as char);
        out.push(if r.len() == 2 {
            B64[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push('=');
    }
}

pub fn base64(data: &[u8]) -> String {
    let mut s = String::new();
    base64_into(data, &mut s);
    s
}

include!("kitty_diacritics.rs");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn encode_and_chunking() {
        let img = RgbaImage::from_fn(200, 100, |x, y| {
            image::Rgba([(x ^ y) as u8, x as u8, y as u8, 255])
        });
        let k = KittyImage::encode(&img, (10, 20), true);
        assert_eq!((k.cols, k.rows), (20, 5));
        assert!(k.id > 0 && k.id <= 0xFF_FFFF);
        let first = k.transmit.split("\x1b\\").next().unwrap();
        assert!(first.contains("a=T,U=1,q=2"));
        assert!(first.contains(&format!("i={},f=32,o=z,s=200,v=100,c=20,r=5", k.id)));
        // Every chunk carries at most 4096 base64 chars, and only the last has m=0.
        let chunks: Vec<&str> = k.transmit.split_terminator("\x1b\\").collect();
        for (i, c) in chunks.iter().enumerate() {
            let data = c.split_once(';').unwrap().1;
            assert!(data.len() <= 4096);
            let last = i + 1 == chunks.len();
            assert_eq!(c.contains("m=0;"), last, "chunk {i}");
        }
        // Round-trip the compressed payload.
        let b64: String = chunks
            .iter()
            .map(|c| c.split_once(';').unwrap().1)
            .collect();
        let raw = decode_b64(&b64);
        let mut inflated = Vec::new();
        use std::io::Read;
        flate2::read::ZlibDecoder::new(&raw[..])
            .read_to_end(&mut inflated)
            .unwrap();
        assert_eq!(inflated.len(), 200 * 100 * 4);
        assert_eq!(&inflated[..4], &[0, 0, 0, 255]);

        // Padding to whole cells: 195×95 px in 10×20 cells → 20×5 cells, 200×100 px.
        let odd = RgbaImage::from_pixel(195, 95, image::Rgba([9, 9, 9, 255]));
        let k3 = KittyImage::encode(&odd, (10, 20), true);
        assert_eq!((k3.cols, k3.rows), (20, 5));
        assert!(k3.transmit.contains(",s=200,v=100,"));

        let k2 = KittyImage::encode(&img, (10, 20), false);
        assert!(k2.transmit.contains(",f=100,"));
        assert_ne!(k.id, k2.id);
    }

    fn decode_b64(s: &str) -> Vec<u8> {
        let val = |c: u8| B64.iter().position(|&b| b == c).map(|p| p as u32);
        let mut out = Vec::new();
        for q in s.as_bytes().chunks(4) {
            let v: Vec<Option<u32>> = q.iter().map(|&c| val(c)).collect();
            let n = v
                .iter()
                .enumerate()
                .fold(0u32, |a, (i, x)| a | x.unwrap_or(0) << (18 - 6 * i));
            let bytes = v.iter().filter(|x| x.is_some()).count() - 1;
            out.extend_from_slice(&n.to_be_bytes()[1..1 + bytes]);
        }
        out
    }

    #[test]
    fn placeholders_and_state() {
        let img = RgbaImage::from_pixel(30, 40, image::Rgba([1, 2, 3, 255]));
        let k = KittyImage::encode(&img, (10, 20), true);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
        k.render(Rect::new(2, 3, 10, 10), &mut buf);
        let c = &buf[(3, 4)];
        let sym: Vec<char> = c.symbol().chars().collect();
        assert_eq!(sym, [PLACEHOLDER, DIACRITICS[1], DIACRITICS[1]]);
        assert_eq!(buf[(5, 3)].symbol(), " "); // outside 3×2
        assert_eq!(
            c.fg,
            Color::Rgb((k.id >> 16) as u8, (k.id >> 8) as u8, k.id as u8)
        );

        let k2 = KittyImage::encode(&img, (10, 20), true);
        let mut st = KittyState::default();
        let mut out = Vec::new();
        st.before_draw(Some(&k), &mut out).unwrap();
        assert!(out.starts_with(b"\x1b_Ga=T"));
        st.after_draw(&mut out).unwrap();
        out.clear();
        st.before_draw(Some(&k), &mut out).unwrap();
        st.after_draw(&mut out).unwrap();
        assert!(out.is_empty());
        // Switching: transmit the new image before the draw, delete the old one after.
        st.before_draw(Some(&k2), &mut out).unwrap();
        assert!(out.starts_with(b"\x1b_Ga=T") && !out.ends_with(delete_sequence(k.id).as_bytes()));
        out.clear();
        st.after_draw(&mut out).unwrap();
        assert_eq!(out, delete_sequence(k.id).as_bytes());
        out.clear();
        st.before_draw(None, &mut out).unwrap();
        assert!(out.is_empty());
        st.after_draw(&mut out).unwrap();
        assert_eq!(out, delete_sequence(k2.id).as_bytes());
    }
}
