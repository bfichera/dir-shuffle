//! Plain-text previews (DESIGN.md §6.1).
//!
//! Any file that is neither a PDF nor a raster image is sniffed by content, not by
//! extension: if the first `MAX_BYTES` are valid UTF-8 without NUL bytes, the file is shown
//! as text. Only that prefix is ever read, so huge logs cost nothing. Loading happens on
//! worker threads; the UI only wraps the visible rows.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use unicode_width::UnicodeWidthChar;

use super::human_size;

/// How much of a file is read for the preview.
pub const MAX_BYTES: usize = 256 * 1024;
const TAB_STOP: usize = 4;

/// Read the head of `path` as text lines. `Ok(None)` means it is not text (binary or
/// empty), and the caller shows the usual card instead.
pub fn load(path: &Path) -> std::io::Result<Option<Vec<String>>> {
    let f = File::open(path)?;
    let total = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::with_capacity((total as usize).min(MAX_BYTES) + 1);
    f.take(MAX_BYTES as u64 + 1).read_to_end(&mut buf)?;
    let truncated = buf.len() > MAX_BYTES;
    buf.truncate(MAX_BYTES);
    Ok(parse(&buf, truncated, total))
}

fn parse(buf: &[u8], truncated: bool, total: u64) -> Option<Vec<String>> {
    if buf.is_empty() || buf.contains(&0) {
        return None;
    }
    let buf = buf.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(buf);
    let text = match std::str::from_utf8(buf) {
        Ok(s) => s,
        // The cut may have split a multi-byte character; that is not a decoding error.
        Err(e) if truncated && e.error_len().is_none() => {
            std::str::from_utf8(&buf[..e.valid_up_to()]).ok()?
        }
        Err(_) => return None,
    };
    let mut lines: Vec<String> = text.lines().map(sanitize).collect();
    if truncated {
        // The last line is probably cut short.
        if lines.len() > 1 {
            lines.pop();
        }
        lines.push(String::new());
        lines.push(format!(
            "… truncated: showing the first {} of {}",
            human_size(MAX_BYTES as u64),
            human_size(total.max(MAX_BYTES as u64))
        ));
    }
    Some(lines)
}

/// Expand tabs and replace control characters, which must never reach the terminal
/// (an ESC inside a cell would be interpreted as an escape sequence).
fn sanitize(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    for c in line.chars() {
        match c {
            '\t' => {
                let n = TAB_STOP - col % TAB_STOP;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\r' => {}
            c if c.is_control() => {
                out.push('\u{FFFD}');
                col += 1;
            }
            c => {
                out.push(c);
                col += c.width().unwrap_or(0);
            }
        }
    }
    out
}

/// Split one line into screen rows at most `width` cells wide (character wrap). Always
/// returns at least one row.
pub fn wrap(line: &str, width: usize) -> Vec<&str> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let (mut start, mut used) = (0, 0);
    for (i, c) in line.char_indices() {
        let w = c.width().unwrap_or(0);
        if used + w > width && i > start {
            rows.push(&line[start..i]);
            start = i;
            used = 0;
        }
        used += w;
    }
    rows.push(&line[start..]);
    rows
}

/// The largest first-line index that still fills the pane: scrolling further would only
/// show blank space below the end of the file.
pub fn max_scroll(lines: &[String], width: usize, height: usize) -> usize {
    let mut rows = 0;
    for (i, l) in lines.iter().enumerate().rev() {
        rows += wrap(l, width).len();
        if rows > height {
            return (i + 1).min(lines.len().saturating_sub(1));
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_split_and_sanitized() {
        let l = parse(b"a\tb\r\nx\x1b[31my\n\nlast", false, 0).unwrap();
        assert_eq!(l, ["a   b", "x\u{FFFD}[31my", "", "last"]);
        assert_eq!(parse(b"\xEF\xBB\xBFhi\n", false, 0).unwrap(), ["hi"]);
    }

    #[test]
    fn binary_and_empty_are_not_text() {
        assert!(parse(b"", false, 0).is_none());
        assert!(parse(b"ab\0cd", false, 0).is_none());
        assert!(parse(b"\xff\xfe\xfd", false, 0).is_none());
        // A character split by the cut is fine, but only when the file was cut.
        let cut = &"é".as_bytes()[..1];
        assert!(parse(cut, false, 1).is_none());
        let mut buf = b"one\ntw".to_vec();
        buf.extend_from_slice(cut);
        let l = parse(&buf, true, 1 << 20).unwrap();
        assert_eq!(l[0], "one");
        assert!(l.last().unwrap().contains("truncated"), "{l:?}");
        assert_eq!(l.len(), 3);
    }

    #[test]
    fn load_reads_only_the_head() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("big.log");
        std::fs::write(&p, "0123456789\n".repeat(MAX_BYTES / 11 + 100)).unwrap();
        let l = load(&p).unwrap().unwrap();
        assert!(l.last().unwrap().contains("truncated"));
        assert!(l.len() <= MAX_BYTES / 11 + 3);
        assert!(l[..l.len() - 2].iter().all(|s| s == "0123456789"));
    }

    #[test]
    fn wrapping_and_scroll_limit() {
        assert_eq!(wrap("", 5), [""]);
        assert_eq!(wrap("abcdefg", 3), ["abc", "def", "g"]);
        assert_eq!(wrap("日本語", 4), ["日本", "語"]);
        assert_eq!(wrap("日本", 1), ["日", "本"]); // too narrow: still progresses
        let lines: Vec<String> = ["1", "2", "abcdef", "4"].map(String::from).into();
        assert_eq!(max_scroll(&lines, 10, 10), 0);
        assert_eq!(max_scroll(&lines, 10, 2), 2);
        // "abcdef" takes two rows at width 3, so only it and "4" fit in three rows.
        assert_eq!(max_scroll(&lines, 3, 3), 2);
        assert_eq!(max_scroll(&lines, 3, 1), 3);
    }
}
