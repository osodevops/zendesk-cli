//! Spinners and bars on stderr — shown only when stderr is a terminal and `--quiet` is off,
//! so piped or scripted runs never see control sequences.

use std::io::IsTerminal;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

/// A progress indicator that is a no-op when hidden.
#[derive(Debug, Clone)]
pub struct Progress {
    bar: ProgressBar,
}

impl Progress {
    /// Whether indicators would be visible right now.
    #[must_use]
    pub fn enabled() -> bool {
        std::io::stderr().is_terminal() && !super::is_quiet()
    }

    /// An indeterminate spinner with a message.
    #[must_use]
    pub fn spinner(message: impl Into<String>) -> Self {
        let bar = if Self::enabled() {
            let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
            bar.set_style(ProgressStyle::default_spinner());
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        } else {
            ProgressBar::hidden()
        };
        bar.set_message(message.into());
        Self { bar }
    }

    /// A bar for a known number of steps (records, pages).
    #[must_use]
    pub fn bar(len: u64, message: impl Into<String>) -> Self {
        let bar = if Self::enabled() {
            let bar = ProgressBar::with_draw_target(Some(len), ProgressDrawTarget::stderr());
            let style =
                ProgressStyle::with_template("{spinner} {msg} [{bar:30}] {pos}/{len} ({eta})")
                    .unwrap_or_else(|_| ProgressStyle::default_bar());
            bar.set_style(style);
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        } else {
            ProgressBar::hidden()
        };
        bar.set_message(message.into());
        Self { bar }
    }

    pub fn set_message(&self, message: impl Into<String>) {
        self.bar.set_message(message.into());
    }

    pub fn inc(&self, delta: u64) {
        self.bar.inc(delta);
    }

    pub fn set_length(&self, len: u64) {
        self.bar.set_length(len);
    }

    /// Remove the indicator from the terminal.
    pub fn finish(&self) {
        self.bar.finish_and_clear();
    }

    #[must_use]
    pub fn is_hidden(&self) -> bool {
        self.bar.is_hidden()
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_when_not_a_terminal_or_quiet() {
        // Test harnesses capture stderr, so this is always hidden here — and must not panic.
        let p = Progress::spinner("working");
        p.set_message("still working");
        p.inc(1);
        let b = Progress::bar(10, "pages");
        b.inc(3);
        b.set_length(20);
        if !std::io::stderr().is_terminal() {
            assert!(p.is_hidden());
            assert!(b.is_hidden());
        }
        super::super::set_quiet(true);
        assert!(!Progress::enabled());
        assert!(Progress::spinner("q").is_hidden());
        super::super::set_quiet(false);
    }
}
