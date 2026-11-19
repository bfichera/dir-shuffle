//! Configuration discovery, layered merging and the `config` subcommands (DESIGN.md §7).
//!
//! Precedence, highest wins: CLI flags > project config > global config > built-in defaults.
//! Scalars and `ignore` from a higher layer replace lower ones; `extend-ignore` lists from all
//! layers are concatenated; `[labels]` merge per level index.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

pub const DEFAULT_IGNORE: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini", "._*"];
pub const DEFAULT_PREFETCH: usize = 5;
pub const DEFAULT_CACHE_MB: u64 = 256;
pub const PROJECT_FILE: &str = ".dir-shuffle.toml";
pub const PYPROJECT_FILE: &str = "pyproject.toml";
pub const PYPROJECT_TABLE: &str = "dir-shuffle";

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// A label key in `[labels]`: a level index written as a TOML key (`0 = "reject"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LevelKey(pub usize);

impl<'de> Deserialize<'de> for LevelKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = LevelKey;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a level index (0, 1, 2, ...)")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<LevelKey, E> {
                s.trim().parse::<usize>().map(LevelKey).map_err(|_| {
                    E::custom(format!(
                        "label keys must be level indices (0, 1, 2, ...), got `{s}`"
                    ))
                })
            }
            fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<LevelKey, E> {
                Ok(LevelKey(n as usize))
            }
            fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<LevelKey, E> {
                usize::try_from(n)
                    .map(LevelKey)
                    .map_err(|_| E::custom("label keys must be non-negative level indices"))
            }
        }
        d.deserialize_str(V)
    }
}

/// The keys of one config file (identical in `.dir-shuffle.toml`, the global config and
/// `[tool.dir-shuffle]` of `pyproject.toml`). Unknown keys are rejected to catch typos.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Layer {
    pub ignore: Option<Vec<String>>,
    pub extend_ignore: Option<Vec<String>>,
    pub respect_ignore_files: Option<bool>,
    pub prefetch: Option<usize>,
    pub cache_mb: Option<u64>,
    pub labels: Option<BTreeMap<LevelKey, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Default,
    Global(PathBuf),
    Project(PathBuf),
    Cli,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Source::Default => write!(f, "default"),
            Source::Global(p) => write!(f, "global: {}", p.display()),
            Source::Project(p) => write!(f, "project: {}", p.display()),
            Source::Cli => write!(f, "command line"),
        }
    }
}

/// Settings given on the command line (§8).
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub prefetch: Option<usize>,
    pub cache_mb: Option<u64>,
    pub no_ignore: bool,
    pub no_ignore_files: bool,
}

/// The merged configuration, with the source of every value.
#[derive(Debug, Clone)]
pub struct Config {
    pub ignore: Vec<String>,
    pub ignore_src: Source,
    pub extend_ignore: Vec<(String, Source)>,
    /// `false` with `--no-ignore`: the patterns above are not applied.
    pub use_ignore_patterns: bool,
    pub respect_ignore_files: bool,
    pub respect_src: Source,
    pub prefetch: usize,
    pub prefetch_src: Source,
    pub cache_mb: u64,
    pub cache_src: Source,
    pub labels: BTreeMap<usize, (String, Source)>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            ignore: DEFAULT_IGNORE.iter().map(|s| s.to_string()).collect(),
            ignore_src: Source::Default,
            extend_ignore: Vec::new(),
            use_ignore_patterns: true,
            respect_ignore_files: true,
            respect_src: Source::Default,
            prefetch: DEFAULT_PREFETCH,
            prefetch_src: Source::Default,
            cache_mb: DEFAULT_CACHE_MB,
            cache_src: Source::Default,
            labels: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Merge layers given in increasing precedence (e.g. `[global, project]`) over the
    /// defaults, then apply CLI overrides.
    pub fn merge(layers: &[(Layer, Source)], cli: &CliOverrides) -> Config {
        let mut c = Config::default();
        for (layer, src) in layers {
            if let Some(v) = &layer.ignore {
                c.ignore = v.clone();
                c.ignore_src = src.clone();
            }
            if let Some(v) = &layer.extend_ignore {
                c.extend_ignore
                    .extend(v.iter().map(|p| (p.clone(), src.clone())));
            }
            if let Some(v) = layer.respect_ignore_files {
                c.respect_ignore_files = v;
                c.respect_src = src.clone();
            }
            if let Some(v) = layer.prefetch {
                c.prefetch = v;
                c.prefetch_src = src.clone();
            }
            if let Some(v) = layer.cache_mb {
                c.cache_mb = v;
                c.cache_src = src.clone();
            }
            if let Some(labels) = &layer.labels {
                for (k, v) in labels {
                    c.labels.insert(k.0, (v.clone(), src.clone()));
                }
            }
        }
        if let Some(v) = cli.prefetch {
            c.prefetch = v;
            c.prefetch_src = Source::Cli;
        }
        if let Some(v) = cli.cache_mb {
            c.cache_mb = v;
            c.cache_src = Source::Cli;
        }
        if cli.no_ignore {
            c.use_ignore_patterns = false;
        }
        if cli.no_ignore_files {
            c.respect_ignore_files = false;
            c.respect_src = Source::Cli;
        }
        c
    }

    /// The gitignore-style patterns applied during the scan: `ignore` + all `extend-ignore`.
    pub fn ignore_patterns(&self) -> Vec<String> {
        if !self.use_ignore_patterns {
            return Vec::new();
        }
        self.ignore
            .iter()
            .cloned()
            .chain(self.extend_ignore.iter().map(|(p, _)| p.clone()))
            .collect()
    }

    pub fn label_overrides(&self) -> BTreeMap<usize, String> {
        self.labels
            .iter()
            .map(|(k, (v, _))| (*k, v.clone()))
            .collect()
    }

    pub fn cache_bytes(&self) -> usize {
        (self.cache_mb.max(1) as usize).saturating_mul(1024 * 1024)
    }
}

// ---------------------------------------------------------------------------
// Parsing with line-numbered errors
// ---------------------------------------------------------------------------

fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

fn format_toml_error(path: &Path, text: &str, e: &toml::de::Error) -> anyhow::Error {
    match e.span() {
        Some(span) => {
            let (line, col) = line_col(text, span.start);
            let src_line = text.lines().nth(line - 1).unwrap_or("");
            anyhow::anyhow!(
                "{}:{line}:{col}: {}\n    {line} | {src_line}",
                path.display(),
                e.message().trim()
            )
        }
        None => anyhow::anyhow!("{}: {}", path.display(), e.message().trim()),
    }
}

#[derive(Deserialize)]
struct PyProject {
    tool: Option<PyTool>,
}

#[derive(Deserialize)]
struct PyTool {
    #[serde(rename = "dir-shuffle")]
    dir_shuffle: Option<Layer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    /// `.dir-shuffle.toml` or the global `config.toml`: keys at top level.
    Plain,
    /// `pyproject.toml`: keys under `[tool.dir-shuffle]`.
    Pyproject,
}

/// Parse config text. For `Pyproject`, returns `None` if there is no `[tool.dir-shuffle]`.
pub fn parse_layer(path: &Path, text: &str, kind: ConfigKind) -> Result<Option<Layer>> {
    match kind {
        ConfigKind::Plain => toml::from_str::<Layer>(text)
            .map(Some)
            .map_err(|e| format_toml_error(path, text, &e)),
        ConfigKind::Pyproject => toml::from_str::<PyProject>(text)
            .map(|p| p.tool.and_then(|t| t.dir_shuffle))
            .map_err(|e| format_toml_error(path, text, &e)),
    }
}

pub fn load_layer(path: &Path, kind: ConfigKind) -> Result<Option<Layer>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_layer(path, &text, kind)
}

/// Does this pyproject text contain a `[tool.dir-shuffle]` table? Only the table's presence
/// matters, so this does not validate its contents.
fn pyproject_has_table(text: &str) -> Result<bool, toml::de::Error> {
    let v: toml::Table = toml::from_str(text)?;
    Ok(v.get("tool")
        .and_then(|t| t.as_table())
        .is_some_and(|t| t.contains_key(PYPROJECT_TABLE)))
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFile {
    pub path: PathBuf,
    pub kind: ConfigKind,
}

#[derive(Debug, Clone, Default)]
pub struct Discovery {
    /// Where the global config lives (whether or not it exists).
    pub global: Option<PathBuf>,
    pub project: Option<ProjectFile>,
    /// Project candidates checked, in order, with a note on what was found.
    pub checked: Vec<(PathBuf, &'static str)>,
    pub warnings: Vec<String>,
}

pub fn global_config_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.config_dir().join("dir-shuffle").join("config.toml"))
}

/// Find the project config: the first directory, walking up from `start`, that has a
/// `.dir-shuffle.toml` or a `pyproject.toml` with `[tool.dir-shuffle]`.
pub fn discover_project(start: &Path, d: &mut Discovery) {
    let start = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    for dir in start.ancestors() {
        let dot = dir.join(PROJECT_FILE);
        let py = dir.join(PYPROJECT_FILE);
        let dot_exists = dot.is_file();
        let py_has_table = if py.is_file() {
            match std::fs::read_to_string(&py)
                .map_err(|e| e.to_string())
                .and_then(|t| pyproject_has_table(&t).map_err(|e| e.message().to_string()))
            {
                Ok(has) => {
                    d.checked.push((
                        py.clone(),
                        if has {
                            "has [tool.dir-shuffle]"
                        } else {
                            "no [tool.dir-shuffle]"
                        },
                    ));
                    has
                }
                Err(e) => {
                    d.checked.push((py.clone(), "unreadable"));
                    d.warnings
                        .push(format!("skipping {}: {}", py.display(), e.trim()));
                    false
                }
            }
        } else {
            d.checked.push((py.clone(), "not found"));
            false
        };
        // Keep `.dir-shuffle.toml` listed before `pyproject.toml` (its precedence order).
        let n = d.checked.len();
        d.checked.insert(
            n - 1,
            (dot.clone(), if dot_exists { "found" } else { "not found" }),
        );
        if dot_exists {
            if py_has_table {
                d.warnings.push(format!(
                    "both {} and [tool.dir-shuffle] in {} exist; using {}",
                    dot.display(),
                    py.display(),
                    PROJECT_FILE
                ));
            }
            d.project = Some(ProjectFile {
                path: dot,
                kind: ConfigKind::Plain,
            });
            return;
        }
        if py_has_table {
            d.project = Some(ProjectFile {
                path: py,
                kind: ConfigKind::Pyproject,
            });
            return;
        }
    }
}

pub fn discover(start: &Path) -> Discovery {
    let mut d = Discovery {
        global: global_config_path(),
        ..Default::default()
    };
    discover_project(start, &mut d);
    d
}

/// Load the layers of a discovery in increasing precedence. Errors are fatal: a broken
/// config file should be fixed rather than silently ignored.
pub fn load_layers(d: &Discovery) -> Result<Vec<(Layer, Source)>> {
    let mut layers = Vec::new();
    if let Some(g) = &d.global {
        if g.is_file() {
            if let Some(l) = load_layer(g, ConfigKind::Plain)? {
                layers.push((l, Source::Global(g.clone())));
            }
        }
    }
    if let Some(p) = &d.project {
        if let Some(l) = load_layer(&p.path, p.kind)? {
            layers.push((l, Source::Project(p.path.clone())));
        }
    }
    Ok(layers)
}

/// Discover, load and merge everything for a run on `start`.
pub fn resolve(start: &Path, cli: &CliOverrides) -> Result<(Config, Discovery)> {
    let d = discover(start);
    let layers = load_layers(&d)?;
    Ok((Config::merge(&layers, cli), d))
}

// ---------------------------------------------------------------------------
// `config` subcommands (§7.3)
// ---------------------------------------------------------------------------

pub const TEMPLATE_BODY: &str = r#"# Patterns use gitignore syntax and are applied during the scan.
# ignore = [".DS_Store", "Thumbs.db", "desktop.ini", "._*"]
# extend-ignore = []          # added to `ignore` (from every config layer)
# respect-ignore-files = true # .ignore / .gitignore / .dir-shuffleignore
# prefetch = 5                # neighbours to pre-render along the free axis
# cache-mb = 256              # in-memory preview cache size
#
# [labels]                    # cosmetic dimension labels, by level index
# 0 = "reject"
# 1 = "norm"
# 2 = "temp"
"#;

pub fn template_plain() -> String {
    format!(
        "# dir-shuffle configuration. Every key is optional; uncomment to change a default.\n\
         # See `dir-shuffle config show` for the merged result.\n#\n{TEMPLATE_BODY}"
    )
}

fn toml_str_array(v: &[String]) -> String {
    let items: Vec<String> = v
        .iter()
        .map(|s| toml::Value::String(s.clone()).to_string())
        .collect();
    format!("[{}]", items.join(", "))
}

/// Render a merged config as TOML, annotating each value with its source.
pub fn render_show(c: &Config, header: &[String]) -> String {
    let mut out = String::new();
    for h in header {
        out.push_str(&format!("# {h}\n"));
    }
    if !header.is_empty() {
        out.push('\n');
    }
    let row = |out: &mut String, kv: String, src: &Source| {
        out.push_str(&format!("{kv:<48} # {src}\n"));
    };
    row(
        &mut out,
        format!("ignore = {}", toml_str_array(&c.ignore)),
        &c.ignore_src,
    );
    if c.extend_ignore.is_empty() {
        row(&mut out, "extend-ignore = []".into(), &Source::Default);
    } else {
        out.push_str("extend-ignore = [\n");
        for (p, src) in &c.extend_ignore {
            row(
                &mut out,
                format!("    {},", toml::Value::String(p.clone())),
                src,
            );
        }
        out.push_str("]\n");
    }
    row(
        &mut out,
        format!("respect-ignore-files = {}", c.respect_ignore_files),
        &c.respect_src,
    );
    row(
        &mut out,
        format!("prefetch = {}", c.prefetch),
        &c.prefetch_src,
    );
    row(&mut out, format!("cache-mb = {}", c.cache_mb), &c.cache_src);
    if !c.labels.is_empty() {
        out.push_str("\n[labels]\n");
        for (k, (v, src)) in &c.labels {
            row(
                &mut out,
                format!("{k} = {}", toml::Value::String(v.clone())),
                src,
            );
        }
    }
    if !c.use_ignore_patterns {
        out.push_str("\n# --no-ignore: the patterns above are not applied\n");
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Merged,
    Global,
    Project,
}

pub fn cmd_show(dir: &Path, scope: Scope) -> Result<()> {
    let d = discover(dir);
    for w in &d.warnings {
        eprintln!("warning: {w}");
    }
    let mut header = Vec::new();
    let mut layers = Vec::new();
    let describe = |p: &Option<PathBuf>| match p {
        Some(p) if p.exists() => p.display().to_string(),
        Some(p) => format!("{} (not found)", p.display()),
        None => "(no config directory)".to_string(),
    };
    if scope != Scope::Project {
        header.push(format!("global config:  {}", describe(&d.global)));
        if let Some(g) = &d.global {
            if g.is_file() {
                if let Some(l) = load_layer(g, ConfigKind::Plain)? {
                    layers.push((l, Source::Global(g.clone())));
                }
            }
        }
    }
    if scope != Scope::Global {
        header.push(format!(
            "project config: {}",
            d.project
                .as_ref()
                .map_or("(none found)".to_string(), |p| p.path.display().to_string())
        ));
        if let Some(p) = &d.project {
            if let Some(l) = load_layer(&p.path, p.kind)? {
                layers.push((l, Source::Project(p.path.clone())));
            }
        }
    }
    let c = Config::merge(&layers, &CliOverrides::default());
    print!("{}", render_show(&c, &header));
    Ok(())
}

pub fn cmd_path(dir: &Path) -> Result<()> {
    let d = discover(dir);
    println!("global config:");
    match &d.global {
        Some(g) => println!(
            "  {}  [{}]",
            g.display(),
            if g.is_file() {
                "found, active"
            } else {
                "not found"
            }
        ),
        None => println!("  (could not determine the config directory)"),
    }
    println!("project config (first match walking up; .dir-shuffle.toml beats pyproject.toml):");
    for (p, note) in &d.checked {
        let active = d.project.as_ref().is_some_and(|pf| &pf.path == p);
        println!(
            "  {}  [{}{}]",
            p.display(),
            note,
            if active { ", active" } else { "" }
        );
    }
    for w in &d.warnings {
        eprintln!("warning: {w}");
    }
    Ok(())
}

/// Add an empty `[tool.dir-shuffle]` table (followed by the commented template) to a
/// pyproject document, preserving existing formatting and comments.
pub fn add_pyproject_table(text: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse().context("parsing pyproject.toml")?;
    let tool = doc
        .entry("tool")
        .or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            toml_edit::Item::Table(t)
        })
        .as_table_mut()
        .context("`tool` in pyproject.toml is not a table")?;
    if tool.contains_key(PYPROJECT_TABLE) {
        return Ok(text.to_string());
    }
    let mut t = toml_edit::Table::new();
    t.decor_mut().set_prefix("\n");
    tool.insert(PYPROJECT_TABLE, toml_edit::Item::Table(t));
    let rendered = doc.to_string();
    // Put the commented template right below the new header.
    let header = format!("[tool.{PYPROJECT_TABLE}]");
    let out = match rendered.lines().position(|l| l.trim() == header) {
        Some(i) => {
            let mut lines: Vec<&str> = rendered.lines().collect();
            let body: Vec<&str> = TEMPLATE_BODY.lines().collect();
            lines.splice(i + 1..i + 1, body);
            let mut s = lines.join("\n");
            s.push('\n');
            s
        }
        None => rendered,
    };
    // Sanity check: the table must now exist.
    if !pyproject_has_table(&out).unwrap_or(false) {
        bail!("internal error: failed to add [tool.dir-shuffle] to pyproject.toml");
    }
    Ok(out)
}

fn run_editor(path: &Path) -> Result<()> {
    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("VISUAL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "vi".to_string());
    #[cfg(unix)]
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path)
        .status();
    #[cfg(not(unix))]
    let status = std::process::Command::new(&editor).arg(path).status();
    let status = status.with_context(|| format!("starting editor `{editor}`"))?;
    if !status.success() {
        bail!("editor `{editor}` exited with {status}");
    }
    Ok(())
}

fn ask_yes(question: &str) -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{question} [Y/n] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    let a = line.trim().to_ascii_lowercase();
    a.is_empty() || a == "y" || a == "yes"
}

/// Open `path` in the editor, then validate; on error show it and offer to edit again.
fn edit_and_validate(path: &Path, kind: ConfigKind) -> Result<()> {
    loop {
        run_editor(path)?;
        match load_layer(path, kind) {
            Ok(_) => {
                eprintln!("{}: OK", path.display());
                return Ok(());
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                if !ask_yes("Edit again?") {
                    bail!("{} is not valid", path.display());
                }
            }
        }
    }
}

pub fn cmd_edit(dir: &Path, scope: Scope, pyproject: bool) -> Result<()> {
    match scope {
        Scope::Global | Scope::Merged => {
            let path = global_config_path().context("could not determine the config directory")?;
            if !path.exists() {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                std::fs::write(&path, template_plain())
                    .with_context(|| format!("writing {}", path.display()))?;
                eprintln!("created {}", path.display());
            }
            edit_and_validate(&path, ConfigKind::Plain)
        }
        Scope::Project => {
            let mut d = Discovery::default();
            discover_project(dir, &mut d);
            for w in &d.warnings {
                eprintln!("warning: {w}");
            }
            let existing = d.project.clone();
            let target = match (&existing, pyproject) {
                (Some(p), false) => p.clone(),
                (Some(p), true) if p.kind == ConfigKind::Pyproject => p.clone(),
                (_, true) => {
                    let path = dir.join(PYPROJECT_FILE);
                    let text = if path.exists() {
                        std::fs::read_to_string(&path)
                            .with_context(|| format!("reading {}", path.display()))?
                    } else {
                        String::new()
                    };
                    let new = add_pyproject_table(&text)?;
                    if new != text {
                        std::fs::write(&path, &new)
                            .with_context(|| format!("writing {}", path.display()))?;
                        eprintln!("added [tool.{PYPROJECT_TABLE}] to {}", path.display());
                    }
                    if let Some(p) = &existing {
                        eprintln!(
                            "note: {} takes precedence over this pyproject.toml",
                            p.path.display()
                        );
                    }
                    ProjectFile {
                        path,
                        kind: ConfigKind::Pyproject,
                    }
                }
                (None, false) => {
                    let path = dir.join(PROJECT_FILE);
                    std::fs::write(&path, template_plain())
                        .with_context(|| format!("writing {}", path.display()))?;
                    eprintln!("created {}", path.display());
                    ProjectFile {
                        path,
                        kind: ConfigKind::Plain,
                    }
                }
            };
            edit_and_validate(&target.path, target.kind)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn layer(text: &str) -> Layer {
        parse_layer(Path::new("t.toml"), text, ConfigKind::Plain)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn parse_full_schema() {
        let l = layer(
            r#"
            ignore = ["a"]
            extend-ignore = ["b"]
            respect-ignore-files = false
            prefetch = 3
            cache-mb = 64
            [labels]
            0 = "reject"
            2 = "temp"
            "#,
        );
        assert_eq!(l.ignore.as_deref(), Some(&["a".to_string()][..]));
        assert_eq!(l.prefetch, Some(3));
        assert_eq!(l.cache_mb, Some(64));
        assert_eq!(l.respect_ignore_files, Some(false));
        let labels = l.labels.unwrap();
        assert_eq!(labels[&LevelKey(0)], "reject");
        assert_eq!(labels[&LevelKey(2)], "temp");
    }

    #[test]
    fn errors_have_line_numbers() {
        let text = "prefetch = 3\ncache-mb = \"lots\"\n";
        let e = parse_layer(Path::new("c.toml"), text, ConfigKind::Plain).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.starts_with("c.toml:2:"), "{msg}");

        let e =
            parse_layer(Path::new("c.toml"), "\n\nprefech = 3\n", ConfigKind::Plain).unwrap_err();
        assert!(format!("{e:#}").starts_with("c.toml:3:"), "{e:#}");

        let e = parse_layer(
            Path::new("c.toml"),
            "[labels]\nx = \"a\"\n",
            ConfigKind::Plain,
        )
        .unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("level ind"), "{msg}");
        assert!(msg.starts_with("c.toml:2:"), "{msg}");

        let py = "[project]\nname = \"x\"\n\n[tool.dir-shuffle]\nprefetch = -1\n";
        let e = parse_layer(Path::new("pyproject.toml"), py, ConfigKind::Pyproject).unwrap_err();
        assert!(format!("{e:#}").starts_with("pyproject.toml:5:"), "{e:#}");
    }

    #[test]
    fn precedence_and_extend_ignore() {
        let g = layer("ignore = [\"g1\"]\nextend-ignore = [\"ge\"]\nprefetch = 2\ncache-mb = 10\n[labels]\n0 = \"g0\"\n1 = \"g1\"");
        let p =
            layer("extend-ignore = [\".snakemake_timestamp\"]\nprefetch = 7\n[labels]\n1 = \"p1\"");
        let gs = Source::Global("g".into());
        let ps = Source::Project("p".into());
        let c = Config::merge(
            &[(g, gs.clone()), (p, ps.clone())],
            &CliOverrides::default(),
        );
        assert_eq!(c.ignore, ["g1"]);
        assert_eq!(c.ignore_src, gs);
        assert_eq!(c.prefetch, 7);
        assert_eq!(c.prefetch_src, ps);
        assert_eq!(c.cache_mb, 10);
        assert_eq!(c.ignore_patterns(), ["g1", "ge", ".snakemake_timestamp"]);
        assert_eq!(c.labels[&0].0, "g0");
        assert_eq!(c.labels[&1], ("p1".to_string(), ps.clone()));

        // Project-only extend-ignore keeps the built-in junk list.
        let p = layer("extend-ignore = [\".snakemake_timestamp\"]");
        let c = Config::merge(&[(p, ps.clone())], &CliOverrides::default());
        let mut want: Vec<String> = DEFAULT_IGNORE.iter().map(|s| s.to_string()).collect();
        want.push(".snakemake_timestamp".into());
        assert_eq!(c.ignore_patterns(), want);

        // CLI beats everything.
        let p = layer("prefetch = 7\ncache-mb = 1\nrespect-ignore-files = true");
        let cli = CliOverrides {
            prefetch: Some(1),
            cache_mb: Some(512),
            no_ignore: true,
            no_ignore_files: true,
        };
        let c = Config::merge(&[(p, ps)], &cli);
        assert_eq!((c.prefetch, c.cache_mb), (1, 512));
        assert_eq!(c.prefetch_src, Source::Cli);
        assert!(!c.respect_ignore_files);
        assert!(c.ignore_patterns().is_empty());
    }

    #[test]
    fn project_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let deep = root.join("a/b/c");
        fs::create_dir_all(&deep).unwrap();

        // Nothing at all.
        let mut d = Discovery::default();
        discover_project(&deep, &mut d);
        assert!(d.project.is_none() || !d.project.as_ref().unwrap().path.starts_with(&root));

        // pyproject without our table is not a project config.
        fs::write(root.join("a/pyproject.toml"), "[project]\nname = \"x\"\n").unwrap();
        let mut d = Discovery::default();
        discover_project(&deep, &mut d);
        assert!(d
            .project
            .as_ref()
            .is_none_or(|p| !p.path.starts_with(&root)));

        // A pyproject with the table further up is found ...
        fs::write(
            root.join("pyproject.toml"),
            "[tool.dir-shuffle]\nprefetch = 9\n",
        )
        .unwrap();
        let mut d = Discovery::default();
        discover_project(&deep, &mut d);
        let p = d.project.unwrap();
        assert_eq!(p.path, root.join("pyproject.toml"));
        assert_eq!(p.kind, ConfigKind::Pyproject);
        let l = load_layer(&p.path, p.kind).unwrap().unwrap();
        assert_eq!(l.prefetch, Some(9));

        // ... but the nearest directory wins.
        fs::write(root.join("a/b/.dir-shuffle.toml"), "prefetch = 1\n").unwrap();
        let mut d = Discovery::default();
        discover_project(&deep, &mut d);
        assert_eq!(d.project.unwrap().path, root.join("a/b/.dir-shuffle.toml"));
        assert!(d.warnings.is_empty());

        // Both in the same directory: dotfile wins, with a warning.
        fs::write(root.join("a/b/pyproject.toml"), "[tool.dir-shuffle]\n").unwrap();
        let mut d = Discovery::default();
        discover_project(&deep, &mut d);
        assert_eq!(d.project.unwrap().kind, ConfigKind::Plain);
        assert_eq!(d.warnings.len(), 1);
        // Checked order: c's candidates first, dotfile before pyproject.
        assert_eq!(d.checked[0].0, deep.join(PROJECT_FILE));
        assert_eq!(d.checked[1].0, deep.join(PYPROJECT_FILE));
    }

    #[test]
    fn pyproject_table_insertion_preserves_formatting() {
        let orig =
            "# my project\n[project]\nname = \"x\"  # keep me\n\n[tool.ruff]\nline-length = 100\n";
        let out = add_pyproject_table(orig).unwrap();
        assert!(out.starts_with(orig.trim_end()), "{out}");
        assert!(out.contains("[tool.dir-shuffle]\n# Patterns"), "{out}");
        let l = parse_layer(Path::new("p"), &out, ConfigKind::Pyproject).unwrap();
        assert_eq!(l, Some(Layer::default()));
        // Idempotent.
        assert_eq!(add_pyproject_table(&out).unwrap(), out);
        // Empty / new file.
        let out = add_pyproject_table("").unwrap();
        assert!(parse_layer(Path::new("p"), &out, ConfigKind::Pyproject)
            .unwrap()
            .is_some());
    }

    #[test]
    fn template_is_valid_and_empty() {
        assert_eq!(layer(&template_plain()), Layer::default());
    }
}
