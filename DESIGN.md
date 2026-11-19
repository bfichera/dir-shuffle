# `dir-shuffle` — faceted terminal browser for parameter-sweep output trees

## 1. Problem

Analysis pipelines often write outputs into nested directories, one level per parameter:

```
start/
  mean-rejected/            ← level 0
    gaussian-normalized/    ← level 1
      364K/                 ← level 2
        beta_hist.pdf       ← level 3 (file)
        tau.pdf
        g2_frames/…         ← deeper than the shallowest file level → ignored
```

A normal file manager only lets you compare files in the same physical folder, so you can only vary the last level. The user wants to compare along **any** level. For example: "`beta_hist` for every temperature, with reject=mean and norm=gaussian."

**Model:** treat the tree as a table, with one row per output file and one column per directory level, plus the filename. A **view** is a **slice** of this table. You fix a value for every dimension except one (the *free* dimension), and the matching rows form a "virtual folder" you can scroll through with inline image previews.

Useful vocabulary: Hive-style partitioning, OLAP slice/dice/pivot, faceted browsing, small multiples.

## 2. Goals and non-goals

**Goals**
- `dir-shuffle [DIR]` works with no config.
- The directory structure is the only source of truth. There are no user-supplied path patterns and nothing tool-specific (no Snakemake/Hydra/etc. knowledge).
- **Speed comes first:** instant startup on large trees, and smooth scrolling with previews.
- Inline previews using the kitty graphics protocol (target terminal: Ghostty). Supports PDF (via PDFium, using `pdfium-render`) and common raster formats.
- A single Rust binary that installs with `uv tool install dir-shuffle`.

**Non-goals (for now)**
- Changing files on disk.
- Interpreting folder names in any meaningful way (labels are cosmetic only, §3.4).
- Remote filesystems or live file watching.

## 3. Data model

### 3.1 Scan
1. Walk `start/` with the `ignore` crate. Settings: `hidden(false)`, so hidden files are **included**; `.ignore`, `.gitignore`, and `.dir-shuffleignore` are respected; and the configured ignore patterns (§7) are applied as overrides.
2. **Walk breadth-first, one level at a time.** At each depth, list the entries that aren't ignored. The first depth that contains at least one regular file is **D**. A file directly in `start/` has depth 0. Don't go deeper than D.
3. **Rows** are the non-ignored regular files at depth D. Directories at depth D (e.g. `g2_frames/`) are ignored, along with everything under them.
4. Each row has `D + 1` dimension values: the D directory names, then the file **stem**. The extension is stored on the row but isn't a dimension. If two files differ only by extension, keep both and show the extension in the list.
5. Ignore rules apply **before** D is chosen, so an ignored stray file can't change D.

```rust
struct Table { dims: Vec<Dim>, rows: Vec<Row>, index: HashMap<Box<[u32]>, RowId> }
struct Dim   { label: String, values: Vec<String> }      // values naturally sorted
struct Row   { key: Box<[u32]>, path: PathBuf, ext: String }
```

### 3.2 Startup notice
Show this in the status line: `D=3 · 256 files · e.g. mean-rejected/gaussian-normalized/364K/beta.pdf · ignored N deeper files`. If D turns out shallower than the user expected, the example path shows which stray file caused it, and the user can fix it with an ignore rule.

### 3.3 Sorting
Use natural sort everywhere (`300K < 1000K`, `frame_2 < frame_10`).

### 3.4 Dimension labels (cosmetic only)
If every value at a level shares a suffix after a separator (`-rejected`, `-normalized`) or a unit suffix (`K`), use it as the label. Otherwise use `level N`, and label the last dimension `file`. Labels can be overridden in config (§7).

### 3.5 Constant dimensions
A dimension with one value is shown in the header for context, but it isn't offered as a free axis.

### 3.6 Missing combinations
**TODO:** for now, values with no file in the current slice are left out. Later, show placeholders.

## 4. Views
- A view is `(free: DimId, fixed: Vec<Option<ValueId>>)`.
- Building a slice: for each value `v` of the free dimension, look up `fixed` with `v` swapped in, using `Table.index`. That's O(values), so nothing is precomputed.
- Changing one fixed value keeps all the other choices.
- **Pivot:** making a fixed dimension free fixes the old free dimension at its currently selected value, and keeps the selection on the same file.
- **TODO:** two free dimensions shown as a grid (small multiples).

## 5. UI (ratatui + crossterm)

```
┌ dir-shuffle: debug/fit ───────────────────────────────────────────────┐
│ free: [K]   rejected=mean  normalized=gaussian  file=beta_hist        │
├──────────────────┬────────────────────────────────────────────────────┤
│   300K           │                                                    │
│ > 364K           │              (kitty image preview)                 │
│   423K           │                                                    │
│   …              │                                                    │
├──────────────────┴────────────────────────────────────────────────────┤
│ debug/fit/mean-rejected/gaussian-normalized/364K/beta_hist.pdf        │
└───────────────────────────────────────────────────────────────────────┘
```

**Screens:** the view picker (on startup, or with `v`) lists the free-axis choices with how many values each has. The **browser** is shown above.

| key | action |
|---|---|
| `j`/`k`, arrows | move along the free dimension |
| `g`/`G` | first / last |
| `Tab`/`Shift-Tab` | move focus between the fixed dimensions in the header |
| `h`/`l` | change the value of the focused fixed dimension |
| `f` | pivot: make the focused dimension the free one |
| `v` | view picker |
| `o` | open in the system viewer (`open` / `xdg-open`) |
| `y` | copy the path to the clipboard (OSC 52) |
| `q` | quit |

## 6. Preview pipeline

### 6.1 Rendering
- `trait Renderer { fn render(&self, path: &Path, target_px: (u32, u32)) -> Result<RgbaImage>; }`, chosen by file extension.
- **PDF:** use the `pdfium-render` crate, which binds at runtime to a **prebuilt** PDFium shared library from [`bblanchon/pdfium-binaries`](https://github.com/bblanchon/pdfium-binaries). No PDF engine is compiled from source. Render **page 1 only**, at a scale picked to fit `target_px` exactly (`PdfRenderConfig::scale_page_to_display_size`). Don't render big and then downscale.
  - **Version pinning:** depend on `pdfium-render` with `default-features = false, features = ["pdfium_8125", "image_latest", "thread_safe"]`. The `pdfium_<N>` feature must match the PDFium build you bundle (`chromium/8125` today). Change both together, along with `PDFIUM_BUILD` in `PiContainerfile`.
  - **Library lookup, first hit wins:** (1) `$DIR_SHUFFLE_PDFIUM_DIR`; (2) the copy bundled with the wheel (§9), found relative to the canonicalized `std::env::current_exe()`, because `uv tool` symlinks the binary; (3) the system library search path.
  - **If PDFium can't be loaded,** don't crash: PDFs get the text card, and the status line says once that PDF previews are unavailable and why.
  - **Threading:** with `thread_safe`, pdfium-render serializes PDFium calls behind a global lock, so PDF renders effectively run one at a time even with N workers. That's acceptable for page-1 previews. Bind the library **once** and share the `Pdfium` instance across workers. Measure this in milestone 4.
- **Raster:** use `image` plus `fast_image_resize`.
- **Anything else:** show a text card with the path, size, and modified time.

### 6.2 Display
- Use `ratatui-image`. It detects the protocol (kitty first, with sixel/halfblock fallbacks) and the cell size in pixels.
- Target size = the preview area × the cell size in pixels. When the terminal is resized, throw away renders made for the old size.
- Clear or replace kitty images when the selection changes so nothing stale stays on screen. **TODO:** tmux passthrough.

### 6.3 Cache and prefetch
- The cache key is `(path, mtime, len, target_px)`.
- A memory LRU of protocol-ready images, with a size limit in bytes (default 256 MB).
- A pool of N worker threads (N = cores − 1, so one core stays free) takes jobs from a **priority queue**, highest priority first:
  1. the current selection,
  2. neighbors along the free axis, ±1…±K, nearest first (K = 5 by default),
  3. ±1…±2 neighbors along the focused fixed dimension.
- When the selection changes, reorder the queue. Jobs already running finish and go into the cache. The UI thread **never blocks**: it shows "rendering…" and redraws when a worker sends a result back over a `crossbeam-channel`.
- **TODO:** a disk thumbnail cache in `$XDG_CACHE_HOME/dir-shuffle/`.

## 7. Configuration and ignore rules

### 7.1 Sources and precedence (highest wins)
1. CLI flags
2. **Project config**: the first one found when walking up from `start/`:
   - `.dir-shuffle.toml`, or
   - `pyproject.toml` with a `[tool.dir-shuffle]` table (only used if that table exists)
   If both are in the same directory, `.dir-shuffle.toml` wins, and the tool prints a warning.
3. **Global config**: `$XDG_CONFIG_HOME/dir-shuffle/config.toml`, or `~/Library/Application Support/dir-shuffle/config.toml` on macOS (use the `directories` crate).
4. Built-in defaults

Ignore files (`.ignore`, `.gitignore`, `.dir-shuffleignore`) are always respected on top of this, unless turned off in config.

### 7.2 Schema
The same keys are used in every config file. In `pyproject.toml` they go under `[tool.dir-shuffle]`.

```toml
# Patterns use gitignore syntax and are applied during the scan.
ignore = [".DS_Store", "Thumbs.db", "desktop.ini", "._*"]   # built-in default
extend-ignore = []          # added to `ignore` (lets a project add without replacing)
respect-ignore-files = true # .ignore / .gitignore / .dir-shuffleignore
prefetch = 5
cache-mb = 256

[labels]                    # optional cosmetic overrides, by level index
0 = "reject"
1 = "norm"
2 = "temp"
```

How config files are merged: a scalar or `ignore` set in a higher-precedence file replaces the lower one. `extend-ignore` lists from all layers are **added together**. So, for example, a project can add `.snakemake_timestamp` without repeating the OS junk list.

### 7.3 Config subcommands
```
dir-shuffle config show [--global|--project]   # print the merged config, noting where each value comes from
dir-shuffle config edit --global               # open in $EDITOR, creating it with commented defaults
dir-shuffle config edit --project              # .dir-shuffle.toml in DIR (or the existing project config)
dir-shuffle config path                        # list the paths that were checked
```
- `edit --project` edits whichever project config already exists. If none exists, it creates `.dir-shuffle.toml`. If you pass `--pyproject`, it adds a `[tool.dir-shuffle]` table to `pyproject.toml` instead, using `toml_edit` so the existing formatting and comments are kept.
- After the editor closes, the file is validated, and any error shows the line where it occurred.

## 8. CLI
```
dir-shuffle [DIR]                 # default "."
  --prefetch K
  --cache-mb N
  --no-ignore                     # don't use the ignore patterns from config (ignore files still apply)
  --no-ignore-files               # don't respect .ignore/.gitignore/.dir-shuffleignore
dir-shuffle config {show,edit,path} ...
dir-shuffle --version
```

## 9. Packaging (`uv tool install dir-shuffle`)
Use **maturin with `bindings = "bin"`**, the same way `ruff` and `uv` are shipped. The Python wheel contains the compiled binary plus the matching prebuilt PDFium shared library (§6.1).

```toml
# pyproject.toml (of this repo)
[build-system]
requires = ["maturin>=1.5,<2"]
build-backend = "maturin"

[project]
name = "dir-shuffle"
requires-python = ">=3.8"
license = "MIT"
dynamic = ["version"]

[tool.maturin]
bindings = "bin"
strip = true
data = "packaging/data"        # CI puts libpdfium here, e.g. packaging/data/share/dir-shuffle/
```
```toml
# Cargo.toml
[package]
name = "dir-shuffle"            # binary: dir-shuffle, crate ident: dir_shuffle
license = "MIT"

[profile.release]
lto = "fat"
codegen-units = 1
opt-level = 3
```
- **CI:** `PyO3/maturin-action` builds wheels for macOS arm64 and x86_64 and Linux x86_64 and aarch64 (manylinux; no musl wheels, because a static musl binary cannot `dlopen` PDFium), and publishes to PyPI when a tag is pushed. Before each build, CI downloads the matching `pdfium-<platform>.tgz` for that target (`mac-arm64`, `mac-x64`, `linux-x64`, `linux-arm64`) and puts `libpdfium.{dylib,so}` in the wheel through maturin's `data` directory, so each wheel is self-contained. No C/C++ toolchain is needed. **To verify in milestone 6:** where the `data` files end up after installing with `uv tool` (the lookup in §6.1 must match), that the manylinux glibc baseline of PDFium's Linux builds works, and macOS code-signing/quarantine for the bundled dylib.
- An sdist is also published, so `uv tool install git+…` builds from source when Rust is installed. A source build doesn't bundle PDFium, so it uses `$DIR_SHUFFLE_PDFIUM_DIR` or a system libpdfium, and otherwise falls back to text cards for PDFs.
- **License:** the project is released under the MIT license (`LICENSE` at the repo root). The dependencies are permissive: PDFium is BSD-3-Clause/Apache-2.0, and pdfium-render is MIT/Apache-2.0. The wheel redistributes the PDFium binary, so ship PDFium's `LICENSE` file (it comes in the `pdfium-*.tgz`) inside the wheel.
- **Acceptance test:** on a clean machine without Rust, `uv tool install dir-shuffle && dir-shuffle --version` works.

### 9.1 Dev container
The dev container (`PiContainerfile`) contains PDFium build `chromium/8125` in `/opt/pdfium/lib`, with `DIR_SHUFFLE_PDFIUM_DIR` and `LD_LIBRARY_PATH` pointing there, so `cargo run` and `cargo test` can render PDFs with no extra setup. Nothing PDF-related is compiled from source. Tests that need PDFium should skip, not fail, if the library can't be loaded.

## 10. Crates
`ratatui`, `crossterm`, `ratatui-image`, `ignore`, `pdfium-render`, `image`, `fast_image_resize`, `natord`, `lru`, `crossbeam-channel`, `clap` (derive), `serde`, `toml`, `toml_edit`, `directories`, `anyhow`.

## 11. Layout
```
src/
  main.rs        CLI dispatch, terminal setup/teardown, event loop
  config.rs      discovery, layered merge, config subcommands
  scan.rs        BFS walk, depth-D detection
  table.rs       Table/Dim/Row, index, natural sort, label inference
  view.rs        slicing, value stepping, pivot
  render/{mod,pdf,raster}.rs
  cache.rs       LRU, priority queue, workers
  ui.rs          header, list, preview, status, view picker
```

## 12. Milestones
1. `config` + `scan` + `table`, with unit tests on synthetic temp-dir trees: uneven depth, a stray `.DS_Store` with and without ignore rules, constant dimensions, natural sort, config precedence and `extend-ignore` merging, and pyproject detection.
2. A text-only TUI: view picker, browser, pivot.
3. Raster previews.
4. PDFium previews, cache, and prefetch. Benchmark the time to first preview and the frame time while holding `j`.
5. `config` subcommands, `o`/`y`, resize handling, label inference.
6. maturin packaging and the CI wheel matrix.

## 13. TODO / future
- Show missing combinations as placeholders (§3.6).
- Grid view with two free dimensions.
- Disk thumbnail cache.
- Filtering or multi-selecting values within a dimension ("dice").
- tmux passthrough.
- Live updates while a pipeline is writing outputs.
