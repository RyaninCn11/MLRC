//! Event sink for the terminal: a progress bar, warnings and notes.
//!
//! The engine emits counts; how they look is entirely this file's business. A GUI
//! front-end implements the same trait against its own widgets.

use std::sync::Mutex;

use indicatif::{ProgressBar, ProgressStyle};
use mlrc_core::events::{Event, EventSink};

const TEMPLATE: &str =
    "{spinner:.green} {msg} {bar:32.cyan/blue} {pos}/{len} [{elapsed_precise}, {per_sec}, eta {eta}]";
const CHARS: &str = "=> ";

pub struct ProgressSink {
    /// `None` until a phase with a known total arrives, and while not live.
    bar: Mutex<Option<ProgressBar>>,
    live: bool,
}

impl ProgressSink {
    pub fn new(live: bool) -> Self {
        Self {
            bar: Mutex::new(None),
            live,
        }
    }

    /// Clears the bar, so the caller's own output starts on a clean line.
    pub fn finish(&self) {
        if let Some(bar) = self.bar() {
            bar.finish_and_clear();
        }
    }

    fn bar(&self) -> Option<ProgressBar> {
        self.bar.lock().ok().and_then(|guard| guard.clone())
    }

    fn print(&self, line: &str) {
        match self.bar() {
            // suspending keeps the bar from drawing over the message
            Some(bar) => bar.suspend(|| eprintln!("{line}")),
            None => eprintln!("{line}"),
        }
    }
}

impl EventSink for ProgressSink {
    fn emit(&self, event: Event) {
        match event {
            Event::Phase { phase, total } => {
                let Some(total) = total.filter(|total| *total > 0) else {
                    return;
                };
                if !self.live {
                    return;
                }

                if let Some(previous) = self.bar() {
                    previous.finish_and_clear();
                }

                let bar = ProgressBar::new(total);
                if let Ok(style) = ProgressStyle::with_template(TEMPLATE) {
                    bar.set_style(style.progress_chars(CHARS));
                }
                bar.set_message(phase);
                if let Ok(mut guard) = self.bar.lock() {
                    *guard = Some(bar);
                }
            }
            Event::FileFinished { cached, .. } => {
                if cached {
                    return;
                }
                if let Some(bar) = self.bar() {
                    bar.inc(1);
                }
            }
            Event::Warning { code, message } => {
                self.print(&format!("warning [{code}]: {message}"));
            }
            Event::Note { message } if self.live => {
                self.print(&message);
            }
            _ => {}
        }
    }
}
