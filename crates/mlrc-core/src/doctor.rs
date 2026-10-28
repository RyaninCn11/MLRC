//! `mlrc doctor`: report the environment without changing it.
//!
//! Everything here is a *check*: it reads, times and reports. The one network call
//! exists so that "it downloads slowly" and "it cannot download" are distinguishable
//! from the first line of output.

use std::path::Path;
use std::time::Instant;

use crate::fetch::HttpClient;
use crate::java::discover;
use crate::meta::MANIFEST_URL;
use crate::port::{config::Config, state::InstalledState, DataRoot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

pub async fn run(root: &DataRoot, cfg: &Config, client: &HttpClient) -> Vec<Check> {
    let mut checks = Vec::new();

    checks.push(Check {
        name: "data root",
        status: Status::Ok,
        detail: format!(
            "{} ({})",
            root.root().display(),
            if root.is_portable() {
                "portable"
            } else {
                "explicit"
            }
        ),
    });

    let writable = root.probe_writable();
    checks.push(Check {
        name: "writable",
        status: if writable.is_ok() {
            Status::Ok
        } else {
            Status::Fail
        },
        detail: match &writable {
            Ok(()) => "ok".to_string(),
            Err(err) => err.message.clone(),
        },
    });

    let started = Instant::now();
    let network = client.get_bytes(MANIFEST_URL).await;
    let elapsed = started.elapsed().as_millis();
    checks.push(Check {
        name: "network",
        status: if network.is_ok() {
            Status::Ok
        } else {
            Status::Fail
        },
        detail: match network {
            Ok(bytes) => format!(
                "manifest {} in {elapsed} ms",
                bytes_human(bytes.len() as u64)
            ),
            Err(err) => err.message,
        },
    });

    if cfg.java.custom {
        let source = if cfg.java.path.is_empty() {
            "JAVA_HOME / PATH".to_string()
        } else {
            cfg.java.path.clone()
        };
        checks.push(Check {
            name: "host java",
            status: Status::Warn,
            detail: format!("enabled (customJava=true), using {source}"),
        });
    } else {
        checks.push(Check {
            name: "host java",
            status: Status::Ok,
            detail: "ignored (customJava=false), managed runtimes only".to_string(),
        });
    }

    let (jdks, broken) = managed_jdks(root);
    checks.push(Check {
        name: "managed jdks",
        status: if jdks.is_empty() {
            Status::Warn
        } else if broken.is_empty() {
            Status::Ok
        } else {
            Status::Fail
        },
        detail: if jdks.is_empty() {
            "none".to_string()
        } else if broken.is_empty() {
            jdks.join(", ")
        } else {
            format!("{} (broken: {})", jdks.join(", "), broken.join(", "))
        },
    });

    let (complete, incomplete) = versions(root);
    checks.push(Check {
        name: "versions",
        status: if incomplete.is_empty() {
            Status::Ok
        } else {
            Status::Fail
        },
        detail: if complete.is_empty() && incomplete.is_empty() {
            "none".to_string()
        } else if incomplete.is_empty() {
            format!("{} complete", complete.len())
        } else {
            format!(
                "{} complete, {} incomplete ({})",
                complete.len(),
                incomplete.len(),
                incomplete.join(", ")
            )
        },
    });

    checks.push(Check {
        name: "mirror",
        status: Status::Ok,
        detail: format!("{} (override with --mirror)", cfg.general.mirror.as_str()),
    });

    checks.push(Check {
        name: "config",
        status: Status::Ok,
        detail: if root.config_path().is_file() {
            root.config_path().display().to_string()
        } else {
            "not created yet (defaults in use)".to_string()
        },
    });

    checks
}

fn managed_jdks(root: &DataRoot) -> (Vec<String>, Vec<String>) {
    let mut valid = Vec::new();
    let mut broken = Vec::new();

    for entry in list_dirs(&root.java_dir()) {
        // Directory names are `jdk-25`, not bare numbers.
        let Some(major) = entry
            .strip_prefix("jdk-")
            .and_then(|rest| rest.parse::<u32>().ok())
        else {
            continue;
        };
        match discover::managed(root, major) {
            Some(_) => valid.push(format!("jdk-{major}")),
            None => broken.push(format!("jdk-{major}")),
        }
    }

    valid.sort();
    broken.sort();
    (valid, broken)
}

fn versions(root: &DataRoot) -> (Vec<String>, Vec<String>) {
    let mut complete = Vec::new();
    let mut incomplete = Vec::new();

    for id in list_dirs(&root.versions_dir()) {
        let path = root
            .versions_dir()
            .join(&id)
            .join(InstalledState::FILE_NAME);
        match InstalledState::read(&path) {
            Ok(Some(state)) if state.complete => complete.push(id),
            _ => incomplete.push(id),
        }
    }

    (complete, incomplete)
}

fn list_dirs(path: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(path) else {
        return names;
    };
    for entry in entries.flatten() {
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    names.sort();
    names
}

fn bytes_human(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}
