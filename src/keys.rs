//! Key bindings: actions, key specs (`"j"`, `"L"`, `"ctrl-d"`, `"shift-tab"`), defaults, and
//! the effective keymap built from the config's `[keys]` tables.
//!
//! ```toml
//! [keys]
//! focus-next = "L"              # replaces the default (tab)
//! focus-prev = ["H", "shift-tab"]
//! open = []                     # unbind
//! ```
//!
//! Setting an action replaces its default keys. A key bound explicitly takes priority over
//! defaults and lower-precedence config files: it is removed from whatever action had it.
//! Binding one key to two actions in the same file is an error. `ctrl-c` always quits.

use std::collections::BTreeMap;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    MoveDown,
    MoveUp,
    PageDown,
    PageUp,
    HalfPageDown,
    HalfPageUp,
    First,
    Last,
    FocusNext,
    FocusPrev,
    ValueNext,
    ValuePrev,
    Pivot,
    ViewPicker,
    Open,
    CopyPath,
    Quit,
}

impl Action {
    pub const ALL: [Action; 17] = [
        Action::MoveDown,
        Action::MoveUp,
        Action::PageDown,
        Action::PageUp,
        Action::HalfPageDown,
        Action::HalfPageUp,
        Action::First,
        Action::Last,
        Action::FocusNext,
        Action::FocusPrev,
        Action::ValueNext,
        Action::ValuePrev,
        Action::Pivot,
        Action::ViewPicker,
        Action::Open,
        Action::CopyPath,
        Action::Quit,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Action::MoveDown => "move-down",
            Action::MoveUp => "move-up",
            Action::PageDown => "page-down",
            Action::PageUp => "page-up",
            Action::HalfPageDown => "half-page-down",
            Action::HalfPageUp => "half-page-up",
            Action::First => "first",
            Action::Last => "last",
            Action::FocusNext => "focus-next",
            Action::FocusPrev => "focus-prev",
            Action::ValueNext => "value-next",
            Action::ValuePrev => "value-prev",
            Action::Pivot => "pivot",
            Action::ViewPicker => "view-picker",
            Action::Open => "open",
            Action::CopyPath => "copy-path",
            Action::Quit => "quit",
        }
    }

    pub fn defaults(self) -> &'static [&'static str] {
        match self {
            Action::MoveDown => &["j", "down"],
            Action::MoveUp => &["k", "up"],
            Action::PageDown => &["pagedown"],
            Action::PageUp => &["pageup"],
            Action::HalfPageDown => &["ctrl-d"],
            Action::HalfPageUp => &["ctrl-u"],
            Action::First => &["g", "home"],
            Action::Last => &["G", "end"],
            Action::FocusNext => &["tab"],
            Action::FocusPrev => &["shift-tab"],
            Action::ValueNext => &["l", "right"],
            Action::ValuePrev => &["h", "left"],
            Action::Pivot => &["f"],
            Action::ViewPicker => &["v"],
            Action::Open => &["o", "enter"],
            Action::CopyPath => &["y"],
            Action::Quit => &["q"],
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A key with its modifiers, normalized so config specs and terminal events compare equal:
/// letters carry their case instead of SHIFT, and shift-tab is `BackTab`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    code: KeyCodeOrd,
    mods: u8,
}

/// `KeyCode` subset we bind, with a total order (for stable maps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum KeyCodeOrd {
    Char(char),
    F(u8),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

const CTRL: u8 = 1;
const ALT: u8 = 2;

impl Key {
    pub fn from_event(k: &KeyEvent) -> Option<Key> {
        let mut mods = 0;
        if k.modifiers.contains(KeyModifiers::CONTROL) {
            mods |= CTRL;
        }
        if k.modifiers.contains(KeyModifiers::ALT) {
            mods |= ALT;
        }
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let code = match k.code {
            KeyCode::Char(c) => {
                // Ctrl/Alt with shift may report the lowercase letter plus SHIFT.
                if shift && c.is_lowercase() {
                    KeyCodeOrd::Char(c.to_uppercase().next().unwrap_or(c))
                } else {
                    KeyCodeOrd::Char(c)
                }
            }
            KeyCode::F(n) => KeyCodeOrd::F(n),
            KeyCode::Enter => KeyCodeOrd::Enter,
            KeyCode::Esc => KeyCodeOrd::Esc,
            KeyCode::Tab if shift => KeyCodeOrd::BackTab,
            KeyCode::Tab => KeyCodeOrd::Tab,
            KeyCode::BackTab => KeyCodeOrd::BackTab,
            KeyCode::Backspace => KeyCodeOrd::Backspace,
            KeyCode::Delete => KeyCodeOrd::Delete,
            KeyCode::Insert => KeyCodeOrd::Insert,
            KeyCode::Up => KeyCodeOrd::Up,
            KeyCode::Down => KeyCodeOrd::Down,
            KeyCode::Left => KeyCodeOrd::Left,
            KeyCode::Right => KeyCodeOrd::Right,
            KeyCode::Home => KeyCodeOrd::Home,
            KeyCode::End => KeyCodeOrd::End,
            KeyCode::PageUp => KeyCodeOrd::PageUp,
            KeyCode::PageDown => KeyCodeOrd::PageDown,
            _ => return None,
        };
        Some(Key { code, mods })
    }

    /// Parse a key spec: a single character (`j`, `L`, `?`, `-`), or a named key (`tab`,
    /// `enter`, `esc`, `space`, `up`, `pagedown`, `f1`, …), optionally prefixed with
    /// `ctrl-`, `alt-` or `shift-` (case-insensitive; `c-`, `a-`/`m-`, `s-` also work).
    pub fn parse(spec: &str) -> Result<Key, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err("empty key".into());
        }
        let mut rest = spec;
        let mut mods = 0u8;
        let mut shift = false;
        loop {
            if rest.chars().count() <= 1 {
                break;
            }
            let lower = rest.to_ascii_lowercase();
            let prefix = ["ctrl-", "c-", "alt-", "a-", "m-", "shift-", "s-"]
                .into_iter()
                .find(|p| lower.starts_with(p) && rest.len() > p.len());
            let Some(p) = prefix else { break };
            match p {
                "ctrl-" | "c-" => mods |= CTRL,
                "alt-" | "a-" | "m-" => mods |= ALT,
                _ => shift = true,
            }
            rest = &rest[p.len()..];
        }
        let mut chars = rest.chars();
        let code = match (chars.next(), chars.next()) {
            (Some(c), None) => {
                if shift && c.is_alphabetic() {
                    KeyCodeOrd::Char(c.to_uppercase().next().unwrap_or(c))
                } else if shift && !c.is_alphabetic() {
                    return Err(format!(
                        "`{spec}`: write the shifted character itself instead of shift-"
                    ));
                } else {
                    KeyCodeOrd::Char(c)
                }
            }
            _ => {
                let name = rest.to_ascii_lowercase();
                let code = match name.as_str() {
                    "space" => KeyCodeOrd::Char(' '),
                    "enter" | "return" | "cr" => KeyCodeOrd::Enter,
                    "esc" | "escape" => KeyCodeOrd::Esc,
                    "tab" if shift => KeyCodeOrd::BackTab,
                    "tab" => KeyCodeOrd::Tab,
                    "backtab" => KeyCodeOrd::BackTab,
                    "backspace" | "bs" => KeyCodeOrd::Backspace,
                    "delete" | "del" => KeyCodeOrd::Delete,
                    "insert" | "ins" => KeyCodeOrd::Insert,
                    "up" => KeyCodeOrd::Up,
                    "down" => KeyCodeOrd::Down,
                    "left" => KeyCodeOrd::Left,
                    "right" => KeyCodeOrd::Right,
                    "home" => KeyCodeOrd::Home,
                    "end" => KeyCodeOrd::End,
                    "pageup" | "pgup" => KeyCodeOrd::PageUp,
                    "pagedown" | "pgdown" | "pgdn" => KeyCodeOrd::PageDown,
                    f if f.starts_with('f') && f.len() > 1 => match f[1..].parse::<u8>() {
                        Ok(n @ 1..=24) => KeyCodeOrd::F(n),
                        _ => return Err(format!("unknown key `{spec}`")),
                    },
                    _ => return Err(format!("unknown key `{spec}`")),
                };
                if shift && !matches!(code, KeyCodeOrd::BackTab) {
                    return Err(format!("`{spec}`: shift- only works with letters and tab"));
                }
                code
            }
        };
        if code == KeyCodeOrd::Char('c') && mods == CTRL {
            return Err("ctrl-c is reserved (it always quits)".into());
        }
        Ok(Key { code, mods })
    }
}

impl fmt::Display for Key {
    /// Short form for the help line: `j`, `L`, `Tab`, `S-Tab`, `C-d`, `↓`, `PgDn`.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.mods & CTRL != 0 {
            f.write_str("C-")?;
        }
        if self.mods & ALT != 0 {
            f.write_str("M-")?;
        }
        match self.code {
            KeyCodeOrd::Char(' ') => f.write_str("Space"),
            KeyCodeOrd::Char(c) => write!(f, "{c}"),
            KeyCodeOrd::F(n) => write!(f, "F{n}"),
            KeyCodeOrd::Enter => f.write_str("Enter"),
            KeyCodeOrd::Esc => f.write_str("Esc"),
            KeyCodeOrd::Tab => f.write_str("Tab"),
            KeyCodeOrd::BackTab => f.write_str("S-Tab"),
            KeyCodeOrd::Backspace => f.write_str("Bksp"),
            KeyCodeOrd::Delete => f.write_str("Del"),
            KeyCodeOrd::Insert => f.write_str("Ins"),
            KeyCodeOrd::Up => f.write_str("↑"),
            KeyCodeOrd::Down => f.write_str("↓"),
            KeyCodeOrd::Left => f.write_str("←"),
            KeyCodeOrd::Right => f.write_str("→"),
            KeyCodeOrd::Home => f.write_str("Home"),
            KeyCodeOrd::End => f.write_str("End"),
            KeyCodeOrd::PageUp => f.write_str("PgUp"),
            KeyCodeOrd::PageDown => f.write_str("PgDn"),
        }
    }
}

impl Key {
    /// The canonical config spec (round-trips through `parse`).
    pub fn spec(&self) -> String {
        let mut s = String::new();
        if self.mods & CTRL != 0 {
            s.push_str("ctrl-");
        }
        if self.mods & ALT != 0 {
            s.push_str("alt-");
        }
        let name = match self.code {
            KeyCodeOrd::Char(' ') => "space".to_string(),
            KeyCodeOrd::Char(c) => c.to_string(),
            KeyCodeOrd::F(n) => format!("f{n}"),
            KeyCodeOrd::Enter => "enter".into(),
            KeyCodeOrd::Esc => "esc".into(),
            KeyCodeOrd::Tab => "tab".into(),
            KeyCodeOrd::BackTab => "shift-tab".into(),
            KeyCodeOrd::Backspace => "backspace".into(),
            KeyCodeOrd::Delete => "delete".into(),
            KeyCodeOrd::Insert => "insert".into(),
            KeyCodeOrd::Up => "up".into(),
            KeyCodeOrd::Down => "down".into(),
            KeyCodeOrd::Left => "left".into(),
            KeyCodeOrd::Right => "right".into(),
            KeyCodeOrd::Home => "home".into(),
            KeyCodeOrd::End => "end".into(),
            KeyCodeOrd::PageUp => "pageup".into(),
            KeyCodeOrd::PageDown => "pagedown".into(),
        };
        s.push_str(&name);
        s
    }
}

/// The keys bound to one action in a config file: a string or an array of strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings(pub Vec<Key>);

impl<'de> Deserialize<'de> for Bindings {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(String),
            Many(Vec<String>),
        }
        let raw = Raw::deserialize(d).map_err(|_| {
            serde::de::Error::custom("expected a key (\"L\") or a list of keys ([\"L\", \"tab\"])")
        })?;
        let specs = match raw {
            Raw::One(s) => vec![s],
            Raw::Many(v) => v,
        };
        let mut keys = Vec::with_capacity(specs.len());
        for s in &specs {
            let k = Key::parse(s).map_err(serde::de::Error::custom)?;
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        Ok(Bindings(keys))
    }
}

/// Reject a `[keys]` table that binds one key to two actions.
pub fn check_duplicates(keys: &BTreeMap<Action, Bindings>) -> Result<(), String> {
    let mut seen: BTreeMap<Key, Action> = BTreeMap::new();
    for (action, b) in keys {
        for k in &b.0 {
            if let Some(other) = seen.insert(*k, *action) {
                return Err(format!(
                    "[keys]: `{}` is bound to both {other} and {action}",
                    k.spec()
                ));
            }
        }
    }
    Ok(())
}

/// The effective bindings.
#[derive(Debug, Clone)]
pub struct KeyMap {
    by_key: BTreeMap<Key, Action>,
    by_action: BTreeMap<Action, Vec<Key>>,
}

impl Default for KeyMap {
    fn default() -> Self {
        KeyMap::build(&[])
    }
}

impl KeyMap {
    /// Build from explicit bindings in increasing precedence (`[global, project]`). A later
    /// layer replaces an action's keys and takes its keys away from other actions.
    pub fn build(layers: &[&BTreeMap<Action, Bindings>]) -> KeyMap {
        let mut by_action: BTreeMap<Action, Vec<Key>> = Action::ALL
            .iter()
            .map(|&a| {
                let keys = a
                    .defaults()
                    .iter()
                    .map(|s| Key::parse(s).expect("default key specs are valid"))
                    .collect();
                (a, keys)
            })
            .collect();
        for layer in layers {
            for (action, b) in layer.iter() {
                for keys in by_action.values_mut() {
                    keys.retain(|k| !b.0.contains(k));
                }
                by_action.insert(*action, b.0.clone());
            }
        }
        let by_key = by_action
            .iter()
            .flat_map(|(a, keys)| keys.iter().map(move |k| (*k, *a)))
            .collect();
        KeyMap { by_key, by_action }
    }

    pub fn action(&self, k: &KeyEvent) -> Option<Action> {
        Key::from_event(k).and_then(|k| self.by_key.get(&k).copied())
    }

    pub fn keys(&self, a: Action) -> &[Key] {
        self.by_action.get(&a).map_or(&[], |v| v.as_slice())
    }

    /// First key of an action, for help text (`None` if unbound).
    pub fn hint(&self, a: Action) -> Option<String> {
        self.keys(a).first().map(|k| k.to_string())
    }

    /// `"j/k"` for a pair of actions; falls back to whichever is bound.
    pub fn pair_hint(&self, a: Action, b: Action) -> Option<String> {
        match (self.hint(a), self.hint(b)) {
            (Some(x), Some(y)) => Some(format!("{x}/{y}")),
            (x, y) => x.or(y),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn parse_and_match_events() {
        let cases: &[(&str, KeyEvent)] = &[
            ("j", ev(KeyCode::Char('j'), KeyModifiers::NONE)),
            ("L", ev(KeyCode::Char('L'), KeyModifiers::SHIFT)),
            ("shift-l", ev(KeyCode::Char('L'), KeyModifiers::SHIFT)),
            ("S-h", ev(KeyCode::Char('H'), KeyModifiers::SHIFT)),
            ("shift-tab", ev(KeyCode::BackTab, KeyModifiers::SHIFT)),
            ("backtab", ev(KeyCode::Tab, KeyModifiers::SHIFT)),
            ("ctrl-d", ev(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            (
                "C-D",
                ev(
                    KeyCode::Char('D'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
            ),
            (
                "ctrl-shift-d",
                ev(
                    KeyCode::Char('d'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
            ),
            ("alt-j", ev(KeyCode::Char('j'), KeyModifiers::ALT)),
            ("space", ev(KeyCode::Char(' '), KeyModifiers::NONE)),
            ("Enter", ev(KeyCode::Enter, KeyModifiers::NONE)),
            ("pgdn", ev(KeyCode::PageDown, KeyModifiers::NONE)),
            ("f5", ev(KeyCode::F(5), KeyModifiers::NONE)),
            ("-", ev(KeyCode::Char('-'), KeyModifiers::NONE)),
            ("?", ev(KeyCode::Char('?'), KeyModifiers::SHIFT)),
        ];
        for (spec, e) in cases {
            let k = Key::parse(spec).unwrap_or_else(|err| panic!("{spec}: {err}"));
            assert_eq!(Some(k), Key::from_event(e), "{spec}");
            assert_eq!(Key::parse(&k.spec()).unwrap(), k, "round trip {spec}");
        }
        assert_ne!(Key::parse("l"), Key::parse("L"));
        for bad in ["", "foo", "f99", "ctrl-c", "shift-1", "shift-enter"] {
            assert!(Key::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn defaults() {
        let m = KeyMap::default();
        let a = |c, mods| m.action(&ev(c, mods));
        assert_eq!(
            a(KeyCode::Char('j'), KeyModifiers::NONE),
            Some(Action::MoveDown)
        );
        assert_eq!(
            a(KeyCode::Char('G'), KeyModifiers::SHIFT),
            Some(Action::Last)
        );
        assert_eq!(a(KeyCode::Tab, KeyModifiers::NONE), Some(Action::FocusNext));
        assert_eq!(
            a(KeyCode::BackTab, KeyModifiers::SHIFT),
            Some(Action::FocusPrev)
        );
        assert_eq!(
            a(KeyCode::Char('d'), KeyModifiers::CONTROL),
            Some(Action::HalfPageDown)
        );
        assert_eq!(a(KeyCode::Char('d'), KeyModifiers::NONE), None);
        assert_eq!(
            m.pair_hint(Action::MoveDown, Action::MoveUp).unwrap(),
            "j/k"
        );
        assert_eq!(
            m.pair_hint(Action::FocusNext, Action::FocusPrev).unwrap(),
            "Tab/S-Tab"
        );
        // Every action has a default, and no key is bound twice.
        let mut all: Vec<Key> = Action::ALL
            .iter()
            .flat_map(|&a| m.keys(a).to_vec())
            .collect();
        let n = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), n);
    }

    #[test]
    fn layers_replace_and_steal() {
        let k = |s: &str| Key::parse(s).unwrap();
        let global: BTreeMap<Action, Bindings> = [
            (Action::FocusNext, Bindings(vec![k("L")])),
            (Action::FocusPrev, Bindings(vec![k("H")])),
        ]
        .into();
        // The project takes `l` for pivot; value-next keeps only `right`.
        let project: BTreeMap<Action, Bindings> = [
            (Action::Pivot, Bindings(vec![k("l")])),
            (Action::Open, Bindings(vec![])),
        ]
        .into();
        let m = KeyMap::build(&[&global, &project]);
        assert_eq!(m.keys(Action::FocusNext), &[k("L")]);
        assert_eq!(m.keys(Action::FocusPrev), &[k("H")]);
        assert_eq!(m.keys(Action::Pivot), &[k("l")]);
        assert_eq!(m.keys(Action::ValueNext), &[k("right")]);
        assert!(m.keys(Action::Open).is_empty());
        assert_eq!(m.action(&ev(KeyCode::Tab, KeyModifiers::NONE)), None);
        assert_eq!(m.action(&ev(KeyCode::Enter, KeyModifiers::NONE)), None);
        assert_eq!(
            m.pair_hint(Action::FocusNext, Action::FocusPrev).unwrap(),
            "L/H"
        );
        assert_eq!(m.hint(Action::Open), None);

        let dup: BTreeMap<Action, Bindings> = [
            (Action::FocusNext, Bindings(vec![k("L")])),
            (Action::ValueNext, Bindings(vec![k("L")])),
        ]
        .into();
        let err = check_duplicates(&dup).unwrap_err();
        assert!(
            err.contains("`L` is bound to both focus-next and value-next"),
            "{err}"
        );
    }
}
