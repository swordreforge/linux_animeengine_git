//! Rendering an animation with ffmpeg, from a command the user can read and change.
//!
//! The Python app hard-codes the encoder's arguments in three separate tools — `-framerate 60` appears in
//! all of them, and one passes `-qscale` to libwebp, which is not a libwebp option and is therefore
//! ignored. Both problems are the same problem: the numbers live in the source, so nothing can be adjusted
//! without a rebuild and a mistake is invisible.
//!
//! Here the whole argument list is a **template**: `{fps}`, `{quality}`, `{frames}`, `{output}` and the
//! rest are filled in at the moment it runs, and the template itself is editable in the window. If a
//! future ffmpeg renames an option — `libwebp_anim` has been through that once already — the fix is a line
//! in a text field, not a release.

/// Encoders we offer, each with the arguments that actually do what they say.
///
/// `libwebp_anim` is the animated-WebP encoder; `-quality` and `-lossless` are its options, which is why
/// there is no `-qscale` here. `-plays 0` on APNG and `-loop 0` elsewhere mean "loop forever".
pub const PRESETS: &[(&str, &str)] = &[
    (
        "gif",
        "ffmpeg -y -framerate {fps} -i {frames} -vf \"split[a][b];[a]palettegen[p];[b][p]paletteuse\" -loop 0 {output}",
    ),
    (
        "apng",
        "ffmpeg -y -framerate {fps} -i {frames} -plays 0 {output}",
    ),
    (
        "webp",
        "ffmpeg -y -framerate {fps} -i {frames} -c:v libwebp_anim -lossless {lossless} -quality {quality} -loop 0 {output}",
    ),
];

/// The variables every template may use, with what they mean. Shown next to the field, because a template
/// whose vocabulary is a secret is not editable in any useful sense.
pub const VARIABLES: &[(&str, &str)] = &[
    ("{frames}", "the frame pattern, e.g. /tmp/x/frame_%04d.png"),
    ("{output}", "the file to write"),
    ("{fps}", "frames per second"),
    ("{quality}", "0-100, encoders that take one"),
    ("{lossless}", "1 or 0"),
];

/// Replace every `{name}` in one pass. Unknown names are left alone rather than blanked: a template that
/// refers to something this version does not fill in should fail visibly when ffmpeg sees it, not quietly
/// turn into an argument-less command.
pub fn expand(template: &str, variables: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match variables
                    .iter()
                    .find(|(key, _)| *key == format!("{{{name}}}"))
                {
                    Some((_, value)) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Split a command line into arguments, honouring double quotes — `-vf "split[a][b];…"` is one argument,
/// and getting that wrong is how a filter graph ends up as seven.
pub fn split_command(command: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for ch in command.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

/// What running a template produced, for the status line.
pub struct Rendered {
    pub ok: bool,
    pub detail: String,
}

/// Run one expanded template. The first argument is the program; the rest are passed through untouched,
/// with no shell in between, so a path with a space in it is a path with a space in it.
pub fn run(command: &str) -> Rendered {
    let mut args = split_command(command);
    if args.is_empty() {
        return Rendered {
            ok: false,
            detail: "the command is empty".to_string(),
        };
    }
    let program = args.remove(0);
    match std::process::Command::new(&program).args(&args).output() {
        Ok(output) if output.status.success() => Rendered {
            ok: true,
            detail: String::new(),
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // The last non-empty line, not the first: ffmpeg's final line is the one that says what went
            // wrong, and everything above it is progress output.
            let tail = stderr
                .lines()
                .rfind(|line| !line.trim().is_empty())
                .unwrap_or("ffmpeg failed without saying why")
                .to_string();
            Rendered {
                ok: false,
                detail: tail,
            }
        }
        Err(error) => Rendered {
            ok: false,
            detail: format!("could not run {program}: {error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_are_replaced_wherever_they_appear() {
        let out = expand(
            "ffmpeg -framerate {fps} -i {frames} {output} -r {fps}",
            &[
                ("{fps}", "30"),
                ("{frames}", "f_%04d.png"),
                ("{output}", "out.gif"),
            ],
        );
        assert_eq!(out, "ffmpeg -framerate 30 -i f_%04d.png out.gif -r 30");
    }

    #[test]
    fn an_unknown_variable_is_left_visible() {
        // Blanking it would hand ffmpeg a subtly different command; leaving it makes the mistake loud.
        let out = expand("ffmpeg {nope} {fps}", &[("{fps}", "30")]);
        assert_eq!(out, "ffmpeg {nope} 30");
    }

    #[test]
    fn an_unterminated_brace_does_not_eat_the_rest() {
        let out = expand("ffmpeg -i {frames {output}", &[("{output}", "o.gif")]);
        assert_eq!(out, "ffmpeg -i {frames {output}");
    }

    #[test]
    fn a_quoted_filter_graph_stays_one_argument() {
        let args = split_command("ffmpeg -i in -vf \"split[a][b];[a]p;[b]q\" -loop 0 out.gif");
        assert_eq!(args.len(), 8);
        assert_eq!(args[4], "split[a][b];[a]p;[b]q");
    }

    #[test]
    fn the_shipped_presets_either_expand_or_are_plainly_broken() {
        for (name, template) in PRESETS {
            let expanded = expand(
                template,
                &[
                    ("{frames}", "/t/frame_%04d.png"),
                    ("{output}", "/t/out"),
                    ("{fps}", "24"),
                    ("{quality}", "90"),
                    ("{lossless}", "1"),
                ],
            );
            assert!(
                !expanded.contains('{'),
                "{name} left a variable unexpanded: {expanded}"
            );
            // The one option the Python tools got wrong, pinned so it cannot come back.
            if *name == "webp" {
                assert!(
                    !expanded.contains("-qscale"),
                    "libwebp has no -qscale; it would be ignored"
                );
                assert!(expanded.contains("-quality"));
            }
        }
    }

    #[test]
    fn an_empty_command_is_an_error_and_not_a_panic() {
        let rendered = run("   ");
        assert!(!rendered.ok);
    }

    #[test]
    fn a_command_that_does_not_exist_reports_which_one() {
        let rendered = run("definitely-not-a-real-program-xyz --version");
        assert!(!rendered.ok);
        assert!(
            rendered
                .detail
                .contains("definitely-not-a-real-program-xyz")
        );
    }
}
