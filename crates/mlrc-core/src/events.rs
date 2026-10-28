//! Progress and decision reporting.
//!
//! The engine never prints and never blocks on input. It emits [`Event`]s to an
//! [`EventSink`]; the CLI renders them as text, the future GUI as widgets, and
//! `--json` as NDJSON. `JavaRequired` in particular is a *question*, not a prompt:
//! whoever owns the UI decides whether to ask.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Phase {
        phase: String,
        total: Option<u64>,
    },
    FileStarted {
        path: String,
        total_bytes: Option<u64>,
    },
    FileProgress {
        path: String,
        bytes: u64,
    },
    FileFinished {
        path: String,
        ok: bool,
        /// Already in the object store: materialized instead of downloaded.
        cached: bool,
    },
    /// The version needs a Java runtime that is not installed yet.
    JavaRequired {
        component: String,
        major: u32,
        download_bytes: u64,
    },
    Warning {
        code: String,
        message: String,
    },
    /// Neutral progress, e.g. "running the installer" plus a heartbeat. Not a
    /// warning and not a result; a front-end may show it or ignore it.
    Note {
        message: String,
    },
    Done {
        message: String,
        ok: bool,
    },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}

/// Discards everything; useful in tests and in `--quiet` paths.
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: Event) {}
}

/// Adapts a closure into a sink.
pub struct FnSink<F>(pub F);

impl<F> EventSink for FnSink<F>
where
    F: Fn(Event) + Send + Sync,
{
    fn emit(&self, event: Event) {
        (self.0)(event);
    }
}
