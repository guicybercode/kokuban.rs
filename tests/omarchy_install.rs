//! scripts/omarchy-install.sh against throwaway HOME directories.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const DESKTOP_ID: &str = "io.github.guicybercode.kokuban.desktop";
const LUA_RULE: &str = r#"o.window("io\\.github\\.guicybercode\\.kokuban", { tag = "+terminal" })"#;
const CONF_RULE: &str =
    "windowrule = tag +terminal, match:class ^(io[.]github[.]guicybercode[.]kokuban)$";
// What omarchy-default-terminal writes when Ghostty is chosen from the menu.
const OMARCHY_GHOSTTY_LIST: &str = "# Terminal emulator preference order for xdg-terminal-exec\n\
    # The first found and valid terminal will be used\n\
    com.mitchellh.ghostty.desktop\n";
const USER_LUA: &str = "dofile(\"/usr/share/omarchy/default/hypr/bootstrap.lua\")\n\
    require(\"default.hypr.omarchy\")\n\
    require(\"hypr.looknfeel\")\n\
    \n\
    -- Add any other personal Hyprland configuration below.\n\
    -- o.window(\"qemu\", { workspace = \"5\" })\n";

struct Home {
    root: PathBuf,
}

impl Home {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "kokuban-omarchy-install-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("home")).unwrap();
        let binary = root.join("fake-kokuban");
        fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.home().join(relative)
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path(relative)).unwrap()
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.run_in_desktop(arguments, Some("Hyprland"))
    }

    fn run_in_desktop(&self, arguments: &[&str], desktop: Option<&str>) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/omarchy-install.sh");
        let binary = format!("--binary={}", self.root.join("fake-kokuban").display());
        let mut command = Command::new("bash");
        command.arg(script);
        if arguments.first() != Some(&"uninstall") {
            // A --binary given by the test comes later and takes precedence.
            command.arg(binary);
        }
        command
            .args(arguments)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home());
        if let Some(desktop) = desktop {
            command.env("XDG_CURRENT_DESKTOP", desktop);
        }
        command.output().unwrap()
    }

    fn run_ok(&self, arguments: &[&str]) -> String {
        self.run_ok_in_desktop(arguments, Some("Hyprland"))
    }

    fn run_ok_in_desktop(&self, arguments: &[&str], desktop: Option<&str>) -> String {
        let output = self.run_in_desktop(arguments, desktop);
        assert!(
            output.status.success(),
            "{arguments:?} failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Every regular file below HOME with its contents.
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(directory: &Path, base: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, base, files);
                } else {
                    let contents = fs::read(&path).unwrap();
                    files.insert(path.strip_prefix(base).unwrap().to_path_buf(), contents);
                }
            }
        }
        let mut files = BTreeMap::new();
        walk(&self.home(), &self.home(), &mut files);
        files
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn entries(list: &str) -> Vec<&str> {
    list.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

#[test]
fn lua_install_selects_tags_and_uninstall_restores_the_home() {
    let home = Home::new();
    home.write(".config/hypr/hyprland.lua", USER_LUA);
    let before = home.files();

    home.run_ok(&[]);
    assert_eq!(
        fs::read(home.path(".local/bin/kokuban")).unwrap(),
        b"#!/bin/sh\nexit 0\n"
    );
    let mode = fs::metadata(home.path(".local/bin/kokuban"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert!(home.path(".local/share/kokuban/kokuban-icon.png").is_file());
    assert_eq!(
        entries(&home.read(".config/xdg-terminals.list")),
        [DESKTOP_ID]
    );
    let lua = home.read(".config/hypr/hyprland.lua");
    assert!(
        lua.starts_with(USER_LUA),
        "user configuration is kept first"
    );
    assert_eq!(lua.matches(LUA_RULE).count(), 1);
    assert!(!lua.contains("windowrule"));
    let installed = home.files();

    // Repeating the installation changes nothing.
    home.run_ok(&["install"]);
    assert_eq!(home.files(), installed);

    home.run_ok(&["uninstall"]);
    assert_eq!(home.files(), before);
    // Uninstalling twice is harmless.
    home.run_ok(&["uninstall"]);
    assert_eq!(home.files(), before);
}

#[test]
fn desktop_entry_points_at_the_installed_files_and_keeps_terminal_arguments() {
    let home = Home::new();
    home.run_ok(&["--no-default", "--no-hypr"]);
    let entry = home.read(&format!(".local/share/applications/{DESKTOP_ID}"));
    let home_path = home.home();
    let home_path = home_path.display();
    let lines: Vec<&str> = entry.lines().collect();
    assert_eq!(lines[0], "[Desktop Entry]");
    for expected in [
        format!("Exec=\"{home_path}/.local/bin/kokuban\""),
        format!("Icon={home_path}/.local/share/kokuban/kokuban-icon.png"),
        format!("Path={home_path}"),
        "Categories=System;TerminalEmulator;".into(),
        "StartupWMClass=io.github.guicybercode.kokuban".into(),
        "X-TerminalArgExec=-e".into(),
        "X-TerminalArgAppId=--app-id=".into(),
        "X-TerminalArgTitle=--title=".into(),
        "X-TerminalArgDir=--working-directory=".into(),
    ] {
        assert_eq!(
            lines.iter().filter(|line| **line == expected).count(),
            1,
            "{expected} in\n{entry}"
        );
    }
    for key in ["Exec=", "Icon=", "Path="] {
        assert_eq!(lines.iter().filter(|l| l.starts_with(key)).count(), 1);
    }
    if let Ok(output) = Command::new("desktop-file-validate")
        .arg(home.path(&format!(".local/share/applications/{DESKTOP_ID}")))
        .output()
    {
        assert!(
            output.status.success() && output.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    assert!(!home.path(".config").exists());
}

#[test]
fn omarchy_menu_choice_stays_as_a_fallback_and_returns_on_uninstall() {
    let home = Home::new();
    home.write(".config/xdg-terminals.list", OMARCHY_GHOSTTY_LIST);
    home.run_ok(&["--no-hypr"]);
    let list = home.read(".config/xdg-terminals.list");
    assert!(list.starts_with(
        "# Terminal emulator preference order for xdg-terminal-exec\n\
         # The first found and valid terminal will be used\n"
    ));
    assert_eq!(
        entries(&list),
        [DESKTOP_ID, "com.mitchellh.ghostty.desktop"]
    );
    home.run_ok(&["--no-hypr"]);
    assert_eq!(home.read(".config/xdg-terminals.list"), list);
    home.run_ok(&["uninstall"]);
    assert_eq!(
        home.read(".config/xdg-terminals.list"),
        OMARCHY_GHOSTTY_LIST
    );
}

#[test]
fn an_existing_kokuban_entry_moves_to_the_front_once() {
    let home = Home::new();
    home.write(
        ".config/xdg-terminals.list",
        &format!("kitty.desktop\n  {DESKTOP_ID}  \nfoot.desktop\n"),
    );
    home.run_ok(&["--no-hypr"]);
    assert_eq!(
        home.read(".config/xdg-terminals.list"),
        format!("{DESKTOP_ID}\nkitty.desktop\nfoot.desktop\n")
    );
}

#[test]
fn desktop_specific_list_receives_the_preference() {
    // ~/.config/hyprland-xdg-terminals.list outranks ~/.config/xdg-terminals.list.
    let home = Home::new();
    home.write(".config/hyprland-xdg-terminals.list", "kitty.desktop\n");
    home.run_ok(&["--no-hypr"]);
    assert_eq!(
        entries(&home.read(".config/hyprland-xdg-terminals.list")),
        [DESKTOP_ID, "kitty.desktop"]
    );
    assert!(!home.path(".config/xdg-terminals.list").exists());
    // Uninstalling from another desktop session still finds the entry.
    home.run_ok_in_desktop(&["uninstall"], Some("GNOME"));
    assert_eq!(
        home.read(".config/hyprland-xdg-terminals.list"),
        "kitty.desktop\n"
    );
}

#[test]
fn outside_the_graphical_session_the_hyprland_list_is_still_used() {
    // From a TTY or SSH XDG_CURRENT_DESKTOP is unset; Omarchy runs Hyprland.
    let home = Home::new();
    home.write(".config/hyprland-xdg-terminals.list", "foot.desktop\n");
    home.run_ok_in_desktop(&["--no-hypr"], None);
    assert_eq!(
        entries(&home.read(".config/hyprland-xdg-terminals.list")),
        [DESKTOP_ID, "foot.desktop"]
    );
    assert!(!home.path(".config/xdg-terminals.list").exists());
}

#[test]
fn legacy_conf_configuration_receives_a_windowrule() {
    let home = Home::new();
    let conf = "source = ~/.config/hypr/looknfeel.conf\n\n\
                # Add any other personal Hyprland configuration below\n";
    home.write(".config/hypr/hyprland.conf", conf);
    home.run_ok(&["--no-default"]);
    let installed = home.read(".config/hypr/hyprland.conf");
    assert!(installed.starts_with(conf));
    assert_eq!(installed.matches(CONF_RULE).count(), 1);
    assert!(!installed.contains("o.window"));
    assert!(!home.path(".config/hypr/hyprland.lua").exists());
    home.run_ok(&["uninstall"]);
    assert_eq!(home.read(".config/hypr/hyprland.conf"), conf);
}

#[test]
fn lua_configuration_wins_when_both_generations_exist() {
    let home = Home::new();
    home.write(".config/hypr/hyprland.lua", USER_LUA);
    home.write(".config/hypr/hyprland.conf", "# old\n");
    home.run_ok(&["--no-default"]);
    assert!(home.read(".config/hypr/hyprland.lua").contains(LUA_RULE));
    assert_eq!(home.read(".config/hypr/hyprland.conf"), "# old\n");

    home.run_ok(&["--no-default", "--hypr-format", "conf"]);
    assert!(home.read(".config/hypr/hyprland.conf").contains(CONF_RULE));
    home.run_ok(&["uninstall"]);
    assert_eq!(home.read(".config/hypr/hyprland.lua"), USER_LUA);
    assert_eq!(home.read(".config/hypr/hyprland.conf"), "# old\n");
}

#[test]
fn without_hyprland_configuration_the_tag_is_skipped() {
    let home = Home::new();
    let stdout = home.run_ok(&["--no-default"]);
    assert!(stdout.contains("skipped the terminal tag"), "{stdout}");
    assert!(!home.path(".config").exists());
}

#[test]
fn invalid_input_fails_before_changing_the_home() {
    let home = Home::new();
    home.write(".config/hypr/hyprland.lua", USER_LUA);
    let before = home.files();
    for arguments in [
        vec!["--hypr-format", "yaml"],
        vec!["--unknown"],
        vec!["--binary", "/nonexistent/kokuban"],
    ] {
        let output = home.run(&arguments);
        assert!(!output.status.success(), "{arguments:?}");
        assert_eq!(home.files(), before, "{arguments:?}");
    }
}
