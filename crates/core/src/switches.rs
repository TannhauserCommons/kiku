//! Feature switches: unfinished features that ship dark. The list is `switches.toml` at the
//! repository root, compiled in.
//!
//! A switch that is off behaves as if its feature did not exist, so a name that is not in the list
//! is off. In release builds a switch is on only when the list says `default = "on"`. In debug
//! builds the environment variable `KIKU_FLAGS` (comma-separated names) also turns named switches
//! on, so a feature can be tried before its default changes; names not in the list are ignored. If
//! the embedded list is malformed, every switch is off (a test keeps that from shipping).

use std::{collections::HashMap, sync::OnceLock};

/// The list in `switches.toml`, compiled in.
const LIST: &str = include_str!("../../../switches.toml");

/// Whether the feature switch `name` is on. Debug builds also assert that `name` is in the list, so a
/// typo is caught by the tests instead of reading as off.
pub fn enabled(name: &str) -> bool {
    match state().get(name) {
        Some(on) => on,
        None => {
            debug_assert!(false, "unknown feature switch `{name}`: add it to switches.toml");
            false
        }
    }
}

/// The list with `KIKU_FLAGS` applied, read once like the list.
fn state() -> &'static State {
    static STATE: OnceLock<State> = OnceLock::new();
    STATE.get_or_init(|| State::resolve(LIST, &std::env::var("KIKU_FLAGS").unwrap_or_default(), cfg!(debug_assertions)))
}

/// Each switch's name and whether it is on.
struct State(HashMap<String, bool>);

impl State {
    /// The switches in `list` with their defaults, and the names in `flags` turned on when `debug`.
    /// Takes its inputs as arguments so tests need neither the environment nor a release build.
    fn resolve(list: &str, flags: &str, debug: bool) -> State {
        let mut switches = parse(list).unwrap_or_default();
        if debug {
            for name in flags.split(',').map(str::trim) {
                if let Some(on) = switches.get_mut(name) {
                    *on = true;
                }
            }
        }
        State(switches)
    }

    fn get(&self, name: &str) -> Option<bool> {
        self.0.get(name).copied()
    }
}

/// Every switch's name with whether its default is on; `None` if the text isn't a well-formed list.
fn parse(list: &str) -> Option<HashMap<String, bool>> {
    let table: toml::Table = list.parse().ok()?;
    let Some(entries) = table.get("switch") else {
        return Some(HashMap::new());
    };
    let mut switches = HashMap::new();
    for entry in entries.as_array()? {
        let name = entry.get("name")?.as_str()?;
        let on = match entry.get("default")?.as_str()? {
            "on" => true,
            "off" => false,
            _ => return None,
        };
        switches.insert(name.to_owned(), on);
    }
    Some(switches)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
        [[switch]]
        name = "calm"
        default = "off"

        [[switch]]
        name = "loud"
        default = "on"
    "#;

    fn resolve(flags: &str, debug: bool) -> State {
        State::resolve(EXAMPLE, flags, debug)
    }

    #[test]
    fn the_compiled_in_list_is_well_formed() {
        assert!(parse(LIST).is_some(), "switches.toml does not parse as a switch list");
        let table: toml::Table = LIST.parse().unwrap();
        for entry in table.get("switch").and_then(|s| s.as_array()).into_iter().flatten() {
            for key in ["name", "what", "owner", "area", "default", "ready", "expires"] {
                assert!(entry.get(key).is_some(), "a switch in switches.toml has no `{key}`: {entry}");
            }
        }
    }

    #[test]
    fn defaults_come_from_the_list() {
        let state = resolve("", true);
        assert_eq!(state.get("loud"), Some(true));
        assert_eq!(state.get("calm"), Some(false));
    }

    #[test]
    fn a_name_not_in_the_list_is_unknown() {
        assert_eq!(resolve("", true).get("nothing"), None);
        assert_eq!(State::resolve("", "nothing", true).get("nothing"), None);
    }

    #[test]
    fn flags_turn_an_off_switch_on_in_debug_builds_only() {
        assert_eq!(resolve("calm", true).get("calm"), Some(true));
        assert_eq!(resolve("calm", false).get("calm"), Some(false));
    }

    #[test]
    fn flags_ignore_unknown_names_whitespace_and_empty_items() {
        let state = resolve(" nothing, ,,calm ,", true);
        assert_eq!(state.get("calm"), Some(true));
        assert_eq!(state.get("loud"), Some(true));
        assert_eq!(state.get("nothing"), None);
    }

    #[test]
    fn a_malformed_list_turns_everything_off() {
        for list in [
            "[[switch]\nname = \"x\"",
            "switch = 3",
            "[[switch]]\ndefault = \"on\"",
            "[[switch]]\nname = \"x\"",
            "[[switch]]\nname = \"x\"\ndefault = \"maybe\"",
            // One bad entry spoils the list, even beside a good one that is on.
            "[[switch]]\nname = \"good\"\ndefault = \"on\"\n[[switch]]\nname = \"bad\"\ndefault = 1",
        ] {
            let state = State::resolve(list, "x,good,bad", true);
            assert!(state.0.is_empty(), "{list:?} should read as no switches");
        }
    }

    #[test]
    fn a_list_with_no_switches_is_fine() {
        assert_eq!(parse("cap_per_area = 2\ntombstones = []"), Some(HashMap::new()));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "unknown feature switch")]
    fn debug_builds_assert_on_an_unknown_name() {
        enabled("no-such-switch");
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn release_builds_read_an_unknown_name_as_off() {
        assert!(!enabled("no-such-switch"));
    }
}
