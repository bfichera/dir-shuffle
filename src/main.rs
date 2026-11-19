//! dir-shuffle: a faceted terminal browser for parameter-sweep output trees.

mod cache;
mod config;
mod render;
mod scan;
mod table;
mod ui;
mod view;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use crossterm::event::{self, Event};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use ratatui_image::picker::Picker;

use crate::cache::{Cache, Encoder, Preview};
use crate::config::{CliOverrides, Scope};
use crate::render::kitty::{base64, KittyState};
use crate::scan::ScanOptions;
use crate::table::Table;
use crate::ui::{Action, App};

#[derive(Parser, Debug)]
#[command(
    name = "dir-shuffle",
    version,
    about = "Faceted terminal browser for parameter-sweep output trees",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    browse: BrowseArgs,
}

#[derive(Args, Debug, Default)]
struct BrowseArgs {
    /// Directory to browse
    #[arg(default_value = ".")]
    dir: PathBuf,
    /// Neighbours to pre-render along the free axis
    #[arg(long, value_name = "K")]
    prefetch: Option<usize>,
    /// Preview cache size in MB
    #[arg(long, value_name = "N")]
    cache_mb: Option<u64>,
    /// Don't use the ignore patterns from config (ignore files still apply)
    #[arg(long)]
    no_ignore: bool,
    /// Don't respect .ignore/.gitignore/.dir-shuffleignore
    #[arg(long)]
    no_ignore_files: bool,
    /// Scan, print the summary and exit (no TUI)
    #[arg(long, hide = true)]
    summary: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Show, edit or locate configuration
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Print the merged config, noting where each value comes from
    Show {
        #[arg(long, conflicts_with = "project")]
        global: bool,
        #[arg(long)]
        project: bool,
        /// Directory whose project config to use
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Open a config file in $EDITOR (created with commented defaults if missing)
    Edit {
        #[arg(long, conflicts_with = "project", required_unless_present = "project")]
        global: bool,
        #[arg(long)]
        project: bool,
        /// With --project: use [tool.dir-shuffle] in pyproject.toml
        #[arg(long, requires = "project")]
        pyproject: bool,
        /// Directory for the project config
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// List the config paths that are checked
    Path {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Config { action }) => run_config(action),
        None => run_browse(cli.browse),
    };
    if let Err(e) = result {
        eprintln!("dir-shuffle: {e:#}");
        std::process::exit(1);
    }
}

fn run_config(action: ConfigCmd) -> Result<()> {
    match action {
        ConfigCmd::Show {
            global,
            project,
            dir,
        } => {
            let scope = if global {
                Scope::Global
            } else if project {
                Scope::Project
            } else {
                Scope::Merged
            };
            config::cmd_show(&dir, scope)
        }
        ConfigCmd::Edit {
            global,
            project,
            pyproject,
            dir,
        } => {
            let scope = if project && !global {
                Scope::Project
            } else {
                Scope::Global
            };
            config::cmd_edit(&dir, scope, pyproject)
        }
        ConfigCmd::Path { dir } => config::cmd_path(&dir),
    }
}

struct Loaded {
    table: Table,
    notice: String,
    root_display: String,
    scan_opts: ScanOptions,
    skipped_dirs: Vec<PathBuf>,
}

fn load(args: &BrowseArgs) -> Result<(Loaded, config::Config)> {
    let cli = CliOverrides {
        prefetch: args.prefetch,
        cache_mb: args.cache_mb,
        no_ignore: args.no_ignore,
        no_ignore_files: args.no_ignore_files,
    };
    let (cfg, disc) = config::resolve(&args.dir, &cli)?;
    for w in &disc.warnings {
        eprintln!("warning: {w}");
    }
    let scan_opts = ScanOptions {
        patterns: cfg.ignore_patterns(),
        respect_ignore_files: cfg.respect_ignore_files,
    };
    let res = scan::scan(&args.dir, &scan_opts)?;
    for e in res.errors.iter().take(5) {
        eprintln!("warning: {e}");
    }
    let Some(depth) = res.depth else {
        anyhow::bail!(
            "no files found under {}{}",
            args.dir.display(),
            if res.ignored > 0 {
                format!(" ({} entries ignored)", res.ignored)
            } else {
                String::new()
            }
        );
    };
    let table = Table::build(res.files, &cfg.label_overrides());
    let example = table.rel_display(0);
    let mut notice = format!(
        "D={depth} · {} file{} · e.g. {example}",
        table.rows.len(),
        if table.rows.len() == 1 { "" } else { "s" }
    );
    if res.ignored > 0 {
        notice.push_str(&format!(" · {} ignored", res.ignored));
    }
    let root_display = args.dir.display().to_string();
    Ok((
        Loaded {
            table,
            notice,
            root_display,
            scan_opts,
            skipped_dirs: res.skipped_dirs,
        },
        cfg,
    ))
}

fn deeper_suffix(n: usize) -> String {
    format!(
        " · ignored {n} deeper file{}",
        if n == 1 { "" } else { "s" }
    )
}

fn run_browse(args: BrowseArgs) -> Result<()> {
    let (loaded, cfg) = load(&args)?;
    if args.summary {
        return print_summary(&args, &loaded);
    }
    run_tui(args, loaded, cfg)
}

fn print_summary(args: &BrowseArgs, loaded: &Loaded) -> Result<()> {
    let mut out = std::io::stdout().lock();
    let res = (|| -> std::io::Result<()> {
        let mut s = loaded.notice.clone();
        if !loaded.skipped_dirs.is_empty() {
            let root = args.dir.clone();
            let n = scan::count_deeper(&root, &loaded.scan_opts, &loaded.skipped_dirs);
            if n > 0 {
                s.push_str(&deeper_suffix(n));
            }
        }
        writeln!(out, "{s}")?;
        for (d, dim) in loaded.table.dims.iter().enumerate() {
            writeln!(
                out,
                "  [{d}] {:<16} {:>5} values: {}",
                dim.label,
                dim.values.len(),
                dim.values
                    .iter()
                    .take(6)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;
        }
        writeln!(out, "{}", render::pdf::status())
    })();
    match res {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        r => Ok(r?),
    }
}

fn run_tui(args: BrowseArgs, loaded: Loaded, cfg: config::Config) -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        anyhow::bail!("dir-shuffle needs an interactive terminal (try --help)");
    }

    // Count deeper files in the background for the status line.
    let (deeper_tx, deeper_rx) = crossbeam_channel::bounded::<usize>(1);
    if !loaded.skipped_dirs.is_empty() {
        let root = args.dir.clone();
        let opts = loaded.scan_opts.clone();
        let dirs = loaded.skipped_dirs.clone();
        std::thread::Builder::new()
            .name("count-deeper".into())
            .spawn(move || {
                let _ = deeper_tx.send(scan::count_deeper(&root, &opts, &dirs));
            })?;
    }

    let mut app = App::new(
        loaded.table,
        loaded.root_display,
        loaded.notice,
        cfg.prefetch,
    );

    // Terminal setup. Restore on panic, too.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        hook(info);
    }));
    terminal::enable_raw_mode().context("enabling raw mode")?;
    execute!(
        std::io::stdout(),
        terminal::EnterAlternateScreen,
        crossterm::cursor::Hide
    )?;
    // Detect graphics protocol and cell size (must happen before reading events).
    // Kitty transmission compression is probed too: encoding runs on worker threads, so
    // deflating costs the UI nothing and cuts the bytes the terminal must parse.
    let opts = ratatui_image::picker::cap_parser::QueryStdioOptions {
        kitty_compression: true,
        ..Default::default()
    };
    let picker =
        Picker::from_query_stdio_with_options(opts).unwrap_or_else(|_| Picker::halfblocks());
    let result = (|| -> Result<()> {
        let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
        term.clear()?;
        let enc = Encoder::new(picker);
        let mut cache = Cache::new(cfg.cache_bytes(), enc, Cache::default_workers());
        event_loop(&mut term, &mut app, &mut cache, &deeper_rx)
    })();
    restore_terminal();
    result
}

fn restore_terminal() {
    let mut out = std::io::stdout();
    let _ = execute!(out, crossterm::cursor::Show, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
}

/// Current cell size in pixels, preferring a live query (it changes with font size).
fn cell_px(fallback: (u16, u16)) -> (u16, u16) {
    match terminal::window_size() {
        Ok(ws) if ws.width > 0 && ws.height > 0 && ws.columns > 0 && ws.rows > 0 => {
            (ws.width / ws.columns, ws.height / ws.rows)
        }
        _ => fallback,
    }
}

fn event_loop(
    term: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    cache: &mut Cache,
    deeper_rx: &crossbeam_channel::Receiver<usize>,
) -> Result<()> {
    let font_fallback = cache.enc.cell_px;
    let mut kitty = KittyState::default();
    let mut dirty = true;
    let mut last_wanted: Vec<PathBuf> = Vec::new();
    let mut size = term.size()?;
    let mut cells = cell_px(font_fallback);
    let tick = Duration::from_millis(250);

    loop {
        // Geometry: the preview area depends on terminal size and list width.
        let inner = app.preview_inner(ratatui::layout::Rect::new(0, 0, size.width, size.height));
        if cache.set_geometry((inner.width, inner.height), cells) {
            // Old renders and queued jobs were discarded: queue everything again.
            last_wanted.clear();
            dirty = true;
        }

        // Prefetch: re-prioritise whenever the wanted list changes.
        let wanted = app.wanted();
        if wanted != last_wanted {
            cache.request(&wanted);
            last_wanted = wanted;
        }

        if dirty {
            let preview = if app.screen == ui::Screen::Browser {
                let p = app.current_preview(cache);
                if p.is_none() && app.selected_path().is_some() {
                    // Not cached (e.g. the file changed on disk since it was rendered):
                    // make sure it is queued. Running and cached files are skipped.
                    cache.request(&last_wanted);
                }
                p
            } else {
                None
            };
            let k = match &preview {
                Some(Preview::Kitty(k)) => Some(k.clone()),
                _ => None,
            };
            kitty.before_draw(k.as_deref(), term.backend_mut())?;
            term.draw(|f| app.draw(f, preview.as_ref()))?;
            kitty.after_draw(term.backend_mut())?;
            dirty = false;
        }

        // Wait for input or a finished render, whichever comes first.
        let deadline = Instant::now() + tick;
        let mut got_result = false;
        while let Ok(r) = cache.results().try_recv() {
            got_result |= accept(app, cache, r);
        }
        if got_result {
            dirty = true;
            continue;
        }
        if let Ok(n) = deeper_rx.try_recv() {
            if n > 0 {
                app.notice.push_str(&deeper_suffix(n));
                dirty = true;
            }
        }
        // Poll input in short slices so render results are picked up promptly.
        loop {
            if event::poll(Duration::from_millis(8))? {
                match event::read()? {
                    Event::Key(k) => {
                        match app.handle_key(k) {
                            Action::Quit => {
                                kitty.clear(term.backend_mut())?;
                                return Ok(());
                            }
                            Action::None => {}
                            Action::Open(p) => open_external(app, &p),
                            Action::Copy(s) => {
                                let seq = format!("\x1b]52;c;{}\x07", base64(s.as_bytes()));
                                term.backend_mut().write_all(seq.as_bytes())?;
                                term.backend_mut().flush()?;
                                app.set_message(format!("copied {s}"));
                            }
                        }
                        dirty = true;
                        // Drain queued key repeats before redrawing, so holding `j` skips
                        // frames instead of lagging behind.
                        if event::poll(Duration::ZERO)? {
                            continue;
                        }
                        break;
                    }
                    Event::Resize(w, h) => {
                        size = ratatui::layout::Size::new(w, h);
                        cells = cell_px(font_fallback);
                        term.autoresize()?;
                        dirty = true;
                        break;
                    }
                    Event::FocusGained => {
                        cells = cell_px(font_fallback);
                        dirty = true;
                        break;
                    }
                    _ => {}
                }
            }
            if !cache.results().is_empty() || Instant::now() >= deadline {
                break;
            }
        }
        if app.tick() {
            dirty = true;
        }
    }
}

fn accept(app: &mut App, cache: &mut Cache, r: cache::JobResult) -> bool {
    if let Some(reason) = &r.unavailable {
        app.pdf_unavailable(reason);
    }
    cache.accept(r)
}

fn open_external(app: &mut App, path: &Path) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(target_os = "macos"))]
    let cmd = "xdg-open";
    let res = std::process::Command::new(cmd)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match res {
        Ok(mut child) => {
            // Reap in the background so no zombie is left behind.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            app.set_message(format!("opened {}", path.display()));
        }
        Err(e) => app.set_message(format!("{cmd} failed: {e}")),
    }
}
