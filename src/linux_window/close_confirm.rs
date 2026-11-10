//! Confirmation before closing a pane or the window while a job runs in the
//! foreground of its PTY (an editor, `ssh`, a build). An idle shell closes
//! without asking. `[confirm] on_close_pane` and `on_quit` turn it off.

use crate::layout::PaneId;
use crate::pane::pane::Pane;
use crate::pane::PaneTree;
use nix::unistd::Pid;
use std::path::Path;
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CloseTarget {
    Pane(PaneId),
    Window,
}

/// A close waiting for an answer, and the jobs that triggered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClosePrompt {
    pub(super) target: CloseTarget,
    pub(super) running: Vec<String>,
}

impl ClosePrompt {
    pub(super) fn question(&self) -> &'static str {
        match self.target {
            CloseTarget::Pane(_) => "Close pane",
            CloseTarget::Window => "Close window",
        }
    }

    /// True for the prompt about closing `id`; it goes away with that pane.
    pub(super) fn asks_to_close_pane(&self, id: PaneId) -> bool {
        self.target == CloseTarget::Pane(id)
    }

    /// Pane whose last row shows the prompt: the pane being closed, or the
    /// focused pane for the window.
    pub(super) fn shown_in(&self, focused: PaneId) -> PaneId {
        match self.target {
            CloseTarget::Pane(id) => id,
            CloseTarget::Window => focused,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CloseAnswer {
    Confirm,
    Cancel,
    /// Swallowed: nothing reaches the shell while the prompt is open.
    Ignore,
}

/// `y` or Enter confirms, `n` or Escape cancels; chords are ignored.
pub(super) fn close_answer(logical_key: &Key, modifiers: ModifiersState) -> CloseAnswer {
    if modifiers.control_key() || modifiers.alt_key() || modifiers.super_key() {
        return CloseAnswer::Ignore;
    }
    match logical_key {
        Key::Named(NamedKey::Enter) => CloseAnswer::Confirm,
        Key::Named(NamedKey::Escape) => CloseAnswer::Cancel,
        Key::Character(text) => match text.as_str() {
            "y" | "Y" => CloseAnswer::Confirm,
            "n" | "N" => CloseAnswer::Cancel,
            _ => CloseAnswer::Ignore,
        },
        _ => CloseAnswer::Ignore,
    }
}

/// Short name of a process group's leader, from `/proc/<pid>/comm` under
/// `proc_root`; `pid N` when the leader already left (a pipeline whose first
/// command exited) or `/proc` is unavailable.
pub(super) fn process_name_in(proc_root: &Path, group: Pid) -> String {
    std::fs::read_to_string(proc_root.join(group.to_string()).join("comm"))
        .ok()
        .map(|name| name.trim_end_matches('\n').to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("pid {group}"))
}

fn running_job(pane: &Pane) -> Option<String> {
    let group = pane.pty.foreground_job()?;
    Some(process_name_in(Path::new("/proc"), group))
}

/// Prompt for closing `id`, or `None` to close it right away.
pub(super) fn pane_close_prompt(
    enabled: bool,
    id: PaneId,
    pane: Option<&Pane>,
) -> Option<ClosePrompt> {
    if !enabled {
        return None;
    }
    let running = running_job(pane?)?;
    Some(ClosePrompt {
        target: CloseTarget::Pane(id),
        running: vec![running],
    })
}

/// Prompt for closing the window, or `None` to close it right away.
pub(super) fn window_close_prompt(enabled: bool, tree: &PaneTree) -> Option<ClosePrompt> {
    if !enabled {
        return None;
    }
    let jobs = tree
        .pane_ids()
        .into_iter()
        .filter_map(|id| tree.pane(id).and_then(running_job));
    window_prompt_from(jobs)
}

fn window_prompt_from(jobs: impl Iterator<Item = String>) -> Option<ClosePrompt> {
    let mut running: Vec<String> = Vec::new();
    for job in jobs {
        if !running.contains(&job) {
            running.push(job);
        }
    }
    (!running.is_empty()).then_some(ClosePrompt {
        target: CloseTarget::Window,
        running,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        close_answer, pane_close_prompt, process_name_in, window_close_prompt, window_prompt_from,
        CloseAnswer, ClosePrompt, CloseTarget,
    };
    use crate::pane::PaneTree;
    use crate::parser::ansi::GraphicsSupport;
    use crate::renderer::kitty_handler::KittyHandlerOptions;
    use nix::unistd::Pid;
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use winit::keyboard::{Key, ModifiersState, NamedKey, SmolStr};

    #[test]
    fn y_and_enter_confirm_n_and_escape_cancel_everything_else_is_swallowed() {
        let none = ModifiersState::empty();
        let character = |text: &str| Key::Character(SmolStr::new(text));
        assert_eq!(close_answer(&character("y"), none), CloseAnswer::Confirm);
        assert_eq!(
            close_answer(&character("Y"), ModifiersState::SHIFT),
            CloseAnswer::Confirm
        );
        assert_eq!(
            close_answer(&Key::Named(NamedKey::Enter), none),
            CloseAnswer::Confirm
        );
        assert_eq!(close_answer(&character("n"), none), CloseAnswer::Cancel);
        assert_eq!(
            close_answer(&Key::Named(NamedKey::Escape), none),
            CloseAnswer::Cancel
        );
        assert_eq!(close_answer(&character("q"), none), CloseAnswer::Ignore);
        assert_eq!(
            close_answer(&Key::Named(NamedKey::Tab), none),
            CloseAnswer::Ignore
        );
        assert_eq!(
            close_answer(&character("y"), ModifiersState::CONTROL),
            CloseAnswer::Ignore
        );
        assert_eq!(
            close_answer(&Key::Named(NamedKey::Enter), ModifiersState::ALT),
            CloseAnswer::Ignore
        );
    }

    #[test]
    fn prompts_name_the_target_and_the_running_jobs() {
        let pane = ClosePrompt {
            target: CloseTarget::Pane(3),
            running: vec!["vim".to_string()],
        };
        assert_eq!(pane.question(), "Close pane");
        assert_eq!(pane.shown_in(1), 3);
        assert!(pane.asks_to_close_pane(3));
        assert!(!pane.asks_to_close_pane(1));

        let window =
            window_prompt_from(["vim", "ssh", "vim"].into_iter().map(str::to_string)).unwrap();
        assert_eq!(window.target, CloseTarget::Window);
        assert_eq!(window.question(), "Close window");
        assert_eq!(window.running, ["vim", "ssh"]);
        assert_eq!(window.shown_in(1), 1);
        assert!(!window.asks_to_close_pane(1));
        assert_eq!(window_prompt_from(std::iter::empty()), None);
    }

    #[test]
    fn disabled_or_missing_panes_close_without_a_prompt() {
        assert_eq!(pane_close_prompt(false, 1, None), None);
        assert_eq!(pane_close_prompt(true, 1, None), None);
    }

    fn tree_running(script: &str) -> PaneTree {
        let command: Vec<OsString> = ["/bin/bash", "-c", script]
            .into_iter()
            .map(OsString::from)
            .collect();
        PaneTree::new(
            80,
            24,
            1_000,
            KittyHandlerOptions::from_megabytes(1, false),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            Arc::new(|| {}),
            &command,
        )
        .expect("close confirmation test should spawn its PTY")
    }

    fn wait_for_job(tree: &PaneTree, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let pane = tree.focused_pane().unwrap();
        while pane
            .pty
            .foreground_job()
            .map(|group| process_name_in(Path::new("/proc"), group))
            .as_deref()
            != Some(name)
        {
            assert!(Instant::now() < deadline, "{name} never took the terminal");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_foreground_job_raises_both_prompts_unless_disabled() {
        // `set -m` hands the terminal to `sleep` as an interactive shell does.
        let tree = tree_running("set -m; sleep 30; true");
        wait_for_job(&tree, "sleep");
        let id = tree.focused;
        let pane = tree.pane(id);
        assert_eq!(
            pane_close_prompt(true, id, pane),
            Some(ClosePrompt {
                target: CloseTarget::Pane(id),
                running: vec!["sleep".to_string()],
            })
        );
        assert_eq!(pane_close_prompt(false, id, pane), None);
        assert_eq!(
            window_close_prompt(true, &tree),
            Some(ClosePrompt {
                target: CloseTarget::Window,
                running: vec!["sleep".to_string()],
            })
        );
        assert_eq!(window_close_prompt(false, &tree), None);
    }

    #[test]
    fn an_idle_program_in_the_foreground_closes_without_a_prompt() {
        let tree = tree_running("read value");
        let id = tree.focused;
        assert_eq!(pane_close_prompt(true, id, tree.pane(id)), None);
        assert_eq!(window_close_prompt(true, &tree), None);
    }

    #[test]
    fn process_names_come_from_comm_with_a_pid_fallback() {
        let root =
            std::env::temp_dir().join(format!("kokuban-close-confirm-proc-{}", std::process::id()));
        std::fs::create_dir_all(root.join("42")).unwrap();
        std::fs::write(root.join("42").join("comm"), "nvim\n").unwrap();
        std::fs::create_dir_all(root.join("44")).unwrap();
        std::fs::write(root.join("44").join("comm"), "").unwrap();
        let names = [42, 43, 44].map(|pid| process_name_in(&root, Pid::from_raw(pid)));
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(names, ["nvim", "pid 43", "pid 44"]);
    }
}
