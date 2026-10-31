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
            detail: format!("enabled (java.custom=true), using {source}"),
        });
    } else {
        checks.push(Check {
            name: "host java",
            status: Status::Ok,
            detail: "ignored (java.custom=false), managed runtimes only".to_string(),
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

    // Loader remapping writes jars through java.nio's zip filesystem, which trusts
    // Files.isWritable. Antivirus self-protection and restricted tokens can make that
    // report "read-only" for every path while writes still succeed, and the only
    // symptom is an opaque "remapping game jars" failure much later.
    if let Some((java, major)) = any_managed_java(root) {
        checks.push(java_writability(root, &java, major));
    }

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

/// Newest usable managed runtime, so the check runs whatever the user actually has.
fn any_managed_java(root: &DataRoot) -> Option<(std::path::PathBuf, u32)> {
    let mut best: Option<(u32, std::path::PathBuf)> = None;

    for entry in list_dirs(&root.java_dir()) {
        let Some(major) = entry
            .strip_prefix("jdk-")
            .and_then(|rest| rest.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some(installed) = discover::managed(root, major) {
            if best.as_ref().map(|(best, _)| major > *best).unwrap_or(true) {
                best = Some((major, installed.executable));
            }
        }
    }

    best.map(|(major, path)| (path, major))
}

const PROBE_SOURCE: &str = r#"import java.nio.file.*;

public class Probe {
    public static void main(String[] args) throws Exception {
        Path dir = Paths.get(args[0]);
        Path probe = dir.resolve("probe.tmp");
        Files.writeString(probe, "x");
        Files.delete(probe);
        System.out.println(Files.isWritable(dir) ? "OK" : "READONLY");
    }
}
"#;

fn java_writability(root: &DataRoot, java: &Path, major: u32) -> Check {
    // Single-file source launch needs Java 11; older runtimes get no verdict rather
    // than a misleading one.
    if major < 11 {
        return Check {
            name: "java writes",
            status: Status::Warn,
            detail: format!("skipped (java {major} cannot run the probe)"),
        };
    }

    let dir = root.tmp_dir().join("doctor-java");
    let source = dir.join("Probe.java");
    let prepared = std::fs::create_dir_all(&dir)
        .map_err(|err| err.to_string())
        .and_then(|()| std::fs::write(&source, PROBE_SOURCE).map_err(|err| err.to_string()));

    if let Err(err) = prepared {
        return Check {
            name: "java writes",
            status: Status::Warn,
            detail: format!("skipped ({err})"),
        };
    }

    let output = std::process::Command::new(java)
        .arg("-Djava.io.tmpdir=".to_string() + &root.tmp_dir().display().to_string())
        .arg(&source)
        .arg(root.root())
        .output();

    let text = match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        Err(err) => {
            return Check {
                name: "java writes",
                status: Status::Warn,
                detail: format!("skipped ({err})"),
            }
        }
    };

    if text.contains("READONLY") {
        Check {
            name: "java writes",
            status: Status::Warn,
            detail: "reports every path read-only; loader remapping (fabric, neoforge) \
                     will fail. The JVM is running with a restricted token: a sandbox, \
                     an agent or CI runner, or antivirus self-protection"
                .to_string(),
        }
    } else if text.contains("OK") {
        Check {
            name: "java writes",
            status: Status::Ok,
            detail: "ok".to_string(),
        }
    } else {
        Check {
            name: "java writes",
            status: Status::Warn,
            detail: "skipped (probe produced no verdict)".to_string(),
        }
    }
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
        match InstalledState::load(root, &id) {
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
