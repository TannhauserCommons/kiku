//! Keyboard shortcuts: the captions key that is held (Ctrl+Win by default), and the key that runs
//! each command while it is held. Saved in settings as e.g. `hold = ctrl+win` and `key.paste = V`.

/// A key as people name it. Modifiers match either the left or the right one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Ctrl,
    Win,
    Alt,
    Shift,
    /// Any other key, by Windows virtual-key code.
    Other(u16),
}

const VK_CAPITAL: u16 = 0x14;
const VK_F1: u16 = 0x70;
const VK_F24: u16 = 0x87;

/// Keys with names, by virtual-key code: the first name is how a key is written, the rest also read.
const NAMED_KEYS: &[(u16, &[&str])] = &[
    (VK_CAPITAL, &["Caps Lock"]),
    (0x0D, &["Enter"]),
    (0x09, &["Tab"]),
    (0x1B, &["Esc", "Escape"]),
    (0x20, &["Space"]),
    (0x08, &["Backspace"]),
    (0x2E, &["Delete", "Del"]),
    (0x2D, &["Insert"]),
    (0x24, &["Home"]),
    (0x23, &["End"]),
    (0x21, &["Page Up", "PageUp", "PgUp"]),
    (0x22, &["Page Down", "PageDown", "PgDn"]),
    (0x26, &["Up"]),
    (0x28, &["Down"]),
    (0x25, &["Left"]),
    (0x27, &["Right"]),
];

/// How a key with a name is written, if it has one.
fn named(vk: u16) -> Option<&'static str> {
    NAMED_KEYS.iter().find(|(code, _)| *code == vk).map(|(_, names)| names[0])
}

impl Key {
    pub fn from_vk(vk: u16) -> Self {
        match vk {
            0x11 | 0xA2 | 0xA3 => Key::Ctrl,
            0x5B | 0x5C => Key::Win,
            0x12 | 0xA4 | 0xA5 => Key::Alt,
            0x10 | 0xA0 | 0xA1 => Key::Shift,
            vk => Key::Other(vk),
        }
    }

    pub fn is_modifier(self) -> bool {
        !matches!(self, Key::Other(_))
    }

    /// Letters, digits and the like, which would stop typing if held as the captions key.
    fn types_text(self) -> bool {
        // Space, Enter, Backspace, Tab, Escape, digits, letters, then the punctuation and numpad keys.
        matches!(self, Key::Other(vk) if matches!(vk, 0x08 | 0x09 | 0x0D | 0x1B | 0x20 | 0x30..=0x39 | 0x41..=0x5A | 0x60..=0x6F | 0xBA..=0xC0 | 0xDB..=0xDF))
    }

    pub fn name(self) -> String {
        match self {
            Key::Ctrl => "Ctrl".into(),
            Key::Win => "Win".into(),
            Key::Alt => "Alt".into(),
            Key::Shift => "Shift".into(),
            Key::Other(vk) if named(vk).is_some() => named(vk).unwrap_or_default().to_owned(),
            Key::Other(vk @ VK_F1..=VK_F24) => format!("F{}", vk - VK_F1 + 1),
            Key::Other(vk @ (0x30..=0x39 | 0x41..=0x5A)) => (vk as u8 as char).to_string(),
            Key::Other(vk) => format!("Key {vk:#04x}"),
        }
    }

    pub(crate) fn parse(name: &str) -> Option<Self> {
        let name = name.trim();
        let spaced = name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        if let Some((vk, _)) = NAMED_KEYS.iter().find(|(_, names)| names.iter().any(|known| known.to_lowercase() == spaced)) {
            return Some(Key::Other(*vk));
        }
        Some(match spaced.as_str() {
            "ctrl" => Key::Ctrl,
            "win" => Key::Win,
            "alt" => Key::Alt,
            "shift" => Key::Shift,
            lower => {
                if let Some(number) = lower.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()).filter(|n| (1..=24).contains(n)) {
                    Key::Other(VK_F1 + number - 1)
                } else if let Some(code) = lower.strip_prefix("key 0x") {
                    Key::from_vk(u16::from_str_radix(code, 16).ok()?)
                } else {
                    let mut chars = name.chars();
                    let (Some(c), None) = (chars.next(), chars.next()) else { return None };
                    let c = c.to_ascii_uppercase();
                    c.is_ascii_alphanumeric().then_some(Key::Other(c as u16))?
                }
            }
        })
    }
}

/// What a key does while the captions key is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    Paste,
    Undo,
    Stop,
    Lock,
    Hide,
    Quit,
    SaveMiss,
    Subtitles,
    /// The macro palette: what can be said here.
    Palette,
}

impl Command {
    pub const ALL: [Command; 9] = [
        Command::Paste,
        Command::Undo,
        Command::Stop,
        Command::Lock,
        Command::Hide,
        Command::Quit,
        Command::SaveMiss,
        Command::Subtitles,
        Command::Palette,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Command::Paste => "Paste what I said (again: reach further back)",
            Command::Undo => "Erase what was just typed",
            Command::Stop => "Stop or start captioning",
            Command::Lock => "Lock or unlock the panel",
            Command::Hide => "Hide or show the panel",
            Command::Quit => "Quit",
            Command::SaveMiss => "Save the last 30 s (a missed word)",
            Command::Subtitles => "Switch between transcript and captions",
            Command::Palette => "Show the macros you can say here",
        }
    }

    /// The word in the footer hint, e.g. "paste".
    pub fn short(self) -> &'static str {
        match self {
            Command::Paste => "paste",
            Command::Undo => "undo",
            Command::Stop => "stop",
            Command::Lock => "lock",
            Command::Hide => "hide",
            Command::Quit => "quit",
            Command::SaveMiss => "save a miss",
            Command::Subtitles => "captions",
            Command::Palette => "macros",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Command::Paste => "paste",
            Command::Undo => "undo",
            Command::Stop => "stop",
            Command::Lock => "lock",
            Command::Hide => "hide",
            Command::Quit => "quit",
            Command::SaveMiss => "save_miss",
            Command::Subtitles => "captions",
            Command::Palette => "palette",
        }
    }

    fn default_key(self) -> char {
        match self {
            Command::Paste => 'V',
            Command::Undo => 'Z',
            Command::Stop => 'S',
            Command::Lock => 'C',
            Command::Hide => 'H',
            Command::Quit => 'Q',
            Command::SaveMiss => 'B',
            Command::Subtitles => 'T',
            Command::Palette => 'M',
        }
    }
}

/// A macro's shortcut (`hotkey = "Ctrl+Alt+M"`): its modifiers, then one other key. It holds Ctrl, Alt or
/// Win, or is F13 to F24 alone; never Win+L or Ctrl+Alt+Delete.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub keys: Vec<Key>,
}

impl Hotkey {
    /// Reads a shortcut written with the key names settings use, modifiers in any order; an error says why
    /// it can't be one.
    pub fn parse(text: &str) -> Result<Hotkey, String> {
        let keys = text.split('+').map(|name| Key::parse(name).ok_or_else(|| format!("{} isn't a key name", name.trim()))).collect::<Result<Vec<_>, _>>()?;
        let mut others = keys.iter().copied().filter(|key| !key.is_modifier());
        let (Some(key), None) = (others.next(), others.next()) else {
            return Err("a shortcut is its modifiers and one other key, like Ctrl+Alt+M".into());
        };
        let held: Vec<Key> = [Key::Ctrl, Key::Alt, Key::Shift, Key::Win].into_iter().filter(|modifier| keys.contains(modifier)).collect();
        let key_is_f13_up = matches!(key, Key::Other(vk) if (VK_F1 + 12..=VK_F24).contains(&vk));
        if !(held.iter().any(|modifier| *modifier != Key::Shift) || key_is_f13_up) {
            return Err("a shortcut needs Ctrl, Alt or Win (or is F13 to F24), so it doesn't get in the way of typing".into());
        }
        if held.contains(&Key::Win) && key == Key::Other(b'L'.into()) {
            return Err("Windows always locks the PC on Win+L".into());
        }
        if held.contains(&Key::Ctrl) && held.contains(&Key::Alt) && key == Key::Other(0x2E) {
            return Err("Windows keeps Ctrl+Alt+Delete for itself".into());
        }
        Ok(Hotkey { keys: held.into_iter().chain([key]).collect() })
    }

    /// How it is written, as [`Keybinds::shortcut`] writes a shortcut: "Ctrl+Alt+M".
    pub fn name(&self) -> String {
        self.keys.iter().map(|key| key.name()).collect::<Vec<_>>().join("+")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Keybinds {
    /// Held together to dictate, and to make the command keys work.
    pub hold: Vec<Key>,
    commands: Vec<(Command, Key)>,
}

impl Default for Keybinds {
    fn default() -> Self {
        let commands = Command::ALL.iter().map(|&c| (c, Key::Other(c.default_key() as u16))).collect();
        Self { hold: vec![Key::Ctrl, Key::Win], commands }
    }
}

impl Keybinds {
    pub fn key_for(&self, command: Command) -> Key {
        self.commands.iter().find(|(c, _)| *c == command).map_or(Key::Other(command.default_key() as u16), |(_, k)| *k)
    }

    pub fn command_for(&self, key: Key) -> Option<Command> {
        self.commands.iter().find(|(_, k)| *k == key).map(|(c, _)| *c)
    }

    /// E.g. "Ctrl+Win".
    pub fn hold_name(&self) -> String {
        self.hold.iter().map(|k| k.name()).collect::<Vec<_>>().join("+")
    }

    /// E.g. "Ctrl+Win+S".
    pub fn shortcut(&self, command: Command) -> String {
        format!("{}+{}", self.hold_name(), self.key_for(command).name())
    }

    /// Why `hotkey` can't be a macro's shortcut: it is the captions key, or it with a command's key.
    pub fn clash(&self, hotkey: &Hotkey) -> Option<String> {
        if let Some(key) = hotkey.keys.last().filter(|key| !key.is_modifier() && self.hold.contains(key)) {
            return Some(format!("{} is part of the captions key, {}", key.name(), self.hold_name()));
        }
        if !self.hold.iter().all(|key| hotkey.keys.contains(key)) {
            return None;
        }
        let mut rest = hotkey.keys.iter().filter(|key| !self.hold.contains(key));
        match (rest.next(), rest.next()) {
            (None, _) => Some(format!("{} is the captions key", hotkey.name())),
            (Some(&key), None) => self.command_for(key).map(|command| format!("{} is Kiku's own shortcut to {}", hotkey.name(), command.short())),
            _ => None,
        }
    }

    /// Makes `keys` the captions key, or says why it can't be.
    pub fn set_hold(&mut self, keys: Vec<Key>) -> Result<(), String> {
        if keys.is_empty() || keys.len() > 3 {
            return Err("Use one to three keys".into());
        }
        if keys.len() == 1 && keys[0].is_modifier() || keys.iter().any(|k| k.types_text()) {
            return Err("That would get in the way of typing; try Ctrl+Win, Caps Lock or F13".into());
        }
        let candidate = Self { hold: keys, commands: self.commands.clone() };
        candidate.check_commands()?;
        *self = candidate;
        Ok(())
    }

    /// Makes `key` run `command`, or says why it can't.
    pub fn set_command(&mut self, command: Command, key: Key) -> Result<(), String> {
        if key.is_modifier() {
            return Err("Pick a letter, digit or other non-modifier key".into());
        }
        let mut candidate = self.clone();
        for entry in candidate.commands.iter_mut().filter(|(c, _)| *c == command) {
            entry.1 = key;
        }
        candidate.check_commands()?;
        *self = candidate;
        Ok(())
    }

    fn check_commands(&self) -> Result<(), String> {
        for (index, &(command, key)) in self.commands.iter().enumerate() {
            if self.hold.contains(&key) {
                return Err(format!("{} is part of the captions key", key.name()));
            }
            if self.commands[..index].iter().any(|(_, k)| *k == key) {
                return Err(format!("{} already does something else", key.name()));
            }
            // Windows locks the PC on Win+L before any app sees it.
            if self.hold.contains(&Key::Win) && key == Key::Other('L' as u16) {
                return Err(format!("{} can't be Win+L: Windows always locks the PC on it", command.short()));
            }
        }
        Ok(())
    }

    /// Reads `hold = ...` and `key.<command> = ...` settings lines; anything invalid keeps the default.
    pub fn apply_setting(&mut self, key: &str, value: &str) {
        if key == "hold" {
            let keys: Option<Vec<Key>> = value.split('+').map(Key::parse).collect();
            if let Some(keys) = keys {
                let _ = self.set_hold(keys);
            }
        } else if let Some(command) = key.strip_prefix("key.").and_then(|id| Command::ALL.into_iter().find(|c| c.id() == id))
            && let Some(bound) = Key::parse(value)
        {
            let _ = self.set_command(command, bound);
        }
    }

    pub fn to_settings(&self) -> String {
        let mut text = format!("hold = {}\n", self.hold_name().to_lowercase());
        for command in Command::ALL {
            text.push_str(&format!("key.{} = {}\n", command.id(), self.key_for(command).name()));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_codes_read_as_modifiers() {
        assert_eq!(Key::parse("key 0x5b"), Some(Key::Win));
        assert_eq!(Key::parse("key 0x11"), Some(Key::Ctrl));
        assert_eq!(Key::parse("key 0xa0"), Some(Key::Shift));
        assert_eq!(Key::parse("key 0x12"), Some(Key::Alt));
    }

    #[test]
    fn settings_round_trip() {
        let mut binds = Keybinds::default();
        binds.set_hold(vec![Key::Other(VK_CAPITAL)]).unwrap();
        binds.set_command(Command::Paste, Key::Other(0x7C)).unwrap(); // F13
        let mut read = Keybinds::default();
        for (key, value) in binds.to_settings().lines().filter_map(|l| l.split_once('=')) {
            read.apply_setting(key.trim(), value.trim());
        }
        assert_eq!(read, binds);
        assert_eq!(read.shortcut(Command::Paste), "Caps Lock+F13");
    }

    #[test]
    fn shortcuts_that_would_break_typing_or_clash_are_refused() {
        let mut binds = Keybinds::default();
        assert!(binds.set_hold(vec![Key::Shift]).is_err(), "Shift alone");
        assert!(binds.set_hold(vec![Key::Other('A' as u16)]).is_err(), "a letter");
        assert!(binds.set_command(Command::Undo, Key::Other('V' as u16)).is_err(), "V already pastes");
        assert!(binds.set_command(Command::Undo, Key::Other('L' as u16)).is_err(), "Win+L locks the PC");
        assert!(binds.set_command(Command::Undo, Key::Ctrl).is_err(), "a modifier");
        assert_eq!(binds, Keybinds::default(), "refused changes leave the shortcuts alone");
    }

    #[test]
    fn a_shortcut_ending_in_a_key_of_the_captions_key_is_refused() {
        let mut binds = Keybinds::default();
        binds.set_hold(vec![Key::Other(0x7C)]).unwrap();
        let clash = binds.clash(&Hotkey::parse("Ctrl+F13").unwrap()).expect("F13 is the captions key");
        assert!(clash.contains("captions key"), "{clash}");
        assert_eq!(binds.clash(&Hotkey::parse("Ctrl+F14").unwrap()), None);
    }
}
