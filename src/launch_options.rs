use std::ffi::OsString;
use std::path::PathBuf;

#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) const HELP: &str = "Usage: kokuban [OPTIONS] [-e COMMAND [ARGUMENTS...]]

Options:
  -e, --execute COMMAND...    Run a command directly in the terminal
  --working-directory DIR    Start in this directory (alias: --dir)
  --app-id ID                 Set Wayland app ID / X11 window class
  --title TITLE               Set the initial window title
  -h, --help                  Print this help
  -V, --version               Print the version

Use -- to end option parsing and run a command. Arguments following -e or --
are passed unchanged; use sh -c explicitly when shell expansion is required.";

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LaunchOptions {
    pub(crate) command: Vec<OsString>,
    pub(crate) working_directory: Option<PathBuf>,
    pub(crate) app_id: Option<String>,
    pub(crate) title: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LaunchAction {
    Run(LaunchOptions),
    Help,
    Version,
}

pub(crate) fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<LaunchAction, String> {
    let mut arguments = arguments.into_iter();
    let mut options = LaunchOptions::default();
    while let Some(argument) = arguments.next() {
        if argument == "-e" || argument == "--execute" || argument == "--" {
            options.command = arguments.collect();
            if options
                .command
                .first()
                .is_none_or(|program| program.is_empty())
            {
                return Err("expected a command after -e, --execute or --".to_string());
            }
            break;
        }
        if argument == "-h" || argument == "--help" {
            return Ok(LaunchAction::Help);
        }
        if argument == "-V" || argument == "--version" {
            return Ok(LaunchAction::Version);
        }

        // Split Unix OS strings as bytes so paths do not need to be valid UTF-8.
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let bytes = argument.as_bytes();
        let (name, inline_value) = match bytes.iter().position(|byte| *byte == b'=') {
            Some(index) => (
                &bytes[..index],
                Some(OsString::from_vec(bytes[index + 1..].to_vec())),
            ),
            None => (bytes, None),
        };
        if !matches!(
            name,
            b"--working-directory" | b"--dir" | b"--app-id" | b"--title"
        ) {
            return Err(format!(
                "unknown option {:?}; use -e to run a command",
                argument
            ));
        }
        let value = inline_value
            .or_else(|| arguments.next())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("expected a value for {}", String::from_utf8_lossy(name)))?;
        match name {
            b"--working-directory" | b"--dir" => options.working_directory = Some(value.into()),
            _ => {
                let value = value.into_string().map_err(|_| {
                    format!("{} requires UTF-8 text", String::from_utf8_lossy(name))
                })?;
                if value.chars().any(char::is_control) {
                    return Err(format!(
                        "{} must not contain control characters",
                        String::from_utf8_lossy(name)
                    ));
                }
                if name == b"--app-id" {
                    options.app_id = Some(value);
                } else {
                    options.title = Some(value);
                }
            }
        }
    }
    Ok(LaunchAction::Run(options))
}

#[cfg(test)]
mod tests {
    use super::{parse, LaunchAction, LaunchOptions};
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    fn run(arguments: &[&str]) -> LaunchOptions {
        match parse(arguments.iter().map(OsString::from)).unwrap() {
            LaunchAction::Run(options) => options,
            action => panic!("expected launch, got {action:?}"),
        }
    }

    #[test]
    fn supports_omarchy_tui_launch_and_preserves_arguments() {
        let options = run(&[
            "--app-id=org.omarchy.test",
            "--dir",
            "/tmp/a b",
            "-e",
            "sh",
            "-c",
            "printf '%s' \"$1\"",
            "sh",
            "a b;$(false)",
            "--title=child",
        ]);
        assert_eq!(options.app_id.as_deref(), Some("org.omarchy.test"));
        assert_eq!(options.working_directory, Some("/tmp/a b".into()));
        assert_eq!(
            options.command,
            [
                "sh",
                "-c",
                "printf '%s' \"$1\"",
                "sh",
                "a b;$(false)",
                "--title=child"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn supports_separate_and_inline_window_options() {
        let options = run(&[
            "--working-directory=/tmp",
            "--title",
            "Terminal title",
            "--app-id",
            "org.omarchy.test",
        ]);
        assert_eq!(options.working_directory, Some("/tmp".into()));
        assert_eq!(options.title.as_deref(), Some("Terminal title"));
        assert_eq!(options.app_id.as_deref(), Some("org.omarchy.test"));
    }

    #[test]
    fn preserves_non_utf8_paths_and_command_arguments() {
        let path = OsString::from_vec(b"/tmp/\xff".to_vec());
        let argument = OsString::from_vec(b"\xfe".to_vec());
        let parsed = parse([
            "--dir".into(),
            path.clone(),
            "--".into(),
            "printf".into(),
            argument.clone(),
        ])
        .unwrap();
        let LaunchAction::Run(options) = parsed else {
            panic!("expected launch")
        };
        assert_eq!(options.working_directory, Some(path.into()));
        assert_eq!(options.command, vec![OsString::from("printf"), argument]);
    }

    #[test]
    fn rejects_invalid_launches_instead_of_silently_opening_a_shell() {
        for arguments in [
            vec!["-e"],
            vec!["--"],
            vec!["--execute", ""],
            vec!["--dir"],
            vec!["--app-id="],
            vec!["--unknown"],
            vec!["--title=a\nb"],
        ] {
            assert!(parse(arguments.into_iter().map(OsString::from)).is_err());
        }
    }

    #[test]
    fn defaults_to_login_shell_and_prints_help_without_launching() {
        assert_eq!(run(&[]), LaunchOptions::default());
        assert_eq!(parse(["--help".into()]).unwrap(), LaunchAction::Help);
        assert_eq!(parse(["--version".into()]).unwrap(), LaunchAction::Version);
        assert_eq!(
            run(&["--execute", "printf", "--help"]).command,
            ["printf", "--help"].map(OsString::from)
        );
    }
}
