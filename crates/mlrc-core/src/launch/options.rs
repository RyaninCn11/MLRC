//! `options.txt`: the file Minecraft reads for language, window size and the rest.
//!
//! A launcher cannot set the in-game language through an argument; the game only
//! reads it from this file. Editing one line before the process starts is the whole
//! feature.

use std::path::Path;

use crate::error::{ErrorCode, MlrcError, MlrcResult};

pub const FILE_NAME: &str = "options.txt";

/// Sets `lang:<code>` in the game directory's `options.txt`, creating the file if it
/// does not exist yet and leaving every other line untouched.
pub fn set_language(game_dir: &Path, code: &str) -> MlrcResult<()> {
    validate_language(code)?;

    let path = game_dir.join(FILE_NAME);
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(MlrcError::new(
                ErrorCode::Io,
                format!("cannot read {}: {err}", path.display()),
            ))
        }
    };

    std::fs::create_dir_all(game_dir)?;
    std::fs::write(&path, with_language(&existing, code)).map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!("cannot write {}: {err}", path.display()),
        )
    })
}

/// Pure part, so the rewriting rules are testable without a directory.
pub fn with_language(text: &str, code: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();

    let mut replaced = false;
    for line in &mut lines {
        if line.starts_with("lang:") {
            *line = format!("lang:{code}");
            replaced = true;
            break;
        }
    }
    if !replaced {
        lines.insert(0, format!("lang:{code}"));
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Minecraft's codes are lowercase identifiers: `en_us`, `zh_cn`, `pt_br`.
pub fn validate_language(code: &str) -> MlrcResult<()> {
    let valid = !code.is_empty()
        && code.len() <= 16
        && code
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_');

    if valid {
        Ok(())
    } else {
        Err(MlrcError::new(
            ErrorCode::Usage,
            format!("invalid language code: {code} (expected lowercase, e.g. zh_cn)"),
        )
        .with("language", code.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_an_existing_language_line() {
        let text = "fov:0.0\nlang:en_us\npauseOnLostFocus:true\n";
        assert_eq!(
            with_language(text, "zh_cn"),
            "fov:0.0\nlang:zh_cn\npauseOnLostFocus:true\n"
        );
    }

    #[test]
    fn inserts_when_the_file_has_no_language_yet() {
        assert_eq!(with_language("", "zh_cn"), "lang:zh_cn\n");
        assert_eq!(with_language("fov:0.0\n", "zh_cn"), "lang:zh_cn\nfov:0.0\n");
    }

    #[test]
    fn codes_are_lowercase_identifiers() {
        assert!(validate_language("zh_cn").is_ok());
        assert!(validate_language("en_us").is_ok());
        assert!(validate_language("pt_br").is_ok());
        assert!(validate_language("ZH_CN").is_err());
        assert!(validate_language("").is_err());
        assert!(validate_language("zh cn").is_err());
    }
}
