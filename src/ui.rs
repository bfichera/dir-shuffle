//! Terminal UI: view picker, browser (header, list, preview, status) and key handling
//! (DESIGN.md §5).
//!
//! Drawing is pure: `draw` only reads the app state and the cache. Everything else happens
//! in `App` methods called from the event loop in `main.rs`, including moving the
//! selection, sending prefetch requests and emitting kitty images.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::cache::{Cache, Lookup, Preview};
use crate::table::{DimId, Table};
use crate::view::View;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Picker,
    Browser,
}

pub enum Action {
    None,
    Quit,
    /// Open the selected file in the system viewer.
    Open(PathBuf),
    /// Copy text to the clipboard (OSC 52).
    Copy(String),
}

pub struct App {
    pub table: Table,
    pub root_display: String,
    pub view: View,
    pub screen: Screen,
    /// Highlighted row in the view picker (index into `table.free_candidates()`).
    pub picker_sel: usize,
    /// Status line. `notice` is the startup summary; `message` overrides it briefly.
    pub notice: String,
    message: Option<(String, Instant)>,
    pdf_warned: bool,
    pub prefetch: usize,
    /// Preview area of the last frame (inner rect), for geometry and kitty placement.
    pub preview_area: Rect,
    list_state: ListState,
    /// Re-stat the selected file on the next lookup (set on selection changes).
    verify_selected: bool,
}

const FOCUSED_NEIGHBORS: usize = 2;
const MESSAGE_TTL: Duration = Duration::from_secs(4);

impl App {
    pub fn new(table: Table, root_display: String, notice: String, prefetch: usize) -> App {
        // Start on the first non-constant dimension, preferring the deepest directory level
        // (usually the sweep's innermost parameter). If every directory level is constant,
        // use the file dimension.
        let cands = table.free_candidates();
        let ndirs = table.dims.len().saturating_sub(1);
        let free = cands
            .iter()
            .rev()
            .find(|&&d| d < ndirs)
            .or(cands.last())
            .copied()
            .unwrap_or(table.dims.len().saturating_sub(1));
        let view = View::new(&table, free, 0);
        let picker_sel = cands.iter().position(|&d| d == free).unwrap_or(0);
        // The picker is only worth showing when there is a real choice.
        let screen = if cands.len() > 1 {
            Screen::Picker
        } else {
            Screen::Browser
        };
        App {
            table,
            root_display,
            view,
            screen,
            picker_sel,
            notice,
            message: None,
            pdf_warned: false,
            prefetch,
            preview_area: Rect::default(),
            list_state: ListState::default(),
            verify_selected: true,
        }
    }

    pub fn set_message(&mut self, m: impl Into<String>) {
        self.message = Some((m.into(), Instant::now()));
    }

    /// Report a missing PDF backend once (§6.1).
    pub fn pdf_unavailable(&mut self, reason: &str) {
        if !self.pdf_warned {
            self.pdf_warned = true;
            self.notice = format!("PDF previews unavailable: {reason}");
        }
    }

    /// True if a timed message expired since the last draw (so a redraw is needed).
    pub fn tick(&mut self) -> bool {
        if let Some((_, t)) = &self.message {
            if t.elapsed() > MESSAGE_TTL {
                self.message = None;
                return true;
            }
        }
        false
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.view
            .selected_row()
            .map(|r| self.table.row(r).path.clone())
    }

    /// Files to render, highest priority first (§6.3): the selection, the free-axis
    /// neighbours ±1…±K, then ±1…±2 along the focused fixed dimension.
    pub fn wanted(&self) -> Vec<PathBuf> {
        if self.screen != Screen::Browser {
            return self.selected_path().into_iter().collect();
        }
        let mut rows = Vec::new();
        rows.extend(self.view.selected_row());
        rows.extend(self.view.free_neighbors(self.prefetch));
        rows.extend(self.view.focused_neighbors(&self.table, FOCUSED_NEIGHBORS));
        rows.into_iter()
            .map(|r| self.table.row(r).path.clone())
            .collect()
    }

    /// Look up the current preview. Re-stats the file once after each selection change.
    pub fn current_preview(&mut self, cache: &mut Cache) -> Option<Preview> {
        let path = self.selected_path()?;
        let verify = std::mem::take(&mut self.verify_selected);
        match cache.get(&path, verify) {
            Lookup::Ready(p) => Some(p),
            Lookup::Pending => None,
        }
    }

    pub fn handle_key(&mut self, k: KeyEvent) -> Action {
        if k.kind == KeyEventKind::Release {
            return Action::None;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('q'))
        {
            return Action::Quit;
        }
        match self.screen {
            Screen::Picker => self.picker_key(k),
            Screen::Browser => self.browser_key(k),
        }
    }

    fn picker_key(&mut self, k: KeyEvent) -> Action {
        let n = self.table.free_candidates().len();
        match k.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Esc | KeyCode::Char('v') => self.screen = Screen::Browser,
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => {
                if n > 0 {
                    self.picker_sel = (self.picker_sel + 1) % n;
                }
            }
            KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => {
                if n > 0 {
                    self.picker_sel = (self.picker_sel + n - 1) % n;
                }
            }
            KeyCode::Char('g') | KeyCode::Home => self.picker_sel = 0,
            KeyCode::Char('G') | KeyCode::End => self.picker_sel = n.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right | KeyCode::Char(' ') => {
                if let Some(&d) = self.table.free_candidates().get(self.picker_sel) {
                    self.choose_free(d);
                }
                self.screen = Screen::Browser;
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                let i = (c as u8 - b'0') as usize;
                if (1..=n).contains(&i) {
                    let d = self.table.free_candidates()[i - 1];
                    self.choose_free(d);
                    self.screen = Screen::Browser;
                }
            }
            _ => {}
        }
        Action::None
    }

    fn choose_free(&mut self, d: DimId) {
        self.view.set_free(&self.table, d);
        self.selection_changed();
    }

    fn selection_changed(&mut self) {
        self.verify_selected = true;
    }

    fn browser_key(&mut self, k: KeyEvent) -> Action {
        let page = (self.preview_area.height.max(2) as isize) - 1;
        match k.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('j') | KeyCode::Down => self.view.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.view.move_by(-1),
            KeyCode::PageDown => self.view.move_by(page),
            KeyCode::PageUp => self.view.move_by(-page),
            KeyCode::Char('d') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.view.move_by(page / 2)
            }
            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.view.move_by(-page / 2)
            }
            KeyCode::Char('g') | KeyCode::Home => self.view.first(),
            KeyCode::Char('G') | KeyCode::End => self.view.last(),
            KeyCode::Tab => self.view.focus_step(&self.table, 1),
            KeyCode::BackTab => self.view.focus_step(&self.table, -1),
            KeyCode::Char('h') | KeyCode::Left => {
                if !self.view.step_focused(&self.table, -1) {
                    self.no_step_message(-1);
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if !self.view.step_focused(&self.table, 1) {
                    self.no_step_message(1);
                }
            }
            KeyCode::Char('f') => {
                if !self.view.pivot(&self.table) {
                    self.set_message(
                        "nothing to pivot: no other dimension has more than one value",
                    );
                }
            }
            KeyCode::Char('v') => {
                let cands = self.table.free_candidates();
                self.picker_sel = cands.iter().position(|&d| d == self.view.free).unwrap_or(0);
                self.screen = Screen::Picker;
            }
            KeyCode::Char('o') | KeyCode::Enter => {
                if let Some(p) = self.selected_path() {
                    return Action::Open(p);
                }
            }
            KeyCode::Char('y') => {
                if let Some(p) = self.selected_path() {
                    let abs = std::fs::canonicalize(&p).unwrap_or(p);
                    return Action::Copy(abs.display().to_string());
                }
            }
            _ => return Action::None,
        }
        self.selection_changed();
        Action::None
    }

    fn no_step_message(&mut self, dir: isize) {
        match self.view.focus {
            None => self.set_message("no fixed dimension to change (Tab focuses one)"),
            Some(d) => {
                let label = &self.table.dims[d].label;
                let which = if dir < 0 { "first" } else { "last" };
                self.set_message(format!(
                    "{label}: already at the {which} value with files here"
                ));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------

    /// Split the screen. Returns (header, list, preview, status) for the browser.
    pub fn layout(&self, area: Rect) -> (Rect, Rect, Rect, Rect) {
        let [title_and_header, body, status] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(4),
        ])
        .areas(area);
        let list_w = self.list_width().min(body.width / 2).max(8.min(body.width));
        let [list, preview] =
            Layout::horizontal([Constraint::Length(list_w), Constraint::Min(1)]).areas(body);
        (title_and_header, list, preview, status)
    }

    /// Inner preview rect (inside the border) for a given terminal area.
    pub fn preview_inner(&self, area: Rect) -> Rect {
        let (_, _, preview, _) = self.layout(area);
        preview_block().inner(preview)
    }

    fn list_width(&self) -> u16 {
        let dim = &self.table.dims[self.view.free];
        let show_ext = self.view.has_ext_duplicates();
        let w = self
            .view
            .entries
            .iter()
            .map(|e| {
                let mut n = dim.values[e.value as usize].chars().count();
                if show_ext {
                    n += self.table.row(e.row).ext.chars().count() + 1;
                }
                n
            })
            .max()
            .unwrap_or(4);
        (w + 6).clamp(14, 48) as u16
    }

    pub fn draw(&mut self, f: &mut Frame, preview: Option<&Preview>) {
        let area = f.area();
        let (header, list, prev, status) = self.layout(area);
        self.draw_header(f, header);
        match self.screen {
            Screen::Browser => {
                self.draw_list(f, list);
                self.draw_preview(f, prev, preview);
            }
            Screen::Picker => {
                let body = Rect {
                    x: list.x,
                    y: list.y,
                    width: list.width + prev.width,
                    height: list.height,
                };
                self.draw_picker(f, body);
            }
        }
        self.draw_status(f, status);
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let t = &self.table;
        let mut spans = Vec::new();
        let free_label = &t.dims[self.view.free].label;
        spans.push(Span::styled("free: ", Style::new().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format!("[{free_label}]"),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
        for (d, dim) in t.dims.iter().enumerate() {
            if d == self.view.free {
                continue;
            }
            spans.push(Span::raw("   "));
            let v = &dim.short[self.view.fixed[d] as usize];
            let text = format!("{}={}", dim.label, v);
            let style = if dim.is_constant() {
                Style::new().fg(Color::DarkGray)
            } else if self.view.focus == Some(d) {
                Style::new()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(Color::Cyan)
            };
            if self.view.focus == Some(d) && self.screen == Screen::Browser {
                // Show where h/l can go: position among the values.
                let n = dim.values.len();
                let i = self.view.fixed[d] as usize + 1;
                spans.push(Span::styled(format!("‹{text}›"), style));
                spans.push(Span::styled(
                    format!(" {i}/{n}"),
                    Style::new().fg(Color::DarkGray),
                ));
            } else {
                spans.push(Span::styled(text, style));
            }
        }
        let title = Line::from(vec![
            Span::styled(" dir-shuffle", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!(": {} ", self.root_display)),
        ]);
        let block = Block::new().borders(Borders::ALL).title(title);
        f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        let dim = &self.table.dims[self.view.free];
        let show_ext = self.view.has_ext_duplicates();
        let items: Vec<ListItem> = self
            .view
            .entries
            .iter()
            .map(|e| {
                let mut s = dim.values[e.value as usize].clone();
                let ext = &self.table.row(e.row).ext;
                if show_ext && !ext.is_empty() {
                    s.push('.');
                    s.push_str(ext);
                }
                ListItem::new(s)
            })
            .collect();
        let count = format!(" {}/{} ", self.view.selected + 1, self.view.entries.len());
        let block = Block::new()
            .borders(Borders::ALL)
            .title(format!(" {} ", dim.label))
            .title_bottom(Line::from(count).right_aligned());
        let list = List::new(items)
            .block(block)
            .highlight_symbol("> ")
            .highlight_style(Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .scroll_padding(2);
        self.list_state.select(Some(self.view.selected));
        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect, preview: Option<&Preview>) {
        let block = preview_block();
        let inner = block.inner(area);
        self.preview_area = inner;
        f.render_widget(block, area);
        match preview {
            None => {
                let msg = if self.view.entries.is_empty() {
                    "no files"
                } else {
                    "rendering…"
                };
                f.render_widget(
                    Paragraph::new(msg).style(Style::new().fg(Color::DarkGray)),
                    inner,
                );
            }
            Some(Preview::Card(lines)) => {
                let text: Vec<Line> = lines.iter().map(|l| Line::raw(l.as_str())).collect();
                f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
            }
            Some(Preview::Kitty(k)) => k.render(inner, f.buffer_mut()),
            Some(Preview::Proto(p)) => {
                f.render_widget(ratatui_image::Image::new(p), inner);
            }
        }
    }

    fn draw_picker(&self, f: &mut Frame, area: Rect) {
        let t = &self.table;
        let cands = t.free_candidates();
        let items: Vec<ListItem> = cands
            .iter()
            .enumerate()
            .map(|(i, &d)| {
                let dim = &t.dims[d];
                let mut sample: Vec<&str> = dim.values.iter().take(4).map(|s| s.as_str()).collect();
                if dim.values.len() > 4 {
                    sample.push("…");
                }
                let current = if d == self.view.free {
                    "  (current)"
                } else {
                    ""
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:>2}  ", i + 1), Style::new().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{:<16}", dim.label),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{:>5} values   ", dim.values.len())),
                    Span::styled(sample.join(", "), Style::new().fg(Color::DarkGray)),
                    Span::styled(current, Style::new().fg(Color::Yellow)),
                ]))
            })
            .collect();
        let constant: Vec<String> = t
            .dims
            .iter()
            .filter(|d| d.is_constant())
            .map(|d| {
                format!(
                    "{}={}",
                    d.label,
                    d.values.first().map_or("", |s| s.as_str())
                )
            })
            .collect();
        let mut bottom = String::from(" Enter: browse along this axis · Esc: back ");
        if !constant.is_empty() {
            bottom = format!(" constant: {} ·{bottom}", constant.join("  "));
        }
        let block = Block::new()
            .borders(Borders::ALL)
            .title(" choose the free axis ")
            .title_bottom(bottom);
        let list = List::new(items)
            .block(block)
            .highlight_symbol("> ")
            .highlight_style(Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD));
        let mut st = ListState::default().with_selected(Some(self.picker_sel));
        f.render_stateful_widget(list, area, &mut st);
    }

    fn draw_status(&self, f: &mut Frame, area: Rect) {
        let path = self
            .view
            .selected_row()
            .map(|r| self.table.row(r).path.display().to_string())
            .unwrap_or_default();
        let msg = match &self.message {
            Some((m, _)) => Span::styled(m.clone(), Style::new().fg(Color::Yellow)),
            None => Span::styled(self.notice.clone(), Style::new().fg(Color::DarkGray)),
        };
        let keys = match self.screen {
            Screen::Browser => {
                " j/k move · Tab focus · h/l change · f pivot · v views · o open · y copy · q quit "
            }
            Screen::Picker => " j/k choose · Enter browse · Esc back · q quit ",
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .title_bottom(Line::from(keys).right_aligned());
        let lines = vec![Line::raw(path), Line::from(msg)];
        f.render_widget(Paragraph::new(lines).block(block), area);
    }
}

fn preview_block() -> Block<'static> {
    Block::new().borders(Borders::ALL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::FileRecord;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::BTreeMap;

    fn app() -> App {
        let mut files = Vec::new();
        for r in ["mean-rejected", "median-rejected"] {
            for t in ["300K", "364K", "1000K"] {
                for f in ["beta_hist", "tau"] {
                    files.push(FileRecord {
                        parts: vec![r.into(), "gaussian-normalized".into(), t.into(), f.into()],
                        ext: "pdf".into(),
                        path: PathBuf::from(format!("{r}/gaussian-normalized/{t}/{f}.pdf")),
                    });
                }
            }
        }
        let t = Table::build(files, &BTreeMap::new());
        App::new(t, "debug/fit".into(), "D=3 · 12 files".into(), 5)
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn screen_text(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| app.draw(f, None)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn starts_in_picker_on_deepest_dir_level() {
        let mut a = app();
        assert_eq!(a.screen, Screen::Picker);
        assert_eq!(a.view.free, 2);
        let s = screen_text(&mut a);
        assert!(s.contains("choose the free axis"), "{s}");
        assert!(s.contains("K") && s.contains("3 values"), "{s}");
        assert!(
            s.contains("constant: normalized=gaussian-normalized"),
            "{s}"
        );
        a.handle_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Browser);
        let s = screen_text(&mut a);
        assert!(s.contains("free: [K]"), "{s}");
        assert!(s.contains("‹rejected=mean›"), "{s}");
        assert!(s.contains("normalized=gaussian"), "{s}");
        assert!(s.contains("file=beta_hist"), "{s}");
        assert!(s.contains("> 300K"), "{s}");
        assert!(s.contains("rendering…"), "{s}");
        assert!(
            s.contains("mean-rejected/gaussian-normalized/300K/beta_hist.pdf"),
            "{s}"
        );
    }

    #[test]
    fn browser_keys() {
        let mut a = app();
        a.handle_key(key(KeyCode::Enter));
        a.handle_key(key(KeyCode::Char('j')));
        assert_eq!(a.view.selected, 1);
        a.handle_key(key(KeyCode::Char('G')));
        assert_eq!(a.view.selected, 2);
        a.handle_key(key(KeyCode::Char('l'))); // rejected → median
        assert_eq!(a.view.fixed[0], 1);
        assert!(a.selected_path().unwrap().starts_with("median-rejected"));
        a.handle_key(key(KeyCode::Char('l'))); // no further value
        assert!(screen_text(&mut a).contains("already at the last value"));
        a.handle_key(key(KeyCode::Tab));
        assert_eq!(a.view.focus, Some(3));
        a.handle_key(key(KeyCode::Char('f'))); // pivot to file
        assert_eq!(a.view.free, 3);
        assert_eq!(a.view.focus, Some(2));
        assert_eq!(
            a.selected_path().unwrap(),
            PathBuf::from("median-rejected/gaussian-normalized/1000K/beta_hist.pdf")
        );
        a.handle_key(key(KeyCode::Char('v')));
        assert_eq!(a.screen, Screen::Picker);
        a.handle_key(key(KeyCode::Char('1')));
        assert_eq!(a.view.free, 0);
        assert!(matches!(
            a.handle_key(key(KeyCode::Char('y'))),
            Action::Copy(_)
        ));
        assert!(matches!(
            a.handle_key(key(KeyCode::Char('o'))),
            Action::Open(_)
        ));
        assert!(matches!(
            a.handle_key(key(KeyCode::Char('q'))),
            Action::Quit
        ));
    }

    #[test]
    fn prefetch_order() {
        let mut a = app();
        a.handle_key(key(KeyCode::Enter));
        a.handle_key(key(KeyCode::Char('j'))); // 364K
        let w = a.wanted();
        let names: Vec<String> = w.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            names,
            [
                "mean-rejected/gaussian-normalized/364K/beta_hist.pdf",
                "mean-rejected/gaussian-normalized/1000K/beta_hist.pdf",
                "mean-rejected/gaussian-normalized/300K/beta_hist.pdf",
                "median-rejected/gaussian-normalized/364K/beta_hist.pdf",
            ]
        );
    }
}
