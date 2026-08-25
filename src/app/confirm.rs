use crate::layout::PaneId;
use nix::libc;
use std::os::fd::RawFd;
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    ClosePane(PaneId),
    QuitApp,
}

pub enum ConfirmResult {
    Pending,
    Confirmed,
    Cancelled,
}

pub struct ConfirmDialog {
    pub action: ConfirmAction,
    pub process_name: Option<String>,
    pub created_at: Instant,
}

impl ConfirmDialog {
    pub fn new(action: ConfirmAction, process_name: Option<String>) -> Self {
        Self {
            action,
            process_name,
            created_at: Instant::now(),
        }
    }

    /// Current opacity for fade-in animation (0.0 → 1.0 over 150ms).
    pub fn opacity(&self) -> f32 {
        let elapsed = self.created_at.elapsed().as_secs_f32();
        (elapsed / 0.15).min(1.0)
    }

    /// Returns true while the fade-in animation is still playing.
    pub fn is_animating(&self) -> bool {
        self.created_at.elapsed().as_secs_f32() < 0.15
    }

    /// Handle input. Accepts both key_code (for Enter/Escape) and optional
    /// character (for Y/N, layout-independent).
    pub fn handle_input(&self, key_code: u16, character: Option<char>) -> ConfirmResult {
        // Check character first (layout-independent Y/N)
        if let Some(ch) = character {
            match ch {
                'y' | 'Y' => return ConfirmResult::Confirmed,
                'n' | 'N' => return ConfirmResult::Cancelled,
                '\r' | '\n' => return ConfirmResult::Confirmed,
                _ => {}
            }
        }

        // Fall back to key codes for special keys
        match key_code {
            36 => ConfirmResult::Confirmed,  // Return
            53 => ConfirmResult::Cancelled,   // Escape
            _ => ConfirmResult::Pending,
        }
    }

    /// Generate the title text for display.
    pub fn title_text(&self) -> &'static str {
        match &self.action {
            ConfirmAction::ClosePane(_) => "Close this pane?",
            ConfirmAction::QuitApp => "Quit Kokuban?",
        }
    }

    /// Generate the process status text, if any.
    pub fn process_text(&self) -> Option<String> {
        self.process_name.as_ref().map(|name| format!("{name} is running"))
    }
}

/// Get the name of the foreground process for a PTY master fd.
pub fn foreground_process_name(master_fd: RawFd) -> Option<String> {
    extern "C" {
        fn proc_name(pid: i32, buffer: *mut libc::c_void, buffersize: u32) -> i32;
    }

    let fg_pgid = unsafe { libc::tcgetpgrp(master_fd) };
    if fg_pgid <= 0 {
        return None;
    }

    let mut name_buf = [0u8; 256];
    let len = unsafe {
        proc_name(
            fg_pgid,
            name_buf.as_mut_ptr() as *mut libc::c_void,
            name_buf.len() as u32,
        )
    };

    if len > 0 {
        Some(String::from_utf8_lossy(&name_buf[..len as usize]).to_string())
    } else {
        None
    }
}
