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
```

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

The PDFium build is pinned in three places that must agree: the `pdfium_<N>` feature in
`Cargo.toml`, `PDFIUM_BUILD` in `PiContainerfile`, and `packaging/fetch-pdfium.sh`.

License: MIT. Wheels redistribute PDFium (BSD-3-Clause/Apache-2.0); its license files
ship in `share/dir-shuffle/`.
