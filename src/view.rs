//! Views: a slice of the table with one free dimension (DESIGN.md §4).
//!
//! A view is `(free, fixed)`. The slice is built by swapping every value of the free
//! dimension into `fixed` and looking the key up in `Table.index`, which is O(values).
//! Values with no file in the slice are left out (§3.6). Each matching row becomes one
//! entry, so files that differ only by extension appear side by side.

use crate::table::{DimId, RowId, Table, ValueId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// Value of the free dimension.
    pub value: ValueId,
    pub row: RowId,
}

#[derive(Debug, Clone)]
pub struct View {
    pub free: DimId,
    /// One value per dimension. The entry for `free` is ignored while it is free.
    pub fixed: Vec<ValueId>,
    pub entries: Vec<Entry>,
    pub selected: usize,
    /// Fixed dimension focused in the header (`Tab`/`h`/`l`/`f` act on it).
    pub focus: Option<DimId>,
}

/// Build the slice for `free` with the other dimensions fixed to `fixed`.
pub fn slice(table: &Table, free: DimId, fixed: &[ValueId]) -> Vec<Entry> {
    let mut key: Box<[ValueId]> = fixed.into();
    let mut out = Vec::new();
    for v in 0..table.dims[free].values.len() as ValueId {
        key[free] = v;
        for row in table.rows_for(&key) {
            out.push(Entry { value: v, row });
        }
    }
    out
}

fn slice_nonempty(table: &Table, free: DimId, fixed: &[ValueId]) -> bool {
    let mut key: Box<[ValueId]> = fixed.into();
    (0..table.dims[free].values.len() as ValueId).any(|v| {
        key[free] = v;
        table.index.contains_key(&key)
    })
}

impl View {
    /// A view with free dimension `free`, fixed to the key of `base` and with `base` selected.
    pub fn new(table: &Table, free: DimId, base: RowId) -> View {
        let fixed = table.row(base).key.to_vec();
        let entries = slice(table, free, &fixed);
        let selected = entries.iter().position(|e| e.row == base).unwrap_or(0);
        let mut v = View {
            free,
            fixed,
            entries,
            selected,
            focus: None,
        };
        v.focus = v.focusable(table).first().copied();
        v
    }

    pub fn selected_entry(&self) -> Option<Entry> {
        self.entries.get(self.selected).copied()
    }

    pub fn selected_row(&self) -> Option<RowId> {
        self.selected_entry().map(|e| e.row)
    }

    /// Fixed dimensions with more than one value, in order (§3.5: constant ones are context).
    pub fn focusable(&self, table: &Table) -> Vec<DimId> {
        (0..table.dims.len())
            .filter(|&d| d != self.free && !table.dims[d].is_constant())
            .collect()
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let max = self.entries.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub fn first(&mut self) {
        self.selected = 0;
    }

    pub fn last(&mut self) {
        self.selected = self.entries.len().saturating_sub(1);
    }

    pub fn focus_step(&mut self, table: &Table, dir: isize) {
        let f = self.focusable(table);
        if f.is_empty() {
            self.focus = None;
            return;
        }
        let n = f.len() as isize;
        let next = match self.focus.and_then(|d| f.iter().position(|&x| x == d)) {
            Some(i) => (i as isize + dir).rem_euclid(n),
            None => {
                if dir >= 0 {
                    0
                } else {
                    n - 1
                }
            }
        };
        self.focus = Some(f[next as usize]);
    }

    /// Rebuild the slice after `fixed` changed and try to keep the selection on the same
    /// free value (and extension), falling back to the nearest value.
    fn rebuild_keep(&mut self, table: &Table) {
        let old = self.selected_entry();
        let old_ext = old.map(|e| table.row(e.row).ext.clone());
        self.entries = slice(table, self.free, &self.fixed);
        self.selected = match old {
            None => 0,
            Some(o) => {
                let same_val = |e: &Entry| e.value == o.value;
                self.entries
                    .iter()
                    .position(|e| same_val(e) && Some(&table.row(e.row).ext) == old_ext.as_ref())
                    .or_else(|| self.entries.iter().position(same_val))
                    .or_else(|| {
                        self.entries
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, e)| (e.value as i64 - o.value as i64).abs())
                            .map(|(i, _)| i)
                    })
                    .unwrap_or(0)
            }
        };
    }

    /// Change the focused fixed dimension's value by `dir` steps (±1). Values whose slice
    /// would be empty are skipped. Returns `false` if nothing changed.
    pub fn step_focused(&mut self, table: &Table, dir: isize) -> bool {
        let Some(d) = self.focus else { return false };
        let n = table.dims[d].values.len() as isize;
        let mut v = self.fixed[d] as isize;
        let mut probe = self.fixed.clone();
        loop {
            v += dir.signum();
            if v < 0 || v >= n {
                return false;
            }
            probe[d] = v as ValueId;
            if slice_nonempty(table, self.free, &probe) {
                break;
            }
        }
        self.fixed = probe;
        self.rebuild_keep(table);
        true
    }

    /// Make dimension `d` free. The old free dimension is fixed at the selected value and
    /// the selection stays on the same file (§4 pivot; also used by the view picker).
    pub fn set_free(&mut self, table: &Table, d: DimId) {
        if d == self.free {
            return;
        }
        let old_free = self.free;
        let Some(row) = self.selected_row() else {
            return;
        };
        self.fixed = table.row(row).key.to_vec();
        self.free = d;
        self.entries = slice(table, d, &self.fixed);
        self.selected = self.entries.iter().position(|e| e.row == row).unwrap_or(0);
        let f = self.focusable(table);
        self.focus = if f.contains(&old_free) {
            Some(old_free)
        } else if self.focus.is_some_and(|x| f.contains(&x)) {
            self.focus
        } else {
            f.first().copied()
        };
    }

    /// Pivot: the focused fixed dimension becomes free.
    pub fn pivot(&mut self, table: &Table) -> bool {
        match self.focus {
            Some(d) => {
                self.set_free(table, d);
                true
            }
            None => false,
        }
    }

    /// Rows within ±1…±k of the selection along the free axis, nearest first.
    pub fn free_neighbors(&self, k: usize) -> Vec<RowId> {
        let mut out = Vec::with_capacity(2 * k);
        let s = self.selected as isize;
        for i in 1..=k as isize {
            for j in [s + i, s - i] {
                if j >= 0 && (j as usize) < self.entries.len() {
                    out.push(self.entries[j as usize].row);
                }
            }
        }
        out
    }

    /// Rows within ±1…±k along the focused fixed dimension (same free value, preferring the
    /// same extension), nearest first. Only values with an existing file count as steps.
    pub fn focused_neighbors(&self, table: &Table, k: usize) -> Vec<RowId> {
        let (Some(d), Some(sel)) = (self.focus, self.selected_entry()) else {
            return Vec::new();
        };
        let row = table.row(sel.row);
        let n = table.dims[d].values.len() as isize;
        let mut per_dir: [Vec<RowId>; 2] = [Vec::new(), Vec::new()];
        for (slot, dir) in [(0usize, 1isize), (1, -1)] {
            let mut key = row.key.clone();
            let mut v = row.key[d] as isize;
            while per_dir[slot].len() < k {
                v += dir;
                if v < 0 || v >= n {
                    break;
                }
                key[d] = v as ValueId;
                let range = table.rows_for(&key);
                if range.is_empty() {
                    continue;
                }
                let pick = range
                    .clone()
                    .find(|&r| table.row(r).ext == row.ext)
                    .unwrap_or(range.start);
                per_dir[slot].push(pick);
            }
        }
        let mut out = Vec::new();
        for i in 0..k {
            for dir in &per_dir {
                if let Some(&r) = dir.get(i) {
                    out.push(r);
                }
            }
        }
        out
    }

    /// Whether the list needs to show extensions: some value has more than one file.
    pub fn has_ext_duplicates(&self) -> bool {
        self.entries.windows(2).any(|w| w[0].value == w[1].value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{FileRecord, Table};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn table(paths: &[&str]) -> Table {
        let files = paths
            .iter()
            .map(|p| {
                let (stem_path, ext) = match p.rsplit_once('.') {
                    Some((a, b)) => (a, b),
                    None => (*p, ""),
                };
                FileRecord {
                    parts: stem_path.split('/').map(String::from).collect(),
                    ext: ext.to_string(),
                    path: PathBuf::from(p),
                }
            })
            .collect();
        Table::build(files, &BTreeMap::new())
    }

    fn values(t: &Table, v: &View) -> Vec<String> {
        v.entries
            .iter()
            .map(|e| t.dims[v.free].values[e.value as usize].clone())
            .collect()
    }

    fn sel_path(t: &Table, v: &View) -> String {
        t.rel_display(v.selected_row().unwrap())
    }

    fn sweep() -> Table {
        let mut p = Vec::new();
        for r in ["mean", "median"] {
            for t in ["300K", "364K", "1000K"] {
                for f in ["beta", "tau"] {
                    p.push(format!("{r}/{t}/{f}.pdf"));
                }
            }
        }
        // Sparse: median/1000K has no tau.
        p.retain(|s| s != "median/1000K/tau.pdf");
        let refs: Vec<&str> = p.iter().map(|s| s.as_str()).collect();
        table(&refs)
    }

    #[test]
    fn slicing_and_moving() {
        let t = sweep();
        let base = t.index[&[0u32, 0, 0][..]];
        let mut v = View::new(&t, 1, base);
        assert_eq!(values(&t, &v), ["300K", "364K", "1000K"]);
        assert_eq!(v.focusable(&t), vec![0, 2]);
        assert_eq!(v.focus, Some(0));
        v.move_by(5);
        assert_eq!(sel_path(&t, &v), "mean/1000K/beta.pdf");
        v.move_by(-1);
        assert_eq!(sel_path(&t, &v), "mean/364K/beta.pdf");
        v.first();
        assert_eq!(v.selected, 0);
        v.last();
        assert_eq!(v.selected, 2);
    }

    #[test]
    fn stepping_keeps_other_choices_and_selection() {
        let t = sweep();
        let base = t.index[&[0u32, 2, 1][..]]; // mean/1000K/tau
        let mut v = View::new(&t, 1, base);
        assert_eq!(sel_path(&t, &v), "mean/1000K/tau.pdf");
        // Switch reject to median: 1000K/tau is missing there, so the slice has 2 entries
        // and the selection falls back to the nearest value.
        assert!(v.step_focused(&t, 1));
        assert_eq!(values(&t, &v), ["300K", "364K"]);
        assert_eq!(sel_path(&t, &v), "median/364K/tau.pdf");
        assert!(!v.step_focused(&t, 1)); // no further value
        assert!(v.step_focused(&t, -1));
        assert_eq!(sel_path(&t, &v), "mean/364K/tau.pdf");
        // Focus the file dimension and step it.
        v.focus_step(&t, 1);
        assert_eq!(v.focus, Some(2));
        v.focus_step(&t, 1);
        assert_eq!(v.focus, Some(0)); // wraps
        v.focus_step(&t, -1);
        assert!(v.step_focused(&t, -1));
        assert_eq!(sel_path(&t, &v), "mean/364K/beta.pdf");
    }

    #[test]
    fn pivot_keeps_selection() {
        let t = sweep();
        let base = t.index[&[1u32, 1, 0][..]]; // median/364K/beta
        let mut v = View::new(&t, 1, base);
        v.focus = Some(2);
        assert!(v.pivot(&t));
        assert_eq!(v.free, 2);
        assert_eq!(v.fixed[1], 1); // old free fixed at selected value
        assert_eq!(sel_path(&t, &v), "median/364K/beta.pdf");
        assert_eq!(values(&t, &v), ["beta", "tau"]);
        assert_eq!(v.focus, Some(1)); // focus moves to the old free dimension
        v.set_free(&t, 0);
        assert_eq!(sel_path(&t, &v), "median/364K/beta.pdf");
        assert_eq!(values(&t, &v), ["mean", "median"]);
    }

    #[test]
    fn neighbors() {
        let t = sweep();
        let base = t.index[&[0u32, 1, 0][..]]; // mean/364K/beta
        let v = View::new(&t, 1, base);
        let n: Vec<String> = v
            .free_neighbors(5)
            .iter()
            .map(|&r| t.rel_display(r))
            .collect();
        assert_eq!(n, ["mean/1000K/beta.pdf", "mean/300K/beta.pdf"]);
        let n: Vec<String> = v
            .focused_neighbors(&t, 2)
            .iter()
            .map(|&r| t.rel_display(r))
            .collect();
        assert_eq!(n, ["median/364K/beta.pdf"]);
    }

    #[test]
    fn extension_duplicates_are_separate_entries() {
        let t = table(&["a/x.pdf", "a/x.png", "b/x.pdf"]);
        let v = View::new(&t, 0, 0);
        assert_eq!(v.entries.len(), 3);
        assert!(v.has_ext_duplicates());
        assert!(v.focusable(&t).is_empty());
        assert_eq!(v.focus, None);
    }

    #[test]
    fn single_dimension_table() {
        let t = table(&["b.png", "a.png", "c.txt"]);
        let v = View::new(&t, 0, 0);
        assert_eq!(values(&t, &v), ["a", "b", "c"]);
    }
}
