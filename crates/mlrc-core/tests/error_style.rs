//! Enforces the error-message style documented in `error.rs`.
//!
//! Messages are the only part of an `MlrcError` a person reads, so their shape is a
//! contract too: one lowercase sentence, no trailing period, no `error:` prefix, no
//! newlines, and any command mentioned backticked. A test that reads the source keeps
//! that true without anyone having to remember.

use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// The first string literal of a `MlrcError::new(` call, joined across lines.
fn first_literal(text: &str) -> Option<String> {
    let start = text.find("MlrcError::new(")? + "MlrcError::new(".len();
    let rest = &text[start..];

    let mut chars = rest.char_indices();
    let mut opened = false;
    let mut escaped = false;
    let mut literal = String::new();

    for (_, ch) in chars.by_ref() {
        if !opened {
            if ch == '"' {
                opened = true;
            } else if ch == ')' {
                return None;
            }
            continue;
        }

        if escaped {
            // A backslash at end of line is Rust's continuation: the newline and the
            // indentation that follows are not part of the string.
            if ch != '\n' {
                literal.push(ch);
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(literal);
        } else {
            literal.push(ch);
        }
    }

    None
}

#[test]
fn every_error_message_follows_the_style() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no sources found under {}",
        root.display()
    );

    let mut problems = Vec::new();
    let mut checked = 0usize;

    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (index, _) in text.match_indices("MlrcError::new(") {
            let Some(literal) = first_literal(&text[index..]) else {
                continue;
            };
            checked += 1;
            let label = format!("{}: {}", file.display(), literal);

            if literal.trim().is_empty() {
                problems.push(format!("{label}: empty message"));
            }
            if literal.trim() != literal {
                problems.push(format!("{label}: leading or trailing whitespace"));
            }
            if literal.contains('\n') {
                problems.push(format!("{label}: newline in a message"));
            }
            if literal.ends_with('.') {
                problems.push(format!("{label}: trailing period"));
            }

            let first = literal.chars().next().unwrap_or(' ');
            if first.is_ascii_uppercase() {
                problems.push(format!("{label}: starts with a capital"));
            }

            let lower = literal.to_ascii_lowercase();
            for prefix in ["error:", "error ", "failed:"] {
                if lower.starts_with(prefix) {
                    problems.push(format!("{label}: code-like prefix {prefix:?}"));
                }
            }

            // A command mentioned in a message is something to copy, so it is quoted.
            for command in ["mlrc "] {
                let mut search = literal.as_str();
                while let Some(position) = search.find(command) {
                    let before = search[..position].chars().next_back();
                    let quoted = before == Some('`');
                    if !quoted {
                        problems.push(format!("{label}: {command:?} is not backticked"));
                        break;
                    }
                    search = &search[position + command.len()..];
                }
            }
        }
    }

    assert!(
        problems.is_empty(),
        "error messages break the style:\n{}",
        problems.join("\n")
    );

    // A scanner that silently stopped matching would pass forever.
    assert!(
        checked > 60,
        "only {checked} messages were parsed; the scanner is probably broken"
    );
}
