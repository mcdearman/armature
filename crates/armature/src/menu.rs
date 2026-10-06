//! The menu bar: the File, Edit and other menus an app offers. On macOS
//! they go in the system's menu bar at the top of the screen; elsewhere the
//! toolkit draws them at the top of the window (see [`Chrome::menu_bar`]).
//!
//! [`Chrome::menu_bar`]: crate::Chrome::menu_bar

use crate::event::{Key, KeyEvent};

/// A key combination that chooses a menu entry without opening the menu.
/// It always includes the platform's command key: Command on macOS, Ctrl
/// elsewhere.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub key: Key,
    pub shift: bool,
    pub alt: bool,
}

impl Shortcut {
    /// The command key with a character key, such as `"o"`.
    pub fn command(key: &str) -> Self {
        Self { key: Key::Character(key.to_lowercase()), shift: false, alt: false }
    }

    pub fn shift(mut self) -> Self {
        self.shift = true;
        self
    }

    pub fn alt(mut self) -> Self {
        self.alt = true;
        self
    }

    /// Whether a key press is this shortcut.
    pub fn matches(&self, k: &KeyEvent) -> bool {
        let same_key = match (&self.key, &k.key) {
            (Key::Character(a), Key::Character(b)) => a.eq_ignore_ascii_case(b),
            (a, b) => a == b,
        };
        k.pressed && same_key && k.modifiers.command() && k.modifiers.shift == self.shift && k.modifiers.alt == self.alt
    }

    fn key_name(&self) -> String {
        match &self.key {
            Key::Character(c) => c.to_uppercase(),
            Key::Enter => "Enter".into(),
            Key::Space => "Space".into(),
            Key::Tab => "Tab".into(),
            Key::Backspace => "Backspace".into(),
            Key::Delete => "Delete".into(),
            other => format!("{other:?}"),
        }
    }

    /// How the shortcut is written in a menu: `⇧⌘O` on macOS, `Ctrl+Shift+O`
    /// elsewhere.
    pub fn label(&self) -> String {
        if cfg!(target_os = "macos") {
            format!("{}{}⌘{}", if self.alt { "⌥" } else { "" }, if self.shift { "⇧" } else { "" }, self.key_name())
        } else {
            format!("Ctrl+{}{}{}", if self.shift { "Shift+" } else { "" }, if self.alt { "Alt+" } else { "" }, self.key_name())
        }
    }

    /// The shortcut in the form the system's menu bar reads.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn accelerator(&self) -> String {
        format!("CmdOrCtrl+{}{}{}", if self.shift { "Shift+" } else { "" }, if self.alt { "Alt+" } else { "" }, self.key_name())
    }
}

/// One row of a [`Menu`].
#[derive(Clone, Debug)]
pub struct MenuEntry<M> {
    pub label: String,
    pub shortcut: Option<Shortcut>,
    /// Sent when the entry is chosen. Without one it is greyed out.
    pub message: Option<M>,
    /// A dividing line; the other fields are unused.
    pub separator: bool,
}

impl<M> MenuEntry<M> {
    pub fn new(label: impl Into<String>, message: M) -> Self {
        Self { label: label.into(), shortcut: None, message: Some(message), separator: false }
    }

    /// An entry that is shown but cannot be chosen.
    pub fn disabled(label: impl Into<String>) -> Self {
        Self { label: label.into(), shortcut: None, message: None, separator: false }
    }

    pub fn separator() -> Self {
        Self { label: String::new(), shortcut: None, message: None, separator: true }
    }

    pub fn shortcut(mut self, s: Shortcut) -> Self {
        self.shortcut = Some(s);
        self
    }

    /// Greys the entry out unless `on`, for commands that need something
    /// the app does not have right now, such as an open file to save.
    pub fn enabled(mut self, on: bool) -> Self {
        if !on {
            self.message = None;
        }
        self
    }
}

/// One menu of the menu bar, such as File.
#[derive(Clone, Debug)]
pub struct Menu<M> {
    pub title: String,
    pub entries: Vec<MenuEntry<M>>,
}

impl<M> Menu<M> {
    pub fn new(title: impl Into<String>) -> Self {
        Self { title: title.into(), entries: vec![] }
    }

    pub fn push(mut self, entry: MenuEntry<M>) -> Self {
        self.entries.push(entry);
        self
    }

    pub fn separator(self) -> Self {
        self.push(MenuEntry::separator())
    }
}

/// The message of the first enabled entry whose shortcut is this key press.
pub(crate) fn shortcut_message<M: Clone>(menus: &[Menu<M>], k: &KeyEvent) -> Option<M> {
    menus.iter().flat_map(|m| &m.entries).find(|e| e.shortcut.as_ref().is_some_and(|s| s.matches(k))).and_then(|e| e.message.clone())
}

/// Changes whenever the menus would look or behave differently, so the
/// system's menu bar is rebuilt only then.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn signature<M>(menus: &[Menu<M>]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for m in menus {
        m.title.hash(&mut h);
        for e in &m.entries {
            (&e.label, &e.shortcut, e.message.is_some(), e.separator).hash(&mut h);
        }
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Modifiers;

    fn press(c: &str, shift: bool) -> KeyEvent {
        let command = Modifiers { logo: cfg!(target_os = "macos"), ctrl: !cfg!(target_os = "macos"), shift, ..Default::default() };
        KeyEvent { key: Key::Character(c.into()), pressed: true, repeat: false, modifiers: command, text: None }
    }

    #[test]
    fn a_shortcut_needs_exactly_its_modifiers() {
        let save = Shortcut::command("s");
        assert!(save.matches(&press("s", false)));
        assert!(save.matches(&press("S", false)), "whatever the case of the key");
        assert!(!save.matches(&press("s", true)), "Shift makes it a different shortcut");
        assert!(Shortcut::command("s").shift().matches(&press("s", true)));
        let plain = KeyEvent { modifiers: Modifiers::default(), ..press("s", false) };
        assert!(!save.matches(&plain), "not without the command key");
        let released = KeyEvent { pressed: false, ..press("s", false) };
        assert!(!save.matches(&released));
    }

    #[test]
    fn shortcuts_are_written_the_way_the_platform_does() {
        let s = Shortcut::command("o").shift();
        assert_eq!(s.label(), if cfg!(target_os = "macos") { "⇧⌘O" } else { "Ctrl+Shift+O" });
        assert_eq!(s.accelerator(), "CmdOrCtrl+Shift+O");
    }

    #[test]
    fn a_shortcut_finds_its_entry_unless_it_is_disabled() {
        let menus = vec![
            Menu::new("File").push(MenuEntry::new("Open", 1).shortcut(Shortcut::command("o"))).separator().push(MenuEntry::new("Save", 2).shortcut(Shortcut::command("s")).enabled(false)),
            Menu::new("View").push(MenuEntry::new("Zoom", 3).shortcut(Shortcut::command("=")))
        ];
        assert_eq!(shortcut_message(&menus, &press("o", false)), Some(1));
        assert_eq!(shortcut_message(&menus, &press("=", false)), Some(3));
        assert_eq!(shortcut_message(&menus, &press("s", false)), None, "greyed out");
        assert_eq!(shortcut_message(&menus, &press("x", false)), None);
        let mut enabled = menus.clone();
        enabled[0].entries[2].message = Some(2);
        assert_ne!(signature(&menus), signature(&enabled), "enabling an entry changes the menu bar");
        assert_eq!(signature(&menus), signature(&menus.clone()));
    }
}
