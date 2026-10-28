//! Rule evaluation.
//!
//! Semantics, straight from the launcher spec: start denied, walk the rules in
//! order, the **last rule that matches wins**. Inside one rule, `os` and `features`
//! are AND-ed. Getting this backwards is the classic launcher bug: it makes Linux
//! users download Windows natives.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Allow,
    Disallow,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub action: Action,
    #[serde(default)]
    pub os: Option<OsRule>,
    #[serde(default)]
    pub features: Option<BTreeMap<String, bool>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OsRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    /// Regex in the spec; in practice a version prefix such as `^10\.`.
    #[serde(default)]
    pub version: Option<String>,
    /// Newer form used by 26.x for JDK flag selection.
    #[serde(default)]
    pub version_range: Option<VersionRange>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VersionRange {
    #[serde(default)]
    pub min: Option<String>,
    #[serde(default)]
    pub max: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsName {
    Windows,
    Osx,
    Linux,
}

impl OsName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Osx => "osx",
            Self::Linux => "linux",
        }
    }

    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Osx
        } else {
            Self::Linux
        }
    }
}

/// Everything a rule can ask about the machine.
#[derive(Debug, Clone)]
pub struct Env {
    pub os: OsName,
    pub arch: String,
    pub os_version: Option<String>,
    pub features: BTreeSet<String>,
}

impl Env {
    pub fn detect() -> Self {
        Self {
            os: OsName::current(),
            arch: current_arch(),
            os_version: os_version(),
            features: BTreeSet::new(),
        }
    }

    /// Feature flags live in the launcher, not in the data: demo mode, custom
    /// resolution, quick play.
    pub fn with_features(mut self, features: &[&str]) -> Self {
        self.features
            .extend(features.iter().map(|name| (*name).to_string()));
        self
    }
}

pub fn allowed(rules: &[Rule], env: &Env) -> bool {
    // An absent rule list means "always": most libraries carry no rules at all,
    // and treating those as denied would drop half the classpath.
    if rules.is_empty() {
        return true;
    }

    let mut allow = false;
    for rule in rules {
        if rule.matches(env) {
            allow = rule.action == Action::Allow;
        }
    }
    allow
}

impl Rule {
    pub fn matches(&self, env: &Env) -> bool {
        if let Some(os) = &self.os {
            if let Some(name) = &os.name {
                if name != env.os.as_str() {
                    return false;
                }
            }
            if let Some(arch) = &os.arch {
                if arch != &env.arch {
                    return false;
                }
            }
            if let Some(pattern) = &os.version {
                match &env.os_version {
                    Some(actual) if version_pattern_matches(pattern, actual) => {}
                    _ => return false,
                }
            }
            if let Some(range) = &os.version_range {
                match &env.os_version {
                    Some(actual) if range_matches(range, actual) => {}
                    _ => return false,
                }
            }
        }

        if let Some(features) = &self.features {
            for (name, expected) in features {
                if env.features.contains(name) != *expected {
                    return false;
                }
            }
        }

        true
    }
}

fn current_arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" | "amd64" => "x86_64".to_string(),
        "x86" | "i686" => "x86".to_string(),
        "aarch64" => "arm64".to_string(),
        other => other.to_string(),
    }
}

#[cfg(windows)]
fn os_version() -> Option<String> {
    let output = std::process::Command::new("cmd")
        .args(["/c", "ver"])
        .output()
        .ok()?;
    extract_version(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(windows))]
fn os_version() -> Option<String> {
    None
}

/// Pulls `10.0.26200.5074` out of `Microsoft Windows [Version 10.0.26200.5074]`.
fn extract_version(text: &str) -> Option<String> {
    let mut candidate = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            candidate.push(ch);
        } else {
            if is_version_like(&candidate) {
                return Some(candidate);
            }
            candidate.clear();
        }
    }
    if is_version_like(&candidate) {
        Some(candidate)
    } else {
        None
    }
}

fn is_version_like(text: &str) -> bool {
    text.split('.').count() >= 2
        && !text.starts_with('.')
        && !text.ends_with('.')
        && text.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
}

fn version_pattern_matches(pattern: &str, actual: &str) -> bool {
    let cleaned = pattern
        .trim_start_matches('^')
        .trim_end_matches('$')
        .replace("\\.", ".")
        .replace('\\', "");
    actual.starts_with(cleaned.trim())
}

fn range_matches(range: &VersionRange, actual: &str) -> bool {
    if let Some(min) = &range.min {
        if compare_versions(actual, min) < 0 {
            return false;
        }
    }
    if let Some(max) = &range.max {
        if compare_versions(actual, max) > 0 {
            return false;
        }
    }
    true
}

fn compare_versions(left: &str, right: &str) -> i32 {
    let mut left = left.split('.').map(|part| part.parse::<u64>().unwrap_or(0));
    let mut right = right
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0));
    loop {
        match (left.next(), right.next()) {
            (None, None) => return 0,
            (l, r) => {
                let l = l.unwrap_or(0);
                let r = r.unwrap_or(0);
                if l < r {
                    return -1;
                }
                if l > r {
                    return 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(os: OsName) -> Env {
        Env {
            os,
            arch: "x86_64".to_string(),
            os_version: Some("10.0.26200".to_string()),
            features: BTreeSet::new(),
        }
    }

    fn rule(json: &str) -> Rule {
        serde_json::from_str(json).expect("rule parses")
    }

    #[test]
    fn empty_rules_are_allowed() {
        assert!(allowed(&[], &env(OsName::Windows)));
    }

    #[test]
    fn allow_rule_matches_its_os_only() {
        let rules = vec![rule(r#"{"action":"allow","os":{"name":"windows"}}"#)];
        assert!(allowed(&rules, &env(OsName::Windows)));
        assert!(!allowed(&rules, &env(OsName::Linux)));
    }

    #[test]
    fn later_rule_overrides_earlier() {
        let rules = vec![
            rule(r#"{"action":"allow"}"#),
            rule(r#"{"action":"disallow","os":{"name":"osx"}}"#),
        ];
        assert!(allowed(&rules, &env(OsName::Windows)));
        assert!(!allowed(&rules, &env(OsName::Osx)));
    }

    #[test]
    fn feature_rules_need_the_feature() {
        let rules = vec![rule(
            r#"{"action":"allow","features":{"has_custom_resolution":true}}"#,
        )];
        assert!(!allowed(&rules, &env(OsName::Windows)));
        let with_feature = env(OsName::Windows).with_features(&["has_custom_resolution"]);
        assert!(allowed(&rules, &with_feature));
    }

    #[test]
    fn version_range_selects_modern_windows() {
        let modern = rule(
            r#"{"action":"allow","os":{"name":"windows","versionRange":{"min":"10.0.17134"}}}"#,
        );
        assert!(modern.matches(&env(OsName::Windows)));

        let mut old = env(OsName::Windows);
        old.os_version = Some("10.0.15063".to_string());
        assert!(!modern.matches(&old));
    }

    #[test]
    fn version_pattern_is_a_prefix_match() {
        assert!(version_pattern_matches("^10\\.", "10.0.26200.5074"));
        assert!(!version_pattern_matches("^6\\.1", "10.0.26200"));
    }

    #[test]
    fn os_version_is_extracted_from_ver_output() {
        assert_eq!(
            extract_version("Microsoft Windows [Version 10.0.26200.5074]").as_deref(),
            Some("10.0.26200.5074")
        );
        assert_eq!(extract_version("no version here"), None);
    }
}
