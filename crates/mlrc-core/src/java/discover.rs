//! Finding a JDK: managed runtimes first, host only when explicitly allowed.

use std::path::{Path, PathBuf};

use crate::port::DataRoot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JavaSource {
    Managed,
    Host,
}

#[derive(Debug, Clone)]
pub struct JavaInstallation {
    pub home: PathBuf,
    pub executable: PathBuf,
    pub major: u32,
    pub source: JavaSource,
}

pub fn executable_in(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("bin").join("java.exe")
    } else {
        home.join("bin").join("java")
    }
}

/// Reads `release`, which every modern JDK ships:
/// `JAVA_VERSION="17.0.15"` or `JAVA_VERSION="1.8.0_202"`.
pub fn read_release_major(home: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))?;
    parse_major(line.trim().trim_matches('"'))
}

/// `17.0.15` -> 17, `25.0.1` -> 25, `1.8.0_202` -> 8, `8u51-cacert462b08` -> 8.
pub fn parse_major(version: &str) -> Option<u32> {
    let mut parts = version.split(['.', '_', '-', '+']);
    let first = leading_digits(parts.next()?)?;
    if first == 1 {
        leading_digits(parts.next()?)
    } else {
        Some(first)
    }
}

/// Java 8 runtimes are named `8u51`, so the first component is not purely numeric.
fn leading_digits(text: &str) -> Option<u32> {
    let digits: String = text.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

pub fn managed(root: &DataRoot, major: u32) -> Option<JavaInstallation> {
    let home = root.jdk_dir(major);
    if !home.is_dir() {
        return None;
    }
    let executable = executable_in(&home);
    if !executable.is_file() {
        return None;
    }
    Some(JavaInstallation {
        home,
        executable,
        major,
        source: JavaSource::Managed,
    })
}

/// Host JDK, used only when `customJava=true`. `explicit` is `config.java.path`.
pub fn host(explicit: &str) -> Option<JavaInstallation> {
    if !explicit.is_empty() {
        let executable = PathBuf::from(explicit);
        if !executable.is_file() {
            return None;
        }
        let home = executable.parent()?.parent()?.to_path_buf();
        let major = read_release_major(&home)?;
        return Some(JavaInstallation {
            home,
            executable,
            major,
            source: JavaSource::Host,
        });
    }

    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let home = PathBuf::from(home);
        let executable = executable_in(&home);
        if executable.is_file() {
            if let Some(major) = read_release_major(&home) {
                return Some(JavaInstallation {
                    home,
                    executable,
                    major,
                    source: JavaSource::Host,
                });
            }
        }
    }

    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let executable = if cfg!(windows) {
            dir.join("java.exe")
        } else {
            dir.join("java")
        };
        if !executable.is_file() {
            continue;
        }
        let Some(home) = dir.parent() else { continue };
        if let Some(major) = read_release_major(home) {
            return Some(JavaInstallation {
                home: home.to_path_buf(),
                executable,
                major,
                source: JavaSource::Host,
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modern_versions_report_their_own_major() {
        assert_eq!(parse_major("17.0.15"), Some(17));
        assert_eq!(parse_major("25.0.1"), Some(25));
        assert_eq!(parse_major("21"), Some(21));
    }

    #[test]
    fn java_eight_uses_the_second_component() {
        assert_eq!(parse_major("1.8.0_202"), Some(8));
        assert_eq!(parse_major("1.8.0_74"), Some(8));
    }

    #[test]
    fn legacy_eight_names_keep_their_major() {
        // Mojang names the java 8 runtimes `8u51-cacert462b08`.
        assert_eq!(parse_major("8u51-cacert462b08"), Some(8));
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(parse_major(""), None);
        assert_eq!(parse_major("not-a-version"), None);
    }
}
