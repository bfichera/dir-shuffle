//! Breadth-first scan that finds the shallowest file level `D` (DESIGN.md §3.1).
//!
//! Each level is listed in full, and ignore rules are applied to it, before the next level
//! is read. The first level that contains a non-ignored regular file is `D`. Nothing deeper
//! is ever read, so even very large trees start up instantly.
//!
//! Ignore handling:
//! * `.ignore`, `.gitignore` and `.dir-shuffleignore` are honoured through the `ignore`
//!   crate's hierarchical matcher, including the ones in parent directories of the root,
//!   unless `respect-ignore-files = false`. `.gitignore` is honoured even outside a git repo.
//! * Patterns from the config (gitignore syntax) act as overrides. A matching pattern
//!   ignores the entry, and a negated (`!pattern`) match keeps it even if an ignore file
//!   would drop it.
//! * Hidden files are included. Version-control metadata directories (`.git`, `.hg`,
//!   `.svn`, `.jj`) are always skipped, because walking into them would wreck the level
//!   structure.
//! * When ignore files are respected, the ignore files themselves are not rows. Otherwise a
//!   `.gitignore` next to the outputs would collapse `D` to its level.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::{IncrementalIgnore, Match, WalkBuilder};

use crate::table::FileRecord;

pub const CUSTOM_IGNORE_FILE: &str = ".dir-shuffleignore";
const VCS_DIRS: &[&str] = &[".git", ".hg", ".svn", ".jj"];
/// Ignore files are rules, not outputs: when respected they never become rows.
const IGNORE_FILES: &[&str] = &[".ignore", ".gitignore", CUSTOM_IGNORE_FILE];
/// Safety net against pathological (e.g. symlink-generated) trees with no files at all.
const MAX_DEPTH: usize = 128;
/// Below this many directories per level, listing them on one thread is faster.
const PARALLEL_THRESHOLD: usize = 32;

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Config ignore patterns (gitignore syntax), already merged (§7).
    pub patterns: Vec<String>,
    /// Respect `.ignore` / `.gitignore` / `.dir-shuffleignore`.
    pub respect_ignore_files: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    /// The level of the shallowest non-ignored file, or `None` if there are no files.
    pub depth: Option<usize>,
    /// Files at depth `D`: `D` directory names plus the stem.
    pub files: Vec<FileRecord>,
    /// Directories at depth `D` (relative to the root). They and everything below them
    /// are skipped (§3.1.3).
    pub skipped_dirs: Vec<PathBuf>,
    /// Entries at depth ≤ `D` dropped by ignore rules (for the status line).
    pub ignored: usize,
    /// Non-fatal problems such as unreadable directories.
    pub errors: Vec<String>,
}

/// Matches entries against config patterns and ignore files.
#[derive(Clone)]
struct Filter {
    root: PathBuf,
    respect_ignore_files: bool,
    patterns: Gitignore,
    files: IncrementalIgnore,
}

impl Filter {
    fn new(root: &Path, opts: &ScanOptions) -> Result<Filter> {
        let mut gb = GitignoreBuilder::new(root);
        for p in &opts.patterns {
            gb.add_line(None, p)
                .with_context(|| format!("invalid ignore pattern `{p}`"))?;
        }
        let patterns = gb.build().context("building ignore patterns")?;

        let respect = opts.respect_ignore_files;
        let mut wb = WalkBuilder::new(root);
        wb.hidden(false)
            .parents(respect)
            .ignore(respect)
            .git_ignore(respect)
            .git_global(respect)
            .git_exclude(respect)
            .require_git(false);
        if respect {
            wb.add_custom_ignore_filename(CUSTOM_IGNORE_FILE);
        }
        let mut files = wb
            .build_matchers()
            .pop()
            .context("internal error: no ignore matcher")?;
        // Load the root's and its parents' ignore files once, so clones share them.
        let _ = files.matched("_", false);
        Ok(Filter {
            root: root.to_path_buf(),
            respect_ignore_files: respect,
            patterns,
            files,
        })
    }

    fn is_ignored(&mut self, rel: &Path, is_dir: bool) -> bool {
        match self.patterns.matched(self.root.join(rel), is_dir) {
            Match::Ignore(_) => return true,
            Match::Whitelist(_) => return false,
            Match::None => {}
        }
        self.files.matched(rel, is_dir).is_ignore()
    }
}

#[derive(Default)]
struct LevelOut {
    /// Non-ignored subdirectories, relative to the root.
    dirs: Vec<PathBuf>,
    /// Non-ignored regular files, relative to the root.
    files: Vec<PathBuf>,
    /// Canonical paths of symlinked directories (to break cycles).
    linked: Vec<PathBuf>,
    ignored: usize,
    errors: Vec<String>,
}

fn list_dirs(filter: &mut Filter, root: &Path, dirs: &[PathBuf]) -> LevelOut {
    let mut out = LevelOut::default();
    for rel in dirs {
        let abs = root.join(rel);
        let rd = match std::fs::read_dir(&abs) {
            Ok(rd) => rd,
            Err(e) => {
                out.errors.push(format!("{}: {e}", abs.display()));
                continue;
            }
        };
        for entry in rd {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    out.errors.push(format!("{}: {e}", abs.display()));
                    continue;
                }
            };
            let name = entry.file_name();
            let Ok(mut ft) = entry.file_type() else {
                continue;
            };
            let mut symlink_target = None;
            if ft.is_symlink() {
                // Follow symlinks. Broken links are skipped.
                match std::fs::metadata(entry.path()) {
                    Ok(md) => {
                        ft = md.file_type();
                        if ft.is_dir() {
                            symlink_target = std::fs::canonicalize(entry.path()).ok();
                        }
                    }
                    Err(_) => continue,
                }
            }
            let is_dir = ft.is_dir();
            if !is_dir && !ft.is_file() {
                continue; // sockets, FIFOs, devices
            }
            if is_dir && VCS_DIRS.iter().any(|v| OsStr::new(v) == name) {
                continue;
            }
            let child = rel.join(&name);
            let is_rule_file = !is_dir
                && filter.respect_ignore_files
                && IGNORE_FILES.iter().any(|f| OsStr::new(f) == name);
            if is_rule_file || filter.is_ignored(&child, is_dir) {
                out.ignored += 1;
                continue;
            }
            if is_dir {
                if let Some(t) = symlink_target {
                    out.linked.push(t);
                }
                out.dirs.push(child);
            } else {
                out.files.push(child);
            }
        }
    }
    out
}

fn list_level(filter: &Filter, root: &Path, dirs: &[PathBuf]) -> LevelOut {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(16);
    if dirs.len() < PARALLEL_THRESHOLD || threads <= 1 {
        return list_dirs(&mut filter.clone(), root, dirs);
    }
    let chunk = dirs.len().div_ceil(threads);
    let parts: Vec<LevelOut> = std::thread::scope(|s| {
        let handles: Vec<_> = dirs
            .chunks(chunk)
            .map(|c| {
                let mut f = filter.clone();
                s.spawn(move || list_dirs(&mut f, root, c))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let mut out = LevelOut::default();
    for p in parts {
        out.dirs.extend(p.dirs);
        out.files.extend(p.files);
        out.linked.extend(p.linked);
        out.ignored += p.ignored;
        out.errors.extend(p.errors);
    }
    out
}

fn record(root: &Path, rel: &Path) -> FileRecord {
    let comps: Vec<&OsStr> = rel.iter().collect();
    let (file, dirs) = comps.split_last().expect("file path has a name");
    let fp = Path::new(file);
    // `Path::file_stem` keeps dotfiles whole (`.bashrc`), and `extension` is `None` there.
    let stem = fp
        .file_stem()
        .unwrap_or(file)
        .to_string_lossy()
        .into_owned();
    let ext = fp
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut parts: Vec<String> = dirs
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect();
    parts.push(stem);
    FileRecord {
        parts,
        ext,
        path: root.join(rel),
    }
}

pub fn scan(root: &Path, opts: &ScanOptions) -> Result<ScanResult> {
    let md = std::fs::metadata(root).with_context(|| format!("{}", root.display()))?;
    if !md.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    let filter = Filter::new(root, opts)?;
    let mut res = ScanResult::default();
    let mut seen_links: HashSet<PathBuf> = HashSet::new();
    if let Ok(c) = std::fs::canonicalize(root) {
        seen_links.insert(c);
    }
    let mut level: Vec<PathBuf> = vec![PathBuf::new()];
    for depth in 0..=MAX_DEPTH {
        if level.is_empty() {
            break;
        }
        let out = list_level(&filter, root, &level);
        res.ignored += out.ignored;
        res.errors.extend(out.errors);
        if !out.files.is_empty() {
            res.depth = Some(depth);
            res.skipped_dirs = out.dirs.clone();
            res.files = out.files.iter().map(|rel| record(root, rel)).collect();
            return Ok(res);
        }
        // Drop symlinked directories that point somewhere already visited (cycles).
        let mut next = Vec::with_capacity(out.dirs.len());
        let linked: HashSet<&PathBuf> = out.linked.iter().collect();
        for d in out.dirs {
            if !linked.is_empty() {
                if let Ok(c) = std::fs::canonicalize(root.join(&d)) {
                    if linked.contains(&c) && !seen_links.insert(c) {
                        continue;
                    }
                }
            }
            next.push(d);
        }
        level = next;
    }
    Ok(res)
}

/// Count the non-ignored files below level `D`, i.e. inside `skipped_dirs`, using the same
/// ignore rules. This reads the rest of the tree, so the UI runs it in the background after
/// startup. Symlinked directories are not followed here, to keep the count finite.
pub fn count_deeper(root: &Path, opts: &ScanOptions, skipped_dirs: &[PathBuf]) -> usize {
    let Ok(filter) = Filter::new(root, opts) else {
        return 0;
    };
    let mut level: Vec<PathBuf> = skipped_dirs.to_vec();
    let mut count = 0;
    let mut seen: HashSet<PathBuf> = HashSet::new();
    while !level.is_empty() {
        let out = list_level(&filter, root, &level);
        count += out.files.len();
        let linked: HashSet<&PathBuf> = out.linked.iter().collect();
        level = out
            .dirs
            .into_iter()
            .filter(|d| {
                linked.is_empty()
                    || std::fs::canonicalize(root.join(d))
                        .map(|c| !linked.contains(&c) && seen.insert(c))
                        .unwrap_or(false)
            })
            .collect();
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::Table;
    use std::collections::BTreeMap;
    use std::fs;

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    fn opts(patterns: &[&str]) -> ScanOptions {
        ScanOptions {
            patterns: patterns.iter().map(|s| s.to_string()).collect(),
            respect_ignore_files: true,
        }
    }

    fn default_patterns() -> Vec<&'static str> {
        crate::config::DEFAULT_IGNORE.to_vec()
    }

    fn sweep(root: &Path) {
        for r in ["mean-rejected", "median-rejected"] {
            for t in ["300K", "364K", "1000K"] {
                for f in ["beta_hist.pdf", "tau.pdf"] {
                    touch(root, &format!("{r}/gaussian-normalized/{t}/{f}"));
                }
                touch(
                    root,
                    &format!("{r}/gaussian-normalized/{t}/g2_frames/frame_1.png"),
                );
            }
        }
    }

    #[test]
    fn finds_shallowest_level_and_skips_deeper() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sweep(root);
        let r = scan(root, &opts(&default_patterns())).unwrap();
        assert_eq!(r.depth, Some(3));
        assert_eq!(r.files.len(), 12);
        assert_eq!(r.skipped_dirs.len(), 6); // the g2_frames directories
        assert_eq!(
            count_deeper(root, &opts(&default_patterns()), &r.skipped_dirs),
            6
        );
        let t = Table::build(r.files, &BTreeMap::new());
        assert_eq!(t.dims.len(), 4);
        assert_eq!(t.dims[2].values, ["300K", "364K", "1000K"]);
        assert_eq!(t.dims[3].values, ["beta_hist", "tau"]);
        assert!(t.dims[1].is_constant());
        assert_eq!(t.free_candidates(), vec![0, 2, 3]);
    }

    #[test]
    fn uneven_depth_uses_shallowest_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sweep(root);
        touch(root, "mean-rejected/notes.txt");
        let r = scan(root, &opts(&default_patterns())).unwrap();
        assert_eq!(r.depth, Some(1));
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].parts, ["mean-rejected", "notes"]);
        assert_eq!(r.files[0].ext, "txt");
    }

    #[test]
    fn stray_ds_store_with_and_without_ignore_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sweep(root);
        touch(root, "mean-rejected/.DS_Store");
        touch(root, "._junk");

        // Default ignore patterns: the stray file must not change D.
        let r = scan(root, &opts(&default_patterns())).unwrap();
        assert_eq!(r.depth, Some(3));
        assert_eq!(r.files.len(), 12);
        assert_eq!(r.ignored, 2);

        // Without patterns, hidden files count: D collapses to 0 (`._junk`).
        let r = scan(root, &opts(&[])).unwrap();
        assert_eq!(r.depth, Some(0));
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].parts, ["._junk"]);

        fs::remove_file(root.join("._junk")).unwrap();
        let r = scan(root, &opts(&[])).unwrap();
        assert_eq!(r.depth, Some(1));
        assert_eq!(r.files[0].parts, ["mean-rejected", ".DS_Store"]);
        assert_eq!(r.files[0].ext, "");
    }

    #[test]
    fn ignore_files_are_respected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sweep(root);
        touch(root, "mean-rejected/stray.log");
        touch(root, "median-rejected/README");
        fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        fs::write(root.join("median-rejected/.dir-shuffleignore"), "README\n").unwrap();
        // The ignore files themselves are not rows, so they don't change D.
        let r = scan(root, &opts(&[])).unwrap();
        assert_eq!(r.depth, Some(3), "{:?}", r.files);

        // Turning ignore files off brings the strays back (and the rule files are plain
        // hidden files again, here hidden by config patterns).
        let pats = [".gitignore", ".dir-shuffleignore"];
        let mut o = opts(&pats);
        o.respect_ignore_files = false;
        let r = scan(root, &o).unwrap();
        assert_eq!(r.depth, Some(1));
        assert_eq!(r.files.len(), 2);
        o.patterns.clear();
        assert_eq!(scan(root, &o).unwrap().depth, Some(0));

        // A negated config pattern overrides the ignore files.
        let r = scan(root, &opts(&["!README"])).unwrap();
        assert_eq!(r.depth, Some(1));
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].parts, ["median-rejected", "README"]);
    }

    #[test]
    fn ignored_directories_are_not_descended() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sweep(root);
        touch(root, "logs/run.txt");
        touch(root, ".git/HEAD");
        touch(root, ".snakemake/x/y");
        let r = scan(root, &opts(&["logs/", ".snakemake"])).unwrap();
        assert_eq!(r.depth, Some(3));
    }

    #[test]
    fn same_stem_different_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "a/x.pdf");
        touch(root, "a/x.png");
        touch(root, "b/x.pdf");
        let r = scan(root, &opts(&[])).unwrap();
        let t = Table::build(r.files, &BTreeMap::new());
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.dims[1].values, ["x"]);
        assert_eq!(t.rows_for(&[0, 0]).len(), 2);
    }

    #[test]
    fn many_dirs_parallel_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for i in 0..200 {
            touch(root, &format!("p{i}/q/out.png"));
        }
        touch(root, "p7/q/.DS_Store");
        let r = scan(root, &opts(&default_patterns())).unwrap();
        assert_eq!(r.depth, Some(2));
        assert_eq!(r.files.len(), 200);
        assert_eq!(r.ignored, 1);
        let t = Table::build(r.files, &BTreeMap::new());
        assert_eq!(t.dims[0].values[..3], ["p0", "p1", "p2"]);
        assert_eq!(t.free_candidates(), vec![0]);
    }

    #[test]
    fn empty_tree() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("a/b")).unwrap();
        let r = scan(tmp.path(), &opts(&[])).unwrap();
        assert_eq!(r.depth, None);
        assert!(r.files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_cycles_terminate_and_links_are_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        std::os::unix::fs::symlink(root.join("a"), root.join("a/b/loop")).unwrap();
        let r = scan(root, &opts(&[])).unwrap();
        assert_eq!(r.depth, None);

        touch(root, "real/f.png");
        std::os::unix::fs::symlink(root.join("real/f.png"), root.join("a/g.png")).unwrap();
        let r = scan(root, &opts(&[])).unwrap();
        assert_eq!(r.depth, Some(1));
        assert_eq!(r.files.len(), 2);
    }
}
