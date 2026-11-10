//! Voice actions' Windows tests (`.loop/voice-actions/SPINE.md`), run by the `voice-ci` workflow: the key
//! presses a command sends. What lands in another app can't be checked headless; that is the hand check.

use kiku_core::{actions::key_strokes, keybinds::Key};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_KEYBOARD, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP};

use super::keys;

/// Each input as (virtual key, up, extended).
fn sent(inputs: &[INPUT]) -> Vec<(u16, bool, bool)> {
    inputs
        .iter()
        .map(|input| {
            assert_eq!(input.r#type, INPUT_KEYBOARD);
            let key = unsafe { input.Anonymous.ki };
            (key.wVk.0, key.dwFlags.contains(KEYEVENTF_KEYUP), key.dwFlags.contains(KEYEVENTF_EXTENDEDKEY))
        })
        .collect()
}

#[test]
fn v5_a_shortcut_is_sent_as_its_keys_in_order() {
    let inputs = keys::inputs(&key_strokes(&[Key::Ctrl, Key::Shift, Key::Other('T' as u16)]));
    let expected = [(0xA2, false, false), (0xA0, false, false), (0x54, false, false), (0x54, true, false), (0xA0, true, false), (0xA2, true, false)];
    assert_eq!(sent(&inputs), expected);
}

#[test]
fn v5_arrows_are_sent_as_extended_keys() {
    let inputs = keys::inputs(&key_strokes(&[Key::Ctrl, Key::Other(0x25)]));
    assert_eq!(sent(&inputs), [(0xA2, false, false), (0x25, false, true), (0x25, true, true), (0xA2, true, false)]);
}
