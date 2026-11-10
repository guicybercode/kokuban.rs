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

    /// Points `relative` at `target` (taken as is, so it may be relative or
    /// missing), creating the link's directory.
    fn symlink(&self, target: &str, relative: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, path).unwrap();
    }

    fn link_target(&self, relative: &str) -> PathBuf {
        let path = self.path(relative);
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "{relative} is no longer a symbolic link"
        );
        fs::read_link(path).unwrap()
    }

    /// Every regular file below HOME with its contents, and every symbolic
    /// link with `-> target`, without following links.
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(directory: &Path, base: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let kind = entry.file_type().unwrap();
                let contents = if kind.is_dir() {
                    walk(&path, base, files);
                    continue;
                } else if kind.is_symlink() {
                    format!("-> {}", fs::read_link(&path).unwrap().display()).into_bytes()
                } else {
                    fs::read(&path).unwrap()
                };
                files.insert(path.strip_prefix(base).unwrap().to_path_buf(), contents);
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

#[test]
fn symlinked_dotfiles_are_edited_through_the_link() {
    // A dotfiles manager (stow, chezmoi symlink mode, a bare repository with
    // links) keeps the real files elsewhere; the links must survive.
    let home = Home::new();
    home.write("dotfiles/hypr/hyprland.lua", USER_LUA);
    home.write("dotfiles/xdg-terminals.list", OMARCHY_GHOSTTY_LIST);
    home.symlink(
        "../../dotfiles/hypr/hyprland.lua",
        ".config/hypr/hyprland.lua",
    );
    home.symlink(
        "../dotfiles/xdg-terminals.list",
        ".config/xdg-terminals.list",
    );
    let before = home.files();

    home.run_ok(&[]);
    assert_eq!(
        home.link_target(".config/hypr/hyprland.lua"),
        Path::new("../../dotfiles/hypr/hyprland.lua")
    );
    assert_eq!(
        home.link_target(".config/xdg-terminals.list"),
        Path::new("../dotfiles/xdg-terminals.list")
    );
    let lua = home.read("dotfiles/hypr/hyprland.lua");
    assert!(lua.starts_with(USER_LUA));
    assert_eq!(lua.matches(LUA_RULE).count(), 1);
    assert_eq!(
        entries(&home.read("dotfiles/xdg-terminals.list")),
        [DESKTOP_ID, "com.mitchellh.ghostty.desktop"]
    );
    // No temporary file is left next to the link or the target.
    for directory in ["dotfiles", "dotfiles/hypr", ".config", ".config/hypr"] {
        for entry in fs::read_dir(home.path(directory)).unwrap() {
            let name = entry.unwrap().file_name();
            assert!(!name.to_string_lossy().contains(".kokuban-"), "{name:?}");
        }
    }
    let installed = home.files();
    home.run_ok(&["install"]);
    assert_eq!(home.files(), installed);

    home.run_ok(&["uninstall"]);
    assert_eq!(home.files(), before);
}

#[test]
fn a_list_the_script_created_is_emptied_not_deleted_once_it_is_a_link() {
    // The list was created by the script and later moved into dotfiles.
    let home = Home::new();
    home.run_ok(&["--no-hypr"]);
    fs::create_dir_all(home.path("dotfiles")).unwrap();
    fs::rename(
        home.path(".config/xdg-terminals.list"),
        home.path("dotfiles/xdg-terminals.list"),
    )
    .unwrap();
    home.symlink(
        "../dotfiles/xdg-terminals.list",
        ".config/xdg-terminals.list",
    );

    home.run_ok(&["uninstall"]);
    home.link_target(".config/xdg-terminals.list");
    let list = home.read("dotfiles/xdg-terminals.list");
    assert!(entries(&list).is_empty(), "{list}");
}

#[test]
fn a_broken_link_is_refused_before_changing_the_home() {
    for (link, arguments) in [
        (".config/hypr/hyprland.lua", vec!["--no-default"]),
        (".config/hypr/hyprland.conf", vec!["--no-default"]),
        (".config/xdg-terminals.list", vec![]),
    ] {
        let home = Home::new();
        home.symlink("/nonexistent/dotfiles/file", link);
        let before = home.files();
        let output = home.run(&arguments);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{link}");
        assert!(
            stderr.contains("is a symbolic link") && stderr.contains("nothing was changed"),
            "{link}: {stderr}"
        );
        assert_eq!(home.files(), before, "{link}");
    }
}

#[test]
fn a_begin_marker_without_its_end_is_refused_before_changing_the_home() {
    // The end line was deleted by hand; stripping from the begin marker would
    // drop the user's configuration that follows it.
    let lua = format!(
        "{USER_LUA}\n-- >>> kokuban terminal tag >>>\n{LUA_RULE}\n\
         require(\"hypr.bindings\")\n-- keep this line\n"
    );
    let conf = "source = ~/.config/hypr/looknfeel.conf\n\
                # >>> kokuban terminal tag >>>\n\
                bind = SUPER, Return, exec, kokuban\n";
    for (file, contents, line) in [
        (".config/hypr/hyprland.lua", lua.as_str(), 8),
        (".config/hypr/hyprland.conf", conf, 2),
    ] {
        for arguments in [vec![], vec!["uninstall"]] {
            let home = Home::new();
            home.write(file, contents);
            home.write(".config/xdg-terminals.list", &format!("{DESKTOP_ID}\n"));
            let before = home.files();
            let output = home.run(&arguments);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{file} {arguments:?}");
            assert!(
                stderr.contains(&format!("line {line},"))
                    && stderr.contains("<<< kokuban terminal tag <<<")
                    && stderr.contains("nothing was changed"),
                "{file} {arguments:?}: {stderr}"
            );
            assert_eq!(home.files(), before, "{file} {arguments:?}");
        }
    }
}

#[test]
fn a_complete_block_written_by_hand_is_replaced_and_the_rest_kept() {
    let home = Home::new();
    let lua = format!(
        "{USER_LUA}\n-- >>> kokuban terminal tag >>>\n-- old rule\n\
         -- <<< kokuban terminal tag <<<\nrequire(\"hypr.after\")\n"
    );
    home.write(".config/hypr/hyprland.lua", &lua);
    home.run_ok(&["--no-default"]);
    let installed = home.read(".config/hypr/hyprland.lua");
    assert!(installed.contains("require(\"hypr.after\")"), "{installed}");
    assert!(!installed.contains("-- old rule"), "{installed}");
    assert_eq!(installed.matches(LUA_RULE).count(), 1);
    home.run_ok(&["uninstall"]);
    assert_eq!(
        home.read(".config/hypr/hyprland.lua"),
        format!("{USER_LUA}require(\"hypr.after\")\n")
    );
}
