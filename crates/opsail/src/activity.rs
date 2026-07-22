use std::io::{self, IsTerminal as _};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

const TICK_INTERVAL: Duration = Duration::from_millis(80);

pub(crate) struct CliActivity {
    progress: Option<ProgressBar>,
}

#[derive(Clone)]
pub(crate) struct CliActivityHandle {
    progress: Option<ProgressBar>,
}

impl CliActivity {
    pub(crate) fn start(message: impl Into<String>) -> Self {
        Self::start_when(message, io::stderr().is_terminal())
    }

    fn start_when(message: impl Into<String>, interactive: bool) -> Self {
        if !interactive {
            return Self { progress: None };
        }

        let progress = ProgressBar::new_spinner();
        progress.set_draw_target(ProgressDrawTarget::stderr());
        let style = ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner())
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]);
        progress.set_style(style);
        progress.set_message(message.into());
        progress.enable_steady_tick(TICK_INTERVAL);
        Self {
            progress: Some(progress),
        }
    }

    pub(crate) fn finish(mut self) {
        self.clear();
    }

    pub(crate) fn set_message(&self, message: impl Into<String>) {
        if let Some(progress) = &self.progress {
            progress.set_message(message.into());
        }
    }

    pub(crate) fn handle(&self) -> CliActivityHandle {
        CliActivityHandle {
            progress: self.progress.clone(),
        }
    }

    fn clear(&mut self) {
        if let Some(progress) = self.progress.take() {
            progress.finish_and_clear();
        }
    }
}

impl CliActivityHandle {
    pub(crate) fn set_message(&self, message: impl Into<String>) {
        if let Some(progress) = &self.progress {
            progress.set_message(message.into());
        }
    }
}

impl Drop for CliActivity {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_interactive_activity_has_no_progress_renderer() {
        let activity = CliActivity::start_when("waiting", false);
        assert!(activity.progress.is_none());
    }
}
