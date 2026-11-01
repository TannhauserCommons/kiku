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
            Key::Other(VK_CAPITAL) => "Caps Lock".into(),
            Key::Other(vk @ VK_F1..=VK_F24) => format!("F{}", vk - VK_F1 + 1),
            Key::Other(vk @ (0x30..=0x39 | 0x41..=0x5A)) => (vk as u8 as char).to_string(),
            Key::Other(vk) => format!("Key {vk:#04x}"),
        }
    }

    fn parse(name: &str) -> Option<Self> {
        let name = name.trim();
        Some(match name.to_lowercase().as_str() {
            "ctrl" => Key::Ctrl,
            "win" => Key::Win,
            "alt" => Key::Alt,
            "shift" => Key::Shift,
            "caps lock" => Key::Other(VK_CAPITAL),
            lower => {
                if let Some(number) = lower.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()).filter(|n| (1..=24).contains(n)) {
                    Key::Other(VK_F1 + number - 1)
                } else if let Some(code) = lower.strip_prefix("key 0x") {
                    Key::Other(u16::from_str_radix(code, 16).ok()?)
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
}

impl Command {
    pub const ALL: [Command; 8] =
        [Command::Paste, Command::Undo, Command::Stop, Command::Lock, Command::Hide, Command::Quit, Command::SaveMiss, Command::Subtitles];

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
        }
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
        } else if let Some(command) = key.strip_prefix("key.").and_then(|id| Command::ALL.into_iter().find(|c| c.id() == id)) {
            if let Some(bound) = Key::parse(value) {
                let _ = self.set_command(command, bound);
            }
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
}
