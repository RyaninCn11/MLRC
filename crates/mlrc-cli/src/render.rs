//! Rendering. The only place that is allowed to touch stdout.

use mlrc_core::error::MlrcError;
use serde_json::{json, Value};

pub struct Out {
    json: bool,
    quiet: bool,
}

impl Out {
    pub const fn new(json: bool, quiet: bool) -> Self {
        Self { json, quiet }
    }

    pub const fn is_json(&self) -> bool {
        self.json
    }

    /// Human text. Silenced under `--json` and `--quiet` so the two output
    /// channels never interleave.
    pub fn human(&self, text: impl AsRef<str>) {
        if self.json || self.quiet {
            return;
        }
        println!("{}", text.as_ref());
    }

    /// One NDJSON line. Nothing is written when `--json` is off.
    pub fn json_event(&self, value: Value) {
        if !self.json {
            return;
        }
        match serde_json::to_string(&value) {
            Ok(line) => println!("{line}"),
            Err(_) => println!("{{\"schema\":1,\"event\":\"encode_error\"}}"),
        }
    }
}

pub fn error(out: &Out, err: &MlrcError) {
    if out.is_json() {
        out.json_event(json!({
            "schema": 1,
            "event": "error",
            "code": err.code.as_str(),
            "message": err.message,
            "context": err.context,
        }));
    } else {
        eprintln!("error [{}]: {}", err.code.as_str(), err.message);
        if !err.context.is_null() {
            eprintln!("  context: {}", err.context);
        }
    }
}
