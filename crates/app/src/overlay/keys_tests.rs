//! The keyboard hook while Kiku sends keys with modifiers held: it lets them go (`LIFT`), sends, and presses
//! them again (`RESTORE`), and the user's keys land in between. The traces are the 0.7.2 report's, where a held
//! Win's auto-repeat landed inside a paste.

use windows::Win32::UI::WindowsAndMessaging::{KBDLLHOOKSTRUCT, KBDLLHOOKSTRUCT_FLAGS, LLKHF_UP};

use kiku_core::keybinds::{Command, Key, Keybinds};

use super::keys::{self, KeyEvent, Outcome, LIFT, OWN_KEYS, RESTORE};

const LCTRL: u16 = 0xA2;
const RCTRL: u16 = 0xA3;
const LWIN: u16 = 0x5B;
const LSHIFT: u16 = 0xA0;
const INSERT: u16 = 0x2D;
const MASK: u16 = 0xE8;
const S: u16 = 0x53;
const V: u16 = 0x56;

/// One key event: typed by the user, or sent by Kiku as an ordinary key, a lift or a press again.
#[derive(Clone, Copy)]
enum Ev {
    Typed(u16, bool),
    Kiku(u16, bool),
    Lift(u16),
    Restore(u16),
}

use Ev::{Kiku, Lift, Restore, Typed};

/// Kiku's Shift+Insert, between its lifts and its presses again.
const PASTE: [Ev; 4] = [Kiku(LSHIFT, true), Kiku(INSERT, true), Kiku(INSERT, false), Kiku(LSHIFT, false)];

/// The hook with the default captions key, Ctrl+Win, and the keys Windows has down: those the hook let pass.
struct Keyboard {
    hook: keys::Context,
    windows: Vec<u16>,
}

impl Keyboard {
    fn new() -> Keyboard {
        Keyboard { hook: keys::Context::new(Keybinds::default()), windows: Vec::new() }
    }

    fn run(&mut self, events: &[Ev]) -> Vec<Outcome> {
        events.iter().map(|&event| self.event(event)).collect()
    }

    fn event(&mut self, event: Ev) -> Outcome {
        let (vk, down, tag) = match event {
            Typed(vk, down) => (vk, down, 0),
            Kiku(vk, down) => (vk, down, OWN_KEYS),
            Lift(vk) => (vk, false, LIFT),
            Restore(vk) => (vk, true, RESTORE),
        };
        let flags = if down { KBDLLHOOKSTRUCT_FLAGS(0) } else { LLKHF_UP };
        let windows = self.windows.clone();
        let windows_has_down = move |key: Key| windows.iter().any(|&vk| Key::from_vk(vk) == key);
        let key = KBDLLHOOKSTRUCT { vkCode: vk.into(), flags, dwExtraInfo: tag, ..Default::default() };
        let outcome = keys::handle(&mut self.hook, &key, &windows_has_down);
        if !outcome.swallow {
            self.windows.retain(|&k| k != vk);
            if down {
                self.windows.push(vk);
            }
        }
        outcome
    }
}

impl Keyboard {
    fn holding(&self) -> bool {
        let windows = &self.windows;
        self.hook.holding(&|key: Key| windows.iter().any(|&vk| Key::from_vk(vk) == key))
    }
}

fn events(outcomes: &[Outcome]) -> Vec<KeyEvent> {
    outcomes.iter().flat_map(|outcome| outcome.events.clone()).collect()
}

/// Kiku's paste with Ctrl+Win held: the mask key, its lifts, `between` (what the user's keyboard sends meanwhile)
/// and the paste, then its presses again; the outcomes of those two.
fn paste(keyboard: &mut Keyboard, between: &[Ev]) -> (Vec<Outcome>, [Outcome; 2]) {
    let mut sent = keyboard.run(&[Kiku(MASK, true), Kiku(MASK, false), Lift(LCTRL)]);
    sent.extend(keyboard.run(between));
    sent.extend(keyboard.run(&PASTE));
    (sent, [keyboard.event(Restore(LCTRL)), keyboard.event(Restore(LWIN))])
}

fn hold_ctrl_win(keyboard: &mut Keyboard) {
    assert_eq!(events(&keyboard.run(&[Typed(LCTRL, true), Typed(LWIN, true), Typed(LWIN, true)])), [KeyEvent::Held]);
}

/// Lets go of Win, then Ctrl: the hold ends at the first, and the last modifier up lets typing go on.
fn let_go(keyboard: &mut Keyboard) {
    assert_eq!(events(&[keyboard.event(Typed(LWIN, false))]), [KeyEvent::Released], "the hold ends when Win goes up");
    assert_eq!(events(&[keyboard.event(Typed(LCTRL, false))]), [KeyEvent::LetGo], "and nothing is held once Ctrl goes up");
    assert!(keyboard.windows.is_empty(), "Windows has nothing left down: {:?}", keyboard.windows);
}

#[test]
fn a_repeat_after_both_lifts_keeps_the_hold() {
    // 15:54:14: Win's auto-repeat lands after Kiku's Win-up.
    let mut keyboard = Keyboard::new();
    hold_ctrl_win(&mut keyboard);
    let (sent, pressed_again) = paste(&mut keyboard, &[Lift(LWIN), Typed(LWIN, true)]);
    assert_eq!(events(&sent), [], "Kiku's own lift isn't the user letting go");
    assert!(pressed_again.iter().all(|outcome| !outcome.swallow), "both still held, so both pressed again: {pressed_again:?}");
    let_go(&mut keyboard);
}

#[test]
fn a_repeat_between_the_lifts_keeps_the_hold() {
    // 15:54:09: the repeat lands between the Ctrl-up and the Win-up.
    let mut keyboard = Keyboard::new();
    hold_ctrl_win(&mut keyboard);
    let (sent, pressed_again) = paste(&mut keyboard, &[Typed(LWIN, true), Lift(LWIN)]);
    assert_eq!(events(&sent), []);
    assert!(pressed_again.iter().all(|outcome| !outcome.swallow), "{pressed_again:?}");
    let_go(&mut keyboard);
}

#[test]
fn a_key_let_go_during_a_lift_isnt_pressed_again() {
    let mut keyboard = Keyboard::new();
    hold_ctrl_win(&mut keyboard);
    let (sent, pressed_again) = paste(&mut keyboard, &[Lift(LWIN), Typed(LWIN, false), Typed(LCTRL, false)]);
    assert_eq!(events(&sent), [KeyEvent::Released, KeyEvent::LetGo]);
    assert!(pressed_again.iter().all(|outcome| outcome.swallow), "let go, so not pressed again: {pressed_again:?}");
    assert!(keyboard.windows.is_empty(), "no key left stuck down: {:?}", keyboard.windows);
}

#[test]
fn a_lift_just_after_the_last_key_up_leaves_nothing_down() {
    // Kiku types on LetGo, posted before Windows takes the key-up in, so it may still read Ctrl as down and lift it.
    let mut keyboard = Keyboard::new();
    hold_ctrl_win(&mut keyboard);
    keyboard.run(&[Typed(LWIN, false), Typed(LCTRL, false)]);
    keyboard.windows.push(LCTRL);
    keyboard.event(Lift(LCTRL));
    keyboard.run(&PASTE);
    assert!(keyboard.event(Restore(LCTRL)).swallow, "Ctrl is up, so it isn't pressed again");
    assert!(keyboard.windows.is_empty(), "{:?}", keyboard.windows);
}

#[test]
fn a_command_keys_repeat_inside_its_own_paste_stays_a_command() {
    // Ctrl+Win+V pastes with Ctrl+Win held, and holding V repeats it during that paste.
    let mut keyboard = Keyboard::new();
    hold_ctrl_win(&mut keyboard);
    let v = keyboard.event(Typed(V, true));
    assert!(v.swallow && v.events == [KeyEvent::Command(Command::Paste)], "{v:?}");
    let (sent, pressed_again) = paste(&mut keyboard, &[Lift(LWIN), Typed(V, true)]);
    let repeat = &sent[4];
    assert!(repeat.swallow && repeat.events == [KeyEvent::Command(Command::Paste)], "V is still Ctrl+Win+V, not typed: {repeat:?}");
    assert!(pressed_again.iter().all(|outcome| !outcome.swallow), "{pressed_again:?}");
    assert!(keyboard.event(Typed(V, false)).swallow);
    let_go(&mut keyboard);
}

#[test]
fn typing_waits_only_while_the_captions_key_is_held() {
    let mut keyboard = Keyboard::new();
    keyboard.event(Typed(LCTRL, true));
    assert!(!keyboard.holding(), "Ctrl on its own holds nothing back");
    assert_eq!(events(&keyboard.run(&[Typed(0x43, true), Typed(0x43, false), Typed(LCTRL, false)])), [], "nor lets anything go");
    hold_ctrl_win(&mut keyboard);
    assert!(keyboard.holding());
    keyboard.event(Typed(LWIN, false));
    assert!(keyboard.holding(), "Ctrl may still be repeating once Win is up");
    assert_eq!(events(&[keyboard.event(Typed(LCTRL, false))]), [KeyEvent::LetGo]);
    assert!(!keyboard.holding());

    hold_ctrl_win(&mut keyboard);
    keyboard.windows.clear();
    assert!(!keyboard.holding(), "key-ups the hook missed hold nothing back");
}

#[test]
fn key_ups_missed_on_the_secure_desktop_end_the_hold() {
    let mut keyboard = Keyboard::new();
    hold_ctrl_win(&mut keyboard);
    keyboard.windows.clear();
    let s = keyboard.event(Typed(S, true));
    assert!(!s.swallow, "S is typed, not Ctrl+Win+S: {s:?}");
    assert_eq!(s.events, [KeyEvent::Released, KeyEvent::LetGo]);
}

#[test]
fn a_held_key_the_hook_lost_track_of_is_pressed_again() {
    // Both Ctrls down and one let go: the hook keeps Ctrl by name only, so the other is still held.
    let mut keyboard = Keyboard::new();
    keyboard.run(&[Typed(LCTRL, true), Typed(RCTRL, true), Typed(RCTRL, false)]);
    keyboard.event(Lift(LCTRL));
    keyboard.run(&PASTE);
    assert!(!keyboard.event(Restore(LCTRL)).swallow, "LCtrl is still held");

    // A modifier held since before the hook saw it (Kiku started, or its keys were reset, with it down).
    let mut keyboard = Keyboard::new();
    keyboard.windows.push(LSHIFT);
    keyboard.event(Lift(LSHIFT));
    assert!(!keyboard.event(Restore(LSHIFT)).swallow, "never seen going up, so pressed again as before");
    assert_eq!(keyboard.windows, [LSHIFT]);
}
