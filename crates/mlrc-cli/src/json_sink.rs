//! The `--json` rendering of engine events.
//!
//! One JSON object per line on stdout, so a script or a front-end in another language
//! can follow an install without linking the crate. The vocabulary is documented in
//! `docs/API.md`; this file is the only place that produces it, and the mapping is a
//! pure function so it can be tested without a terminal.
//!
//! `Event::Done` is deliberately not emitted: it reports one step finishing, and the
//! CLI already reports the command's outcome with a richer event of its own. Two
//! "done" lines per command would make the stream ambiguous.

use std::io::Write;
use std::sync::Mutex;

use mlrc_core::events::{Event, EventSink};
use serde_json::json;

pub struct JsonSink {
    out: Mutex<Box<dyn Write + Send>>,
}

impl JsonSink {
    pub fn stdout() -> Self {
        Self {
            out: Mutex::new(Box::new(std::io::stdout())),
        }
    }
}

impl EventSink for JsonSink {
    fn emit(&self, event: Event) {
        let Some(line) = encode(&event) else {
            return;
        };
        if let Ok(mut out) = self.out.lock() {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}

/// One engine event as one NDJSON line, or `None` when the CLI reports it instead.
pub fn encode(event: &Event) -> Option<String> {
    let value = match event {
        Event::Phase { phase, total } => json!({
            "schema": 1,
            "event": "phase",
            "phase": phase,
            "total": total,
        }),
        Event::FileStarted { path, total_bytes } => json!({
            "schema": 1,
            "event": "file_started",
            "path": path,
            "total_bytes": total_bytes,
        }),
        Event::FileProgress { path, bytes } => json!({
            "schema": 1,
            "event": "file_progress",
            "path": path,
            "bytes": bytes,
        }),
        Event::FileFinished { path, ok } => json!({
            "schema": 1,
            "event": "file_finished",
            "path": path,
            "ok": ok,
        }),
        Event::JavaRequired {
            component,
            major,
            download_bytes,
        } => json!({
            "schema": 1,
            "event": "java_required",
            "component": component,
            "major": major,
            "download_bytes": download_bytes,
        }),
        Event::Warning { code, message } => json!({
            "schema": 1,
            "event": "warning",
            "code": code,
            "message": message,
        }),
        Event::Note { message } => json!({
            "schema": 1,
            "event": "note",
            "message": message,
        }),
        // The command's own event says it better, and once.
        Event::Done { .. } => return None,
    };

    serde_json::to_string(&value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_events() -> Vec<Event> {
        vec![
            Event::Phase {
                phase: "download".to_string(),
                total: Some(3),
            },
            Event::FileStarted {
                path: "1.21.1.jar".to_string(),
                total_bytes: Some(1024),
            },
            Event::FileProgress {
                path: "1.21.1.jar".to_string(),
                bytes: 512,
            },
            Event::FileFinished {
                path: "1.21.1.jar".to_string(),
                ok: true,
            },
            Event::JavaRequired {
                component: "java-runtime-beta".to_string(),
                major: 17,
                download_bytes: 0,
            },
            Event::Warning {
                code: "E_NET_MISSING_ASSET".to_string(),
                message: "skipped".to_string(),
            },
            Event::Note {
                message: "hello".to_string(),
            },
        ]
    }

    #[test]
    fn every_event_has_a_line_except_done() {
        for event in sample_events() {
            let line = encode(&event).expect("a line");
            let value: serde_json::Value = serde_json::from_str(&line).expect("valid json");
            assert_eq!(value["schema"], 1);
            assert!(value["event"].is_string(), "{line}");
        }

        assert!(encode(&Event::Done {
            message: "installed".to_string(),
            ok: true,
        })
        .is_none());
    }

    #[test]
    fn the_fields_match_the_contract() {
        let line = encode(&Event::FileFinished {
            path: "a.jar".to_string(),
            ok: true,
        })
        .expect("a line");
        let value: serde_json::Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(value["event"], "file_finished");
        assert_eq!(value["path"], "a.jar");
        assert_eq!(value["ok"], true);
        assert!(
            value.get("cached").is_none(),
            "the field is gone on purpose"
        );
    }
}
