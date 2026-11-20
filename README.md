# dir-shuffle

Browse the output folders of a parameter sweep in your terminal, with previews of PDFs, images and text files.

Each folder level is a parameter. Pick one parameter to scroll through while the others stay fixed. For example, you can see `beta_hist.pdf` for every temperature, with everything else held the same.

## Install

```sh
uv tool install dir-shuffle
```

Previews look best in Ghostty or kitty. Other terminals show lower-quality previews.

## Use

```sh
dir-shuffle path/to/outputs
```

| Key | What it does |
|---|---|
| `j` / `k` | next / previous file |
| `Tab` / `Shift-Tab` | select another parameter in the header |
| `J` / `K` | scroll down / up in a text preview |
| `h` / `l` | change the selected parameter's value |
| `f` | scroll through the selected parameter instead |
| `v` | choose which parameter to scroll through |
| `o` | open the file in its usual app |
| `y` | copy the file's path |
| `q` | quit |

dir-shuffle only looks at files in the shallowest folder level that has files. Deeper files are skipped. If a stray file makes it pick the wrong level, the bottom line shows that file's path. Add its name to `ignore` (see below) or to a `.gitignore`.

## Settings

You don't need any settings. To change them, run:

```sh
dir-shuffle config edit --global
```

This opens a settings file that lists every option with a short explanation. For example, to use `H` / `L` to select parameters:

```toml
[keys]
focus-next = "L"
focus-prev = "H"
```

To use settings for one project only, put them in a `.dir-shuffle.toml` file in the project folder. `dir-shuffle config show` shows your current settings.

## License

MIT. The package includes PDFium, which has its own license.
