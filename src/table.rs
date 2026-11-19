//! The table model: one row per output file, one column ("dimension") per directory
//! level plus the file stem (DESIGN.md §3).

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

pub type DimId = usize;
pub type ValueId = u32;
pub type RowId = u32;

#[derive(Debug, Clone)]
pub struct Dim {
    /// Cosmetic label (§3.4): inferred suffix/unit, `level N`, `file`, or a config override.
    pub label: String,
    /// Distinct values, naturally sorted. `ValueId` indexes into this.
    pub values: Vec<String>,
    /// Values with an inferred label affix stripped (e.g. `mean` for `mean-rejected`).
    /// Same length as `values`; identical to `values` when nothing was inferred.
    pub short: Vec<String>,
}

impl Dim {
    pub fn is_constant(&self) -> bool {
        self.values.len() <= 1
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    /// One `ValueId` per dimension (`D` directory levels, then the file stem).
    pub key: Box<[ValueId]>,
    pub path: PathBuf,
    /// Extension without the dot; empty if none. Not a dimension (§3.1.4).
    pub ext: String,
}

/// Rows are sorted by `(key, ext)`, so rows that differ only by extension are adjacent.
/// `index` maps a key to the first row with that key.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub dims: Vec<Dim>,
    pub rows: Vec<Row>,
    pub index: HashMap<Box<[ValueId]>, RowId>,
}

/// One scanned file, split into its dimension values.
#[derive(Debug, Clone)]
pub struct FileRecord {
    /// `D` directory names followed by the file stem.
    pub parts: Vec<String>,
    pub ext: String,
    pub path: PathBuf,
}

impl Table {
    /// Build a table from files that all have the same number of parts (`D + 1`).
    /// `label_overrides` maps level index to label (§7.2 `[labels]`).
    pub fn build(files: Vec<FileRecord>, label_overrides: &BTreeMap<usize, String>) -> Table {
        let Some(first) = files.first() else {
            return Table::default();
        };
        let ndims = first.parts.len();
        debug_assert!(files.iter().all(|f| f.parts.len() == ndims));

        // Intern values per dimension (provisional ids), then sort naturally and remap.
        let mut interners: Vec<HashMap<String, u32>> = vec![HashMap::new(); ndims];
        let mut raw_keys: Vec<Vec<u32>> = Vec::with_capacity(files.len());
        for f in &files {
            let mut k = Vec::with_capacity(ndims);
            for (d, part) in f.parts.iter().enumerate() {
                let interner = &mut interners[d];
                let next = interner.len() as u32;
                k.push(*interner.entry(part.clone()).or_insert(next));
            }
            raw_keys.push(k);
        }

        let mut dims = Vec::with_capacity(ndims);
        let mut remaps: Vec<Vec<u32>> = Vec::with_capacity(ndims);
        for (d, interner) in interners.into_iter().enumerate() {
            let mut vals: Vec<(String, u32)> = interner.into_iter().collect();
            vals.sort_by(|a, b| natural_cmp(&a.0, &b.0));
            let mut remap = vec![0u32; vals.len()];
            for (new_id, (_, old_id)) in vals.iter().enumerate() {
                remap[*old_id as usize] = new_id as u32;
            }
            let values: Vec<String> = vals.into_iter().map(|(s, _)| s).collect();
            let is_file_dim = d + 1 == ndims;
            let (inferred, short) = if is_file_dim {
                (None, values.clone())
            } else {
                match infer_label(&values) {
                    Some((label, short)) => (Some(label), short),
                    None => (None, values.clone()),
                }
            };
            // An overridden label shows full values: stripping an inferred affix would
            // hide information, e.g. `temp=300` instead of `temp=300K`.
            let short = if label_overrides.contains_key(&d) {
                values.clone()
            } else {
                short
            };
            let label = label_overrides
                .get(&d)
                .cloned()
                .or(inferred)
                .unwrap_or_else(|| {
                    if is_file_dim {
                        "file".to_string()
                    } else {
                        format!("level {d}")
                    }
                });
            dims.push(Dim {
                label,
                values,
                short,
            });
            remaps.push(remap);
        }

        let mut rows: Vec<Row> = files
            .into_iter()
            .zip(raw_keys)
            .map(|(f, raw)| Row {
                key: raw
                    .iter()
                    .enumerate()
                    .map(|(d, &v)| remaps[d][v as usize])
                    .collect(),
                path: f.path,
                ext: f.ext,
            })
            .collect();
        rows.sort_by(|a, b| {
            a.key
                .cmp(&b.key)
                .then_with(|| natural_cmp(&a.ext, &b.ext))
                .then_with(|| a.path.cmp(&b.path))
        });

        let mut index = HashMap::with_capacity(rows.len());
        for (i, r) in rows.iter().enumerate() {
            index.entry(r.key.clone()).or_insert(i as RowId);
        }
        Table { dims, rows, index }
    }

    /// All rows with exactly this key (more than one only if files differ by extension).
    pub fn rows_for(&self, key: &[ValueId]) -> std::ops::Range<RowId> {
        match self.index.get(key) {
            None => 0..0,
            Some(&start) => {
                let mut end = start + 1;
                while (end as usize) < self.rows.len() && &*self.rows[end as usize].key == key {
                    end += 1;
                }
                start..end
            }
        }
    }

    pub fn row(&self, id: RowId) -> &Row {
        &self.rows[id as usize]
    }

    /// Path of a row relative to the scan root, as `a/b/c/file.ext` (for display).
    pub fn rel_display(&self, id: RowId) -> String {
        let row = self.row(id);
        let mut s = String::new();
        for (d, &v) in row.key.iter().enumerate() {
            if d > 0 {
                s.push('/');
            }
            s.push_str(&self.dims[d].values[v as usize]);
        }
        if !row.ext.is_empty() {
            s.push('.');
            s.push_str(&row.ext);
        }
        s
    }

    /// Dimensions that can be chosen as the free axis (§3.5: not constant).
    pub fn free_candidates(&self) -> Vec<DimId> {
        (0..self.dims.len())
            .filter(|&d| !self.dims[d].is_constant())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Natural sort (§3.3)
// ---------------------------------------------------------------------------

enum Tok<'a> {
    Num { int: &'a str, frac: &'a str },
    Text(&'a str),
}

fn next_tok(s: &str) -> Option<(Tok<'_>, &str)> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    if b[0].is_ascii_digit() {
        let mut i = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let int = &s[..i];
        // A single decimal point followed by digits: compare as a decimal, so 0.25 < 0.5.
        if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            return Some((
                Tok::Num {
                    int,
                    frac: &s[i + 1..j],
                },
                &s[j..],
            ));
        }
        Some((Tok::Num { int, frac: "" }, &s[i..]))
    } else {
        let mut i = 0;
        while i < b.len() && !b[i].is_ascii_digit() {
            i += 1;
        }
        // `i` is at a char boundary: ASCII digits never occur inside multi-byte chars.
        Some((Tok::Text(&s[..i]), &s[i..]))
    }
}

fn cmp_decimal(ai: &str, af: &str, bi: &str, bf: &str) -> Ordering {
    let ai = ai.trim_start_matches('0');
    let bi = bi.trim_start_matches('0');
    ai.len()
        .cmp(&bi.len())
        .then_with(|| ai.cmp(bi))
        .then_with(|| {
            let af = af.trim_end_matches('0');
            let bf = bf.trim_end_matches('0');
            af.cmp(bf)
        })
}

/// Natural ordering: digit runs compare numerically (`300K < 1000K`, `frame_2 < frame_10`),
/// including simple decimals (`0.25 < 0.5`). Ties are broken by plain string order so this
/// is a total order consistent with equality.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a, b);
    loop {
        match (next_tok(x), next_tok(y)) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some((tx, rx)), Some((ty, ry))) => {
                let o = match (tx, ty) {
                    (Tok::Num { int: ai, frac: af }, Tok::Num { int: bi, frac: bf }) => {
                        cmp_decimal(ai, af, bi, bf)
                    }
                    (Tok::Num { .. }, Tok::Text(_)) => Ordering::Less,
                    (Tok::Text(_), Tok::Num { .. }) => Ordering::Greater,
                    (Tok::Text(p), Tok::Text(q)) => p.cmp(q),
                };
                if o != Ordering::Equal {
                    return o;
                }
                x = rx;
                y = ry;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Label inference (§3.4, cosmetic only)
// ---------------------------------------------------------------------------

fn is_sep(c: char) -> bool {
    matches!(c, '-' | '_' | '.' | ' ' | '=' | '+' | ',')
}

/// Infer a label for a directory level from its values. Returns the label and the values
/// with the label affix removed.
///
/// * Common suffix after a separator: `mean-rejected`, `median-rejected` → `rejected`.
/// * Common unit suffix on numbers: `300K`, `1000K` → `K`.
/// * Hive-style common prefix: `T=300`, `T=400` → `T`.
pub fn infer_label(values: &[String]) -> Option<(String, Vec<String>)> {
    if values.is_empty() {
        return None;
    }
    suffix_after_separator(values)
        .or_else(|| unit_suffix(values))
        .or_else(|| hive_prefix(values))
}

fn common_suffix_chars(values: &[String]) -> String {
    let first: Vec<char> = values[0].chars().collect();
    let mut n = first.len();
    for v in &values[1..] {
        let common = v
            .chars()
            .rev()
            .zip(first.iter().rev())
            .take_while(|(a, b)| a == *b)
            .count();
        n = n.min(common);
    }
    first[first.len() - n..].iter().collect()
}

fn suffix_after_separator(values: &[String]) -> Option<(String, Vec<String>)> {
    let common = common_suffix_chars(values);
    // The first separator inside the common suffix starts the label.
    let (pos, sep) = common.char_indices().find(|&(_, c)| is_sep(c))?;
    let label = &common[pos + sep.len_utf8()..];
    if label.is_empty() || !label.chars().any(|c| c.is_alphabetic()) {
        return None;
    }
    let strip = common.len() - pos; // bytes of "<sep><label>"
    let short: Vec<String> = values
        .iter()
        .map(|v| v[..v.len() - strip].to_string())
        .collect();
    if short.iter().any(|s| s.is_empty()) {
        return None;
    }
    Some((label.to_string(), short))
}

fn split_number_unit(v: &str) -> Option<(&str, &str)> {
    let unit_start = v
        .char_indices()
        .rev()
        .take_while(|&(_, c)| c.is_alphabetic() || c == '%')
        .last()
        .map(|(i, _)| i)?;
    let (num, unit) = v.split_at(unit_start);
    let digits = num.strip_prefix('-').unwrap_or(num);
    let mut parts = digits.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next();
    let ok = !int.is_empty()
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()));
    ok.then_some((num, unit))
}

fn unit_suffix(values: &[String]) -> Option<(String, Vec<String>)> {
    let (_, unit) = split_number_unit(&values[0])?;
    let mut short = Vec::with_capacity(values.len());
    for v in values {
        let (num, u) = split_number_unit(v)?;
        if u != unit {
            return None;
        }
        short.push(num.to_string());
    }
    Some((unit.to_string(), short))
}

fn hive_prefix(values: &[String]) -> Option<(String, Vec<String>)> {
    let (key, _) = values[0].split_once('=')?;
    if key.is_empty() {
        return None;
    }
    let mut short = Vec::with_capacity(values.len());
    for v in values {
        let (k, rest) = v.split_once('=')?;
        if k != key || rest.is_empty() {
            return None;
        }
        short.push(rest.to_string());
    }
    Some((key.to_string(), short))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut v: Vec<&str>) -> Vec<&str> {
        v.sort_by(|a, b| natural_cmp(a, b));
        v
    }

    #[test]
    fn natural_sort_examples() {
        assert_eq!(
            sorted(vec!["1000K", "300K", "364K"]),
            ["300K", "364K", "1000K"]
        );
        assert_eq!(
            sorted(vec!["frame_10", "frame_2", "frame_1"]),
            ["frame_1", "frame_2", "frame_10"]
        );
        assert_eq!(
            sorted(vec!["0.5", "0.25", "0.125", "1"]),
            ["0.125", "0.25", "0.5", "1"]
        );
        assert_eq!(
            sorted(vec!["0.1", "0.01", "0.001"]),
            ["0.001", "0.01", "0.1"]
        );
        assert_eq!(sorted(vec!["b", "a10", "a9", "A"]), ["A", "a9", "a10", "b"]);
        // Total order: numerically-equal strings are still distinguished.
        assert_eq!(natural_cmp("01", "1"), "01".cmp("1"));
        assert_ne!(natural_cmp("1.0", "1"), Ordering::Equal);
        // Non-ASCII text does not panic.
        assert_eq!(sorted(vec!["é2", "é10"]), ["é2", "é10"]);
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn label_inference() {
        let (l, short) = infer_label(&s(&["mean-rejected", "median-rejected"])).unwrap();
        assert_eq!(l, "rejected");
        assert_eq!(short, ["mean", "median"]);

        let (l, short) = infer_label(&s(&["gaussian-normalized"])).unwrap();
        assert_eq!(l, "normalized");
        assert_eq!(short, ["gaussian"]);

        let (l, short) = infer_label(&s(&["300K", "364K", "1000K"])).unwrap();
        assert_eq!(l, "K");
        assert_eq!(short, ["300", "364", "1000"]);

        let (l, short) = infer_label(&s(&["T=1.5", "T=2"])).unwrap();
        assert_eq!(l, "T");
        assert_eq!(short, ["1.5", "2"]);

        assert!(infer_label(&s(&["alpha", "beta"])).is_none());
        assert!(infer_label(&s(&["a_1", "b_1"])).is_none()); // numeric "label" rejected
        assert!(infer_label(&s(&["300K", "300mK"])).is_none());
    }

    fn rec(parts: &[&str], ext: &str) -> FileRecord {
        let mut p = parts.join("/");
        if !ext.is_empty() {
            p.push('.');
            p.push_str(ext);
        }
        FileRecord {
            parts: s(parts),
            ext: ext.to_string(),
            path: PathBuf::from(p),
        }
    }

    #[test]
    fn build_table_and_index() {
        let files = vec![
            rec(&["mean-rejected", "1000K", "beta"], "pdf"),
            rec(&["mean-rejected", "300K", "beta"], "pdf"),
            rec(&["mean-rejected", "300K", "beta"], "png"),
            rec(&["mean-rejected", "300K", "tau"], "pdf"),
        ];
        let t = Table::build(files, &BTreeMap::new());
        assert_eq!(t.dims.len(), 3);
        assert_eq!(t.dims[0].label, "rejected");
        assert!(t.dims[0].is_constant());
        assert_eq!(t.dims[1].label, "K");
        assert_eq!(t.dims[1].values, ["300K", "1000K"]);
        assert_eq!(t.dims[2].label, "file");
        assert_eq!(t.free_candidates(), vec![1, 2]);

        let r = t.rows_for(&[0, 0, 0]);
        assert_eq!(r.len(), 2);
        assert_eq!(t.row(r.start).ext, "pdf");
        assert_eq!(t.row(r.start + 1).ext, "png");
        assert_eq!(t.rows_for(&[0, 1, 1]).len(), 0);
        assert_eq!(t.rel_display(r.start), "mean-rejected/300K/beta.pdf");
    }

    #[test]
    fn label_overrides() {
        let mut o = BTreeMap::new();
        o.insert(0usize, "reject".to_string());
        o.insert(1usize, "name".to_string());
        let t = Table::build(vec![rec(&["mean-rejected", "x"], "pdf")], &o);
        assert_eq!(t.dims[0].label, "reject");
        assert_eq!(t.dims[0].short, ["mean-rejected"]); // overridden: full values
        assert_eq!(t.dims[1].label, "name");
    }
}
