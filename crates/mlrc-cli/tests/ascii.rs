//! Enforces hard constraint C1: source stays ASCII-only, so the engine and the
//! CLI output can be consumed by a GUI without encoding surprises.

use std::path::{Path, PathBuf};

#[test]
fn sources_are_ascii_only() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate has a parent")
        .parent()
        .expect("crates directory has a parent")
        .join("crates");

    assert!(
        crates.is_dir(),
        "expected source tree at {}",
        crates.display()
    );

    let mut offenders = Vec::new();
    visit(&crates, &mut offenders);

    assert!(
        offenders.is_empty(),
        "non-ascii source files (constraint C1): {:?}",
        offenders
    );
}

fn visit(dir: &Path, offenders: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, offenders);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            let bytes = std::fs::read(&path).unwrap_or_default();
            if bytes.iter().any(|byte| *byte > 0x7f) {
                offenders.push(path);
            }
        }
    }
}
