//! `${variable}` expansion for launch arguments.
//!
//! Unknown variables are left verbatim on purpose: a literal `${quickPlayPath}` in
//! a printed command is a bug you can see, while an empty string is a bug you
//! chase.

use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
pub struct Variables {
    values: BTreeMap<String, String>,
}

impl Variables {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        self.values.insert(key.to_string(), value.into());
    }

    pub fn expand(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;

        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            if let Some(end) = after.find('}') {
                let key = &after[..end];
                match self.values.get(key) {
                    Some(value) => out.push_str(value),
                    None => {
                        out.push_str("${");
                        out.push_str(key);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            } else {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }

        out.push_str(rest);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_known_variables() {
        let mut vars = Variables::new();
        vars.set("version_name", "1.12.2");
        vars.set("game_directory", "C:/mc");
        assert_eq!(
            vars.expand("--version ${version_name} --gameDir ${game_directory}"),
            "--version 1.12.2 --gameDir C:/mc"
        );
    }

    #[test]
    fn leaves_unknown_variables_visible() {
        let vars = Variables::new();
        assert_eq!(vars.expand("${nope}"), "${nope}");
    }

    #[test]
    fn handles_text_without_variables_and_trailing_braces() {
        let mut vars = Variables::new();
        vars.set("a", "A");
        assert_eq!(vars.expand("plain"), "plain");
        assert_eq!(vars.expand("${a} ${b"), "A ${b");
    }
}
