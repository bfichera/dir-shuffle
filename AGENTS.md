# AGENTS.md: working on dir-shuffle

`dir-shuffle` is a Rust TUI, a faceted browser for parameter-sweep output trees, with
inline image previews. Read this file first, then `DESIGN.md` (the spec, with section
numbers like §3.1 that the code cites) and `README.md` (the user docs). This file covers
what isn't obvious from the code: invariants, traps, how to test, and where things live.

## 1. What it does (30-second version)

- A directory tree is treated as a **table**. Each file at the shallowest file level **D**
  is a row. The columns are the D directory names plus the file **stem**. The extension
  is stored on the row but isn't a dimension.
- A **view** fixes a value for every dimension except one *free* dimension. The list
  scrolls along the free dimension, and the preview pane shows the selected file.
- Previews:
  - PDFs (page 1, via a prebuilt PDFium loaded at runtime) and raster images (`image`
    crate) are rendered.
  - Previews are drawn with the kitty graphics protocol (Ghostty is the main target).
    Without kitty they fall back to sixel, iTerm2 or half-blocks through `ratatui-image`.
  - Anything else gets a text card (path, size, mtime).
- It's distributed as a Python wheel holding the binary plus `libpdfium` (maturin
  `bindings = "bin"`), so `uv tool install dir-shuffle` works.

Product principles from DESIGN.md §2, which you must not break:
1. **No config is required** (`dir-shuffle [DIR]` just works).
2. **The directory structure is the only source of truth.** The tool has no knowledge of
   Snakemake, Hydra or any other pipeline tool, and no user path patterns. Folder names
   are never interpreted; labels are cosmetic only (§3.4).
3. **Speed comes first.** Startup is instant on big trees, and the UI thread never blocks
   on rendering.
4. Non-goals: modifying files, remote filesystems, live file watching.

## 2. Commands

```sh
cargo build                         # dev build
cargo build --release               # needed for realistic perf and for tools/tui_drive.py
cargo test                          # ~48 unit tests, ~0.1 s; PDF tests skip if PDFium is missing
cargo clippy --all-targets -- -D warnings   # CI runs this exact command
cargo fmt --check                   # CI runs this too
cargo run --release -- /path/to/tree
dir-shuffle /path/to/tree --summary # hidden flag: print scan result + PDFium status, no TUI
```

**Before you finish any change**, run all of these and keep them clean: `cargo fmt`,
`cargo clippy --all-targets -- -D warnings`, `cargo test`. If you touched the UI, rendering
or the event loop, also run an end-to-end check with `tools/tui_drive.py` (§6).

Packaging:
```sh
packaging/fetch-pdfium.sh linux-arm64    # or linux-x64, mac-arm64, mac-x64
maturin build --release -o dist          # wheel with binary + bundled PDFium
uv tool install --force .                # local install (builds via maturin)
```

### Environment notes (dev container, `PiContainerfile`)
- The container is aarch64 Linux. `CARGO_TARGET_DIR=/opt/cargo-target`, so binaries
  live at `/opt/cargo-target/{debug,release}/dir-shuffle`, **not** `./target`.
- PDFium `chromium/8125` is in `/opt/pdfium/lib`. `DIR_SHUFFLE_PDFIUM_DIR` and
  `LD_LIBRARY_PATH` point there, so PDF tests really render.
- Dependency sources for reading APIs are at
  `/opt/cargo/registry/src/index.crates.io-*/`. Prefer reading the real source over
  guessing: several crates here changed their APIs recently.
- RAM can be tight. If a release build gets OOM-killed, use `cargo build -j4`.
- The user runs the app on **macOS in Ghostty** (host), not in this container. The repo
  is shared via a bind mount. `packaging/data/data/share/dir-shuffle/` may contain the
  user's macOS `libpdfium.dylib`, which is git-ignored. **Don't delete it**, and never
  stage a Linux `.so` there without removing it afterwards: it would end up in the user's
  Mac wheel.
- `pyte` isn't installed system-wide. Use `uv run --with pyte …`.

## 3. Source map

```
src/
  main.rs        CLI (clap), config/scan/table loading, terminal setup/teardown,
                 event loop, `o` (xdg-open/open) and `y` (OSC 52) side effects
  config.rs      config discovery, layered merge with sources, parse errors with
                 line:col, `config show|path|edit`, template text
  keys.rs        key-binding actions, key spec parser, defaults, KeyMap (layers)
  scan.rs        breadth-first scan to depth D with ignore rules; count_deeper()
  table.rs       Table/Dim/Row, index, natural sort, label inference
  view.rs        View: slice, move, focus, step values, pivot, prefetch neighbours
  cache.rs       byte-bounded LRU of protocol-ready previews, priority job queue,
                 worker threads, Encoder (own kitty vs ratatui-image)
  ui.rs          App state, key dispatch (via KeyMap), layout, drawing (header,
                 list, preview, status, view picker), help line
  render/
    mod.rs       Renderer trait, Kind by extension, fit(), text cards, size/time fmt
    pdf.rs       PDFium binding (once, OnceLock), lookup order, page-1 render
    raster.rs    decode with `image`, resize with fast_image_resize, flatten on white
    kitty.rs     own kitty-graphics encoder (unicode placeholders) + KittyState
    kitty_diacritics.rs   row/column diacritics table (generated, include!'d)
tools/
  tui_drive.py       end-to-end driver: runs the TUI in a pty, fakes Ghostty's replies,
                     dumps the screen, decodes kitty images to PNG
  make_demo_tree.py  synthetic sweep trees (small demo, or 20k-file benchmark)
packaging/
  fetch-pdfium.sh    downloads bblanchon/pdfium-binaries and stages it for the wheel
  data/data/share/dir-shuffle/.keep   maturin `data` dir; must exist (see §5.8)
prepare-release.sh   bump version, run CI checks, commit, tag, push (see §7)
.github/workflows/release.yml   test job + wheel matrix + sdist + PyPI publish on tags
```

### Data flow
```
main::load
  └─ config::resolve(dir, cli) ─→ Config (merged layers + sources)
  └─ scan::scan(dir, ScanOptions) ─→ ScanResult { depth D, files: Vec<FileRecord>, skipped_dirs, ignored }
  └─ Table::build(files, label_overrides)
main::run_tui
  └─ App::new(table, ...) ; app.keymap = cfg.keymap()
  └─ Picker::from_query_stdio_with_options(kitty_compression) ─→ Encoder ─→ Cache::new(workers = cores-1)
  └─ event_loop:
       cache.set_geometry(preview area, cell px)   (changes → new generation, cache cleared)
       cache.request(app.wanted())                 (priority order: selection, ±1..±K free axis, ±1..±2 focused dim)
       draw: app.current_preview(cache) → KittyState.before_draw → terminal.draw → KittyState.after_draw
       wait: worker results over crossbeam (cache.accept) | key events (app.handle_key) | resize | 250 ms tick
  └─ background thread: scan::count_deeper → appends "ignored N deeper files" to the status line
```

## 4. Key invariants and design decisions (read before changing things)

### Scan (`scan.rs`, §3.1)
- **Breadth-first, one level at a time.** Never read deeper than D. That's why startup is
  instant (20k files in 5k dirs: about 27 ms). `count_deeper` reads below D, but only in
  a background thread after the UI is up.
- **Ignore rules apply before D is chosen.** An ignored stray file must not change D.
- Hidden files are **included** (`hidden(false)` semantics). VCS dirs (`.git`, `.hg`,
  `.svn`, `.jj`) are always skipped.
- `.ignore`, `.gitignore` and `.dir-shuffleignore` are honoured, including those in
  parent directories of the root and outside git repos (`require_git(false)`). Matching
  is done with `WalkBuilder::build_matchers()` → `ignore::IncrementalIgnore` (public API
  since ignore 0.4.34; `Cargo.lock` pins 0.4.34), checked one entry at a time inside our
  own level-by-level walk. We don't use `ignore`'s walker itself, because it's
  depth-first and can't stop at D.
- Config `ignore`/`extend-ignore` patterns use gitignore syntax via `GitignoreBuilder`
  and act as **overrides**: a match ignores the entry, and `!pat` force-keeps it. Don't
  use `ignore::overrides` here: its semantics are inverted (whitelist globs).
- Files that are never rows: the ignore files themselves (when respected) and
  `.dir-shuffle.toml` (always). Otherwise a config or `.gitignore` next to the outputs
  collapses D to that level. This was a real bug.
- Symlinks are followed, with cycle detection on canonical paths. `MAX_DEPTH = 128`.
  Parallel listing kicks in at 32 or more dirs per level.

### Table and view (`table.rs`, `view.rs`, §3–4)
- `Table.index: HashMap<Box<[u32]>, RowId>` maps a full key to its first row. Rows are
  sorted by (key, ext), so files differing only by extension are adjacent
  (`rows_for(key)` returns the range). The list shows the extension only when the
  current slice has duplicates.
- **Natural sort is our own `natural_cmp`** (handles decimals: `0.25 < 0.5`, and
  `300K < 1000K`, `frame_2 < frame_10`). It's a total order. The `natord` crate from §10
  was deliberately dropped because it gets decimals wrong.
- Label inference: a common suffix after a separator (`-rejected` → `rejected`), a unit
  suffix (`K`), or a hive `k=v` prefix. Otherwise `level N`, and the last dimension is
  `file`. `Dim.short` holds the values with the label part stripped, for the header.
  When the user sets a label override, `short` is the full value list.
- Constant dimensions (one value) are shown in the header but are never free or
  focusable.
- Slices are computed on the fly (O(values) index lookups) and never precomputed.
  Missing combinations are **omitted** (§3.6 TODO: placeholders).
- Changing a fixed value keeps the other choices. `step_focused` skips values whose
  slice is empty, and keeps the selection on the same free value (and ext), otherwise the
  nearest one.
- Pivot (`f`): the focused dimension becomes free, the old free dimension is fixed at its
  current value, the selection stays on the same file, and focus moves to the old free
  dimension.

### Rendering (`render/`, §6.1)
- `Renderer::render(path, target_px) -> RgbaImage` must render **at** the target size.
  Don't render big and then downscale.
- **PDF:** use `scale_page_to_display_size(w, h)` **plus
  `.rotate_if_landscape(PdfPageRenderRotation::None, false)`**. Without the second call,
  pdfium-render rotates landscape pages 90°, which turns plots sideways.
- PDFium is bound **once** (`OnceLock`), because pdfium-render's bindings can only be
  initialized once per process. The lookup order (first hit wins):
  1. `$DIR_SHUFFLE_PDFIUM_DIR` (a dir or the lib file)
  2. Bundled, relative to the **canonicalized** `current_exe()`: `<prefix>/share/dir-shuffle/`,
     `<prefix>/lib`, then the bin dir. `uv tool` symlinks the binary, which is why it
     must be canonicalized.
  3. The system library.

  If it fails, return the `Unavailable` error: PDFs get a text card, and the status line
  reports the reason once. Never crash.
- A global `RENDER_LOCK` serializes whole PDF renders. pdfium-render's `thread_safe`
  feature only locks individual FFI calls, and interleaving calls across documents is
  unsafe. Raster renders run in parallel.
- **The PDFium version is pinned in four places that must agree:**
  - the `pdfium_8125` feature in `Cargo.toml`
  - `PDFIUM_BUILD` in `PiContainerfile`
  - `PDFIUM_BUILD` in `packaging/fetch-pdfium.sh`
  - the download URL in `.github/workflows/release.yml`
- Raster: `image::ImageReader::with_guessed_format`, resized with fast_image_resize
  (Bilinear when shrinking, CatmullRom when enlarging, `use_alpha(true)`), then flattened
  on white. AVIF isn't supported, because `image` would need native dav1d.

### Display: kitty (`render/kitty.rs`, `cache.rs::Encoder`, §6.2)
- **We use our own kitty encoder**, not ratatui-image's, when the protocol is kitty and
  we're not in tmux. The reason is lifecycle control: ratatui-image never deletes
  transmitted images, so terminal memory grows while scrolling. ratatui-image is still
  used for detection (`Picker`), cell size, and the sixel/iTerm2/half-block fallbacks
  (`Preview::Proto`).
- How it works:
  - The image is transmitted once (`a=T,U=1,q=2`, 4096-char base64 chunks), as
    `f=32,o=z` (zlib RGBA) when the terminal supports compression, otherwise `f=100` (PNG).
  - It's padded with transparent pixels to whole cells.
  - It's drawn with U+10EEEE placeholder cells: the image id is in the 24-bit foreground
    colour, with row/column diacritics. Each placeholder cell has
    `CellDiffOption::ForcedWidth(1)`.
- `KittyState` keeps **exactly one image alive**. `before_draw` transmits the new image
  *before* the frame that shows its placeholders. `after_draw` deletes the old one
  (`a=d,d=I`) only *after* its cells were overwritten, so there's no blank flash. Keep
  this order.
- The compression probe is opt-in in ratatui-image (`QueryStdioOptions{kitty_compression:
  true}`), so `main.rs` enables it. Deflating happens on worker threads, so it costs the
  UI nothing.
- The terminal query happens after raw mode and the alternate screen are entered, and
  **before** the event loop starts reading stdin.

### Cache and prefetch (`cache.rs`, §6.3)
- Entries are keyed `(path, target_px)` and store a `Stamp{mtime, len}`. `get(path,
  verify=true)` re-stats the file. The UI verifies once per selection change, which
  means the effective key is `(path, mtime, len, target_px)` as the spec says.
- A byte-budgeted LRU (`cache-mb`, default 256). It always keeps at least the newest entry.
- Workers: `cores − 1`, minimum 1. `request(paths)` **replaces** the pending queue
  (priority = list order), and skips cached and running jobs. Running jobs finish and
  land in the cache.
- **Generations:** `set_geometry` bumps `gen`, clears the LRU and pending queue, and
  returns `true`. The caller must then clear `last_wanted` and request again. `accept()`
  drops results from old generations.
- A race that was fixed and must stay fixed: a worker removes the job from `running`
  **before** sending the result. Otherwise a `request` in between skips the file as
  "running" and it's never queued again (the symptom was "rendering…" forever).
- The event loop also re-requests when the selected preview is missing at draw time
  (e.g. the file changed on disk since it was rendered).
- Unsupported extensions get their text card immediately at request time, with no
  worker involved.

### Event loop and UI (`main.rs`, `ui.rs`, §5)
- Drawing is pure: `App::draw` reads state, and all mutation happens in `App` methods.
  `draw` records `preview_area`.
- Key-repeat handling: after a key, queued events are drained before redrawing, so
  holding `j` skips frames instead of lagging. In the benchmark (47 presses at 30 Hz),
  47 images were transmitted and 47 deleted, with no backlog.
- Startup screen: the view picker if there's more than one free candidate, otherwise the
  browser. The initial free dimension is the deepest non-constant directory level.
- The status block is 2 lines: the selected path, then the notice or message. Messages
  expire after 4 s.
- `y` sends OSC 52 with the canonicalized absolute path. `o` runs `xdg-open` or `open`
  in the background, and the child is reaped in a thread.
- The panic hook restores the terminal. Keep it if you restructure setup.

### Key bindings (`keys.rs`)
- `Action` enum with kebab-case names. Each action has `defaults()`. `KeyMap::build(layers)`:
  - Setting an action in a layer **replaces** its keys.
  - A bound key is **taken away** from other actions (higher layers win).
  - `[]` unbinds the action.
- `Key` is normalized so that config specs and terminal events compare equal. Letters
  carry their case instead of SHIFT (`"L"` == Shift+l), and `shift-tab` == `BackTab`.
  Specs look like `j`, `L`, `?`, `tab`, `enter`, `space`, `pagedown`, `f5`, with optional
  `ctrl-`, `alt-` or `shift-` prefixes (also `c-`, `a-`/`m-`, `s-`).
- `ctrl-c` is reserved: it always quits and can't be bound.
- Duplicate keys within one `[keys]` table are an error (`check_duplicates`), reported
  with the file and the line of the `[keys]` header.
- The view picker uses the keymap for movement, quit and view-picker. It also has fixed
  keys: Enter/Space choose, Esc goes back, and `1`–`9` pick directly.
- The help line at the bottom is **generated from the keymap** (`App::help_line`), and
  unbound actions are omitted.
- **To add a new action**, update all of these together:
  - the `Action` variant
  - `Action::ALL` (and its array length)
  - `name()`
  - `defaults()`
  - its handling in `ui.rs::browser_key` (and `picker_key` if relevant)
  - `help_line` if it's user-facing
  - the `[keys]` block in `config.rs::TEMPLATE_BODY`, which a test checks matches the
    defaults exactly
  - the README key table and action list
  - the DESIGN.md §5 table if it's a core key

### Config (`config.rs`, §7)
- Precedence, highest wins:
  1. CLI flags
  2. Project config: the first one found walking up from DIR, either `.dir-shuffle.toml`
     or a `pyproject.toml` that has a `[tool.dir-shuffle]` table. If both are in the same
     dir, the dotfile wins and a warning is printed.
  3. Global config: `directories::BaseDirs::config_dir()/dir-shuffle/config.toml`.
  4. Defaults.
- How layers merge:
  - Scalars and `ignore` replace lower layers.
  - `extend-ignore` lists from all layers are **concatenated**.
  - `[labels]` merge per level index.
  - `[keys]` layers are kept as a list and combined by `KeyMap::build`.
- `Layer` has `deny_unknown_fields`, so typos are errors. A broken config is **fatal**,
  with a `path:line:col` message plus the source line, and isn't silently skipped. An
  unreadable `pyproject.toml` during discovery is only a warning.
- `config show` prints every value with its source, including the effective keymap.
  `config edit` creates the file from the template if missing, opens `$EDITOR`, then
  validates and offers to edit again. `--pyproject` inserts the table using `toml_edit`,
  so formatting and comments are kept.
- `Config` keeps a `Source` for each value. If you add a setting, update all of these:
  - the `Layer` field
  - the `Config` field and its source
  - the `merge` logic
  - `render_show`
  - `TEMPLATE_BODY`
  - the CLI flag if there is one (`CliOverrides`, `main.rs::BrowseArgs`)
  - the README

## 5. Traps (each of these has bitten before)

1. **ratatui-image default features pull in `chafa`**, a C library that needs pkg-config.
   It's disabled via `default-features = false, features = ["crossterm", "image-defaults"]`.
2. **`StatefulProtocol` resizes at render time** and would block the UI. Only use
   `picker.new_protocol(...)` on worker threads, plus the `Image` widget.
3. **PDF auto-rotation:** see §4 Rendering.
4. **pdfium bindings can only be initialized once.** Don't call `bind_to_library` again
   after a failure. The `OnceLock` caches the error message.
5. **Regex traps when parsing terminal output in Python:**
   - In `rb"..."` raw strings, `\x1b\\` matches ESC followed by one backslash. Writing
     `\\\\` there means *two* backslashes and silently matches nothing.
   - Kitty delete commands have no `;payload`, so use the tight
     `rb"\x1b_G([^;\x1b]*);?([^\x1b]*)\x1b\\"`.
6. **A pty test must answer the terminal queries**, or the app hangs and exits. That
   means ratatui-image's query (kitty `_Gi=31`, DA1, `[16t`, terminated by `[5n` → reply
   `\x1b[0n`) and ratatui's cursor position request `\x1b[6n` → `\x1b[1;1R`.
   `tools/tui_drive.py` does this.
7. **Read a pty long enough.** Redraws after async renders arrive later. Don't conclude
   "no preview" from a short read.
8. **maturin needs the `data` dir to exist**, including for sdist builds. That's why
   `packaging/data/data/share/dir-shuffle/.keep` is committed, and why `pyproject.toml`
   has an sdist `include` for it. Only directories named `data`, `scripts`, `headers`,
   `purelib` or `platlib` may sit directly under `packaging/data/`.
9. **`uv tool install .` caches builds** keyed on `pyproject.toml` only by default.
   `[tool.uv] cache-keys` in `pyproject.toml` lists `src/**/*.rs`, `Cargo.*` and the
   data dir. Keep it updated if you add build inputs (e.g. `build.rs`).
10. **Python file rewrites drop the exec bit.** After editing `packaging/fetch-pdfium.sh`
    or `tools/*.py` with a script, run `chmod +x` again.
11. **Writing to stdout in `--summary` mode** has to handle a broken pipe (`| head`).
    It's handled; keep it that way.
12. **crossterm key events:** uppercase letters arrive as `Char('L')` + SHIFT. Shift-Tab
    arrives as `BackTab` (sometimes as `Tab` + SHIFT). Ctrl+Shift+letter may arrive
    lowercase + SHIFT. `Key::from_event` normalizes all of these. Ignore
    `KeyEventKind::Release`.
13. **The cargo target dir isn't `./target`** in the container (see §2).

## 6. Testing

### Unit tests (`cargo test`)
Every module has tests. Key ones:
- **scan:**
  - uneven depth, and stray `.DS_Store` with and without ignore rules
  - ignore files, including parent dirs; ignored dirs aren't descended
  - symlink cycles, parallel listing, the `.dir-shuffle.toml` exclusion
  - `count_deeper`
- **table:** natural sort examples, label inference and overrides, index building.
- **view:** slicing, stepping (keeps choices and skips empty slices), pivot, neighbours,
  extension duplicates.
- **config:**
  - schema, line-numbered errors, precedence and `extend-ignore` merging
  - pyproject detection and table insertion preserving formatting
  - the template matching the defaults, and `[keys]` layering and errors
- **keys:** spec parsing ⇄ event matching round trips, defaults without conflicts, layer
  replace/steal, duplicate detection.
- **cache:**
  - render → hit → staleness on rewrite, geometry change discards renders
  - the byte budget evicts by LRU, broken files get cards
  - these use `Picker::halfblocks()`, so no terminal is needed
- **render:**
  - PDF at the target size and not rotated, parallel PDF renders (skipped without PDFium)
  - raster fit, kitty encoding/chunking/compression/padding
  - the `KittyState` transmit/delete order, base64 against RFC 4648
- **ui:** rendered with ratatui's `TestBackend`: picker → browser screens, key flows,
  prefetch order, custom keymap (H/L focus).

Tests needing PDFium must **skip, not fail**, when it can't be loaded (§9.1). Follow the
pattern in `render/pdf.rs` tests.

### End-to-end (real binary in a pty)
```sh
cargo build --release
python3 tools/make_demo_tree.py /tmp/demo
BIN=/opt/cargo-target/release/dir-shuffle uv run --with pyte tools/tui_drive.py \
    --png-dir /tmp/shots /tmp/demo/start '\r' j l
```
This prints the final screen as text (placeholder cells show as `#`), counts kitty
transmits and deletes, and decodes every transmitted image to PNG, which you can open
and look at. Use `--plain` to simulate a terminal without kitty (half-blocks). Things to
check after UI or render changes:
- Transmits and deletes balance, so only one image is alive. The delete comes after the
  next transmit.
- The PNGs show the right file at the right size (landscape stays landscape).
- The header, list, status line and help line read correctly.
- Resize: send SIGWINCH after `TIOCSWINSZ`. A new image should arrive at the new size.

Benchmark (milestone 4): `python3 tools/make_demo_tree.py /tmp/big --big`, then:
- Time `--summary` (≈ 30 ms for 20k files).
- Time from the first Enter to the first `\x1b_Ga=T` (≈ 1 ms when cached or prefetched,
  10–20 ms for a cold PDF).
- Hold `j` at 30 Hz and confirm there's no backlog.

### What can't be tested here
Real Ghostty rendering, OSC 52 clipboard permission, `open` on macOS, and macOS
dylib quarantine and signing. Ask the user to verify those on their Mac.

## 7. Packaging and release (§9)

- `pyproject.toml`: maturin, `bindings = "bin"`, `strip = true`,
  `data = "packaging/data"`. Wheel `.data/data/share/dir-shuffle/libpdfium.*` installs to
  `<prefix>/share/dir-shuffle/`, which is exactly what `pdf.rs::bundled_dirs()` looks
  for. Verified with `uv tool install` and no env vars set.
- `packaging/fetch-pdfium.sh <platform>` stages the library, `PDFIUM-LICENSE` and
  `pdfium-licenses/`. The staged files are git-ignored except `.keep`.
- CI (`.github/workflows/release.yml`), triggered by PRs, manual dispatch and `v*` tags:
  - A test job (fmt, clippy `-D warnings`, tests with PDFium). On tags, it also fails if
    the tag isn't exactly `v` + the `Cargo.toml` version.
  - A wheel matrix: macOS arm64 (`macos-15`), macOS x64 (`macos-15-intel`), and Linux
    gnu x64/aarch64 (manylinux, cross-built on `ubuntu-latest`). Each fetches its
    matching PDFium. PDFium's Linux builds need only glibc 2.17, so the wheel's
    manylinux tag is decided by our own binary.
  - An sdist, and a PyPI trusted-publishing job (environment `pypi`) on tags.
  - **It has never run on GitHub yet.** The wheel and sdist builds were checked locally.
    Runner labels and action versions were checked as of Nov 2026: `macos-13` and
    `macos-14` are retired, and Node 20 actions stopped working, hence checkout@v7,
    upload@v7 and download@v8.
- **No musl wheels, on purpose.** Rust links musl statically, and a static musl binary
  can't `dlopen` (verified: "Dynamic loading not supported"), so PDFs would never
  preview. `.cargo/config.toml` sets `-crt-static` for musl, so a build from the sdist on
  Alpine links dynamically. That build is untested; it needs a musl-native toolchain.
- **The sdist must not contain a libpdfium.** `include` names only the `.keep` file, not a
  glob, because the user's staged Mac dylib would otherwise ship (7 MB).
- Open question: macOS code-signing and quarantine of the bundled dylib. bblanchon's
  dylib is ad-hoc signed (it has `LC_CODE_SIGNATURE`). Files installed by pip/uv don't
  get the quarantine flag, so it should load. Confirm this on a Mac after the first
  release.
- The version is in `Cargo.toml` (`version`). `pyproject.toml` has
  `dynamic = ["version"]`. **Releases are made with `./prepare-release.sh <version>`**.
  It bumps the version, runs the CI checks, commits, tags and pushes atomically; see the
  README section "Releasing". The script must keep working with macOS `/bin/bash` 3.2
  and BSD tools:
  - no `mapfile`, `declare -A` or `${x,,}`
  - no `sed -i` (it differs between GNU and BSD)
  - keep `[[ =~ ]]` regexes in a variable (3.2 can't parse a literal with parentheses)

  After editing it, run `shellcheck prepare-release.sh`, then test it in a scratch clone
  with a bare repo as `origin`, as done originally.

## 8. Conventions

- Rust 2021, MSRV 1.88 (`rust-version`). `anyhow` for errors, with `.context(...)`
  messages that name the path.
- Comments explain **why**, and cite DESIGN.md sections (`§6.3`) where relevant. Module
  docs (`//!`) describe each module's contract. Keep them accurate when you change
  behaviour.
- Don't add dependencies casually. Speed and a single self-contained binary matter. No C
  libraries besides the runtime-loaded PDFium.
- Never block the UI thread on I/O or rendering. Anything slow goes to workers or a
  background thread and comes back via a channel.
- User-visible changes go in `README.md`. Changes to the spec or its decisions go in
  `DESIGN.md`. Agent-facing knowledge goes in this file.
- Commit with a descriptive message after a coherent, fully green change.

## 9. Roadmap (DESIGN.md §13, not implemented)

- Placeholders for missing combinations (§3.6). Today they're omitted from the slice.
- A grid view with two free dimensions (small multiples).
- A disk thumbnail cache in `$XDG_CACHE_HOME/dir-shuffle/`.
- Filtering or multi-selecting values within a dimension ("dice").
- tmux passthrough. In tmux, kitty currently goes through ratatui-image (`own_kitty =
  false`), and our lifecycle management doesn't apply.
- Live updates while a pipeline writes outputs (file watching is a non-goal for now).

Small ideas noted but not done: a `?` help overlay listing all bindings, and a
search/jump-to-value within the free dimension.
