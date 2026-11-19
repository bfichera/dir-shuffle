# dir-shuffle

A faceted terminal browser for parameter-sweep output trees.

```
start/mean-rejected/gaussian-normalized/364K/beta_hist.pdf
      └─ level 0 ──┘ └──── level 1 ────┘ └ 2 ┘ └─ file ─┘
```

`dir-shuffle` treats the tree as a table, with one row per file and one column per
directory level plus the file stem. You pick one *free* dimension and scroll along it,
with every other dimension held fixed. Previews are shown inline (kitty graphics
protocol, e.g. Ghostty and kitty, with sixel/half-block fallbacks), for PDFs (page 1,
via PDFium) and common raster formats. Example: "`beta_hist` for every temperature,
with reject=mean and norm=gaussian."

## Install

```sh
uv tool install dir-shuffle
```

Wheels bundle the binary and a prebuilt PDFium. A source build (`uv tool install git+…`,
needs Rust) has no bundled PDFium. It then uses `$DIR_SHUFFLE_PDFIUM_DIR` or a system
`libpdfium`, and otherwise shows text cards for PDFs.

## Use

```sh
dir-shuffle [DIR]            # default "."
```

The status line explains the scan: `D=3 · 256 files · e.g. … · ignored N deeper files`.
**D** is the shallowest directory level that contains a file. Files below it are ignored.
If D is shallower than you expected, the example path shows the stray file that caused
it. Add an ignore rule for it (see below).

| key | action |
|---|---|
| `j`/`k`, arrows, PgUp/PgDn | move along the free dimension |
| `g`/`G` | first / last |
| `Tab`/`Shift-Tab` | focus a fixed dimension in the header |
| `h`/`l` | change the focused dimension's value |
| `f` | pivot: make the focused dimension free |
| `v` | choose the free axis |
| `o` / `Enter` | open in the system viewer |
| `y` | copy the absolute path (OSC 52) |
| `q` | quit |

All of these can be rebound (see `[keys]` below). `Ctrl-c` always quits.

## Configure

No configuration is needed. Optional settings are read from (highest precedence first):
CLI flags; the first `.dir-shuffle.toml` or `pyproject.toml` with a `[tool.dir-shuffle]`
table found walking up from `DIR`; the global `config.toml` (see `dir-shuffle config path`).

```toml
ignore = [".DS_Store", "Thumbs.db", "desktop.ini", "._*"]   # gitignore syntax
extend-ignore = [".snakemake_timestamp"]  # added to `ignore`, merged across files
respect-ignore-files = true  # .ignore / .gitignore / .dir-shuffleignore
prefetch = 5                 # neighbours pre-rendered along the free axis
cache-mb = 256               # preview cache

[labels]                     # cosmetic dimension names, by level index
2 = "temp"

[keys]                       # a key or a list of keys; replaces that action's defaults
focus-next = "L"             # default "tab"
focus-prev = ["H", "shift-tab"]
open = []                    # unbind
```

Key actions: `move-down`, `move-up`, `page-down`, `page-up`, `half-page-down`,
`half-page-up`, `first`, `last`, `focus-next`, `focus-prev`, `value-next`, `value-prev`,
`pivot`, `view-picker`, `open`, `copy-path`, `quit`. Keys are single characters (`j`, `L`,
`?`) or names (`tab`, `enter`, `esc`, `space`, `up`, `pagedown`, `f1`, …), with optional
`ctrl-`/`alt-`/`shift-` prefixes. A key you bind is taken away from any action that had it
by default. `dir-shuffle config show` lists the effective bindings and where each came from.

```sh
dir-shuffle config show [--global|--project]
dir-shuffle config edit --global | --project [--pyproject]
dir-shuffle config path
```

Flags: `--prefetch K`, `--cache-mb N`, `--no-ignore` (skip config patterns),
`--no-ignore-files` (skip ignore files).

## Develop

```sh
cargo test        # PDF tests skip if PDFium can't be loaded
cargo run -- path/to/sweep
packaging/fetch-pdfium.sh linux-x64 && maturin build --release   # self-contained wheel
```

The PDFium build is pinned in four places that must agree: the `pdfium_<N>` feature in
`Cargo.toml`, `PDFIUM_BUILD` in `PiContainerfile` and `packaging/fetch-pdfium.sh`, and
the download URL in `.github/workflows/release.yml`.

## Releasing

Releases are built and published by `.github/workflows/release.yml` when a `v*` tag is
pushed. That builds wheels for macOS (arm64, x64) and Linux (x64, aarch64), each
bundling PDFium, plus an sdist, and uploads them to PyPI.

One-time setup:
1. On PyPI, go to *Your account → Publishing → Add a new pending publisher* (GitHub):
   project `dir-shuffle`, owner `bfichera`, repository `dir-shuffle`, workflow
   `release.yml`, environment `pypi`. No API token is needed (trusted publishing).
2. On GitHub, go to *Settings → Environments → New environment* and name it `pypi`.
   Optionally, add yourself as a required reviewer, so every publish waits for your OK.

Each release:
```sh
./prepare-release.sh 0.2.0 --dry-run   # optional: run every check, then put everything back
./prepare-release.sh 0.2.0             # bump, check, commit, tag, push (asks before committing and before pushing)
```
The version can also be `v0.2.0`, a pre-release like `0.2.0-rc.1`, or `patch`, `minor`
or `major` to bump the current version. The script:
1. Checks that you're on an up-to-date `main` with a clean tree, that the tag is new, and
   that the PDFium pins agree.
2. Sets the version in `Cargo.toml` and `Cargo.lock`.
3. Runs the CI checks (`cargo fmt --check`, `clippy -D warnings`, `cargo test`).
4. Commits "Release X.Y.Z" and creates an annotated tag `vX.Y.Z` listing the changes
   since the last tag.
5. Pushes `main` and the tag together.

If anything fails before the commit, it puts `Cargo.toml` and `Cargo.lock` back. Options:
`--yes` (no questions), `--no-push` (stop after tagging) and `--help`.
For the first release, you can also do a CI dry run: *Actions → release → Run workflow*
builds every wheel and publishes nothing.

Then check that `uv tool install --force dir-shuffle` gets the new version and
`dir-shuffle <dir> --summary` reports `PDFium loaded (bundled: …)`. PyPI versions are
immutable. If a release is broken, bump the version and release again (yanking on PyPI is
possible, but the version can't be reused).

License: MIT. Wheels redistribute PDFium (BSD-3-Clause/Apache-2.0); its license files
ship in `share/dir-shuffle/`.
