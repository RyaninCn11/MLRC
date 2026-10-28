//! Event sink for the terminal: a coarse progress line plus warnings.
//!
//! Kept deliberately dumb. A real progress bar (indicatif) belongs here too, but
//! only once the download path is stable - the GUI will use its own sink anyway.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use mlrc_core::events::{Event, EventSink};

pub struct ProgressSink {
    live: bool,
    printed: AtomicBool,
    total: AtomicUsize,
    done: AtomicUsize,
}

impl ProgressSink {
    pub fn new(live: bool) -> Self {
        Self {
            live,
            printed: AtomicBool::new(false),
            total: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
        }
    }

    /// Terminate the inline progress line, but only if one was written.
    pub fn finish(&self) {
        if self.printed.load(Ordering::Relaxed) {
            println!();
        }
    }
}

impl EventSink for ProgressSink {
    fn emit(&self, event: Event) {
        match event {
            Event::Phase { total, .. } => {
                self.total
                    .store(total.unwrap_or(0) as usize, Ordering::Relaxed);
                self.done.store(0, Ordering::Relaxed);
            }
            Event::FileFinished { cached, .. } => {
                if cached {
                    return;
                }
                let done = self.done.fetch_add(1, Ordering::Relaxed) + 1;
                let total = self.total.load(Ordering::Relaxed);
                if self.live && total > 0 && (done % 25 == 0 || done == total) {
                    self.printed.store(true, Ordering::Relaxed);
                    print!("\rdownloading {done}/{total} files");
                    let _ = std::io::stdout().flush();
                }
            }
            Event::Warning { code, message } => {
                eprintln!("warning [{code}]: {message}");
            }
            // A note arrives while a progress line may be on screen.
            Event::Note { message } if self.live => {
                if self.printed.swap(false, Ordering::Relaxed) {
                    println!();
                }
                eprintln!("{message}");
            }
            _ => {}
        }
    }
}
