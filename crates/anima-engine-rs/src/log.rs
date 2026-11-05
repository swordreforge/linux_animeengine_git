//! One line per event, with the fields named rather than interpolated into a sentence.
//!
//! `M-LOG-NOT-PRINT` asks production code to emit diagnostics through the project's own facility instead of
//! `println!`, and `M-LOG-STRUCTURED` asks for named fields rather than prose. This is that, without a
//! dependency: an event name, `key=value` pairs, one line on stderr, and a level that can be turned down.
//!
//! ```text
//! ANIMA_LOG=off     nothing at all
//! ANIMA_LOG=info    state changes and failures — the default
//! ANIMA_LOG=debug   also the per-frame and per-region detail
//! ```
//!
//! Values are formatted only when the level is on, so a debug event on a hot path costs a comparison and a
//! branch. `stderr` is the sink: the point of the rule is that call sites do not choose to print, not that the
//! output has nowhere to go. CLI binaries whose stdout *is* their interface keep using `println!`.

use std::fmt::Display;
use std::sync::OnceLock;

/// How much detail an event carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// A state change or a failure: things a user would want to see the one time they happen.
    Info,
    /// Detail that repeats: per-frame, per-region, per-reload.
    Debug,
}

/// Reads the level out of an environment value, kept separate from the environment so it can be tested.
fn level_from(value: Option<&str>) -> Option<Level> {
    match value {
        Some("off") | Some("0") | Some("false") => None,
        Some("debug") | Some("trace") => Some(Level::Debug),
        Some(_) => Some(Level::Info),
        None => Some(Level::Info),
    }
}

/// The level from the environment, or `None` when the log is off.
fn threshold() -> Option<Level> {
    static LEVEL: OnceLock<Option<Level>> = OnceLock::new();
    *LEVEL.get_or_init(|| level_from(std::env::var("ANIMA_LOG").ok().as_deref()))
}

/// Whether an event at this level would reach stderr.
pub fn enabled(level: Level) -> bool {
    match threshold() {
        Some(limit) => level <= limit,
        None => false,
    }
}

/// Renders one line: the event name, then `key=value`, quoting values that would otherwise break the shape.
fn line(name: &str, fields: &[(&str, &dyn Display)]) -> String {
    let mut out = String::from(name);
    for (key, value) in fields {
        out.push(' ');
        out.push_str(key);
        out.push('=');
        let rendered = value.to_string();
        if rendered.is_empty() || rendered.contains([' ', '"', '=']) {
            out.push('"');
            for character in rendered.chars() {
                if character == '"' || character == '\\' {
                    out.push('\\');
                }
                out.push(character);
            }
            out.push('"');
        } else {
            out.push_str(&rendered);
        }
    }
    out
}

/// Emits one event, unless the environment has turned this level off.
pub fn event(level: Level, name: &str, fields: &[(&str, &dyn Display)]) {
    if !enabled(level) {
        return;
    }
    eprintln!("{}", line(name, fields));
}

/// Emits a failure at [`Level::Info`] — failures are what a user runs the program from a terminal to see.
pub fn failure(name: &str, error: &dyn Display) {
    event(Level::Info, name, &[("error", error)]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_event_is_its_name() {
        assert_eq!(line("overlay.menu.closed", &[]), "overlay.menu.closed");
    }

    #[test]
    fn fields_keep_their_names() {
        assert_eq!(
            line(
                "overlay.frames",
                &[("frames", &12), ("width", &480), ("height", &270)]
            ),
            "overlay.frames frames=12 width=480 height=270"
        );
    }

    #[test]
    fn values_with_spaces_are_quoted() {
        assert_eq!(
            line("overlay.gpu", &[("adapter", &"Intel Arc Graphics")]),
            "overlay.gpu adapter=\"Intel Arc Graphics\""
        );
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        assert_eq!(
            line("probe", &[("value", &"a \"b\" \\ c")]),
            "probe value=\"a \\\"b\\\" \\\\ c\""
        );
    }

    #[test]
    fn an_empty_value_is_quoted_rather_than_missing() {
        assert_eq!(line("probe", &[("value", &"")]), "probe value=\"\"");
    }

    #[test]
    fn a_value_containing_equals_is_quoted() {
        assert_eq!(line("probe", &[("value", &"a=b")]), "probe value=\"a=b\"");
    }

    #[test]
    fn the_environment_decides_the_level() {
        assert_eq!(level_from(None), Some(Level::Info));
        assert_eq!(level_from(Some("")), Some(Level::Info));
        assert_eq!(level_from(Some("info")), Some(Level::Info));
        assert_eq!(level_from(Some("debug")), Some(Level::Debug));
        assert_eq!(level_from(Some("off")), None);
        assert_eq!(level_from(Some("0")), None);
        // Anything unrecognised is treated as the default rather than as silence: a typo should not hide
        // the failure messages a user set the variable to see.
        assert_eq!(level_from(Some("loud")), Some(Level::Info));
    }
}
