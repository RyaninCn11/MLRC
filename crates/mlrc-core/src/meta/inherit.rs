//! `inheritsFrom`: how mod loaders stack on top of a vanilla version.
//!
//! Merge order is child-overrides-parent for scalars, and parent-then-child for
//! argument lists. Libraries are deduplicated by exact coordinate, child first, so
//! a loader that pins a newer ASM keeps its jar ahead of the vanilla one on the
//! classpath instead of silently losing it.

use crate::meta::version::{Argument, Arguments, Library, VersionJson};

pub fn merge(child: &VersionJson, parent: &VersionJson) -> VersionJson {
    let mut merged = child.clone();

    if merged.main_class.is_empty() {
        merged.main_class = parent.main_class.clone();
    }
    if merged.assets.is_none() {
        merged.assets = parent.assets.clone();
    }
    if merged.asset_index.is_none() {
        merged.asset_index = parent.asset_index.clone();
    }
    if merged.java_version.is_none() {
        merged.java_version = parent.java_version.clone();
    }
    if merged.logging.is_none() {
        merged.logging = parent.logging.clone();
    }
    if merged.release_time.is_empty() {
        merged.release_time = parent.release_time.clone();
    }
    if merged.downloads.client.is_none() {
        merged.downloads.client = parent.downloads.client.clone();
    }

    merged.arguments = merge_arguments(child.arguments.as_ref(), parent.arguments.as_ref());
    merged.libraries = merge_libraries(&child.libraries, &parent.libraries);

    merged
}

fn merge_arguments(child: Option<&Arguments>, parent: Option<&Arguments>) -> Option<Arguments> {
    match (child, parent) {
        (None, None) => None,
        (Some(child), None) => Some(child.clone()),
        (None, Some(parent)) => Some(parent.clone()),
        (Some(child), Some(parent)) => Some(Arguments {
            game: concatenate(&parent.game, &child.game),
            jvm: concatenate(&parent.jvm, &child.jvm),
            default_user_jvm: concatenate(&parent.default_user_jvm, &child.default_user_jvm),
        }),
    }
}

fn concatenate(parent: &[Argument], child: &[Argument]) -> Vec<Argument> {
    let mut out = Vec::with_capacity(parent.len() + child.len());
    out.extend(parent.iter().cloned());
    out.extend(child.iter().cloned());
    out
}

fn merge_libraries(child: &[Library], parent: &[Library]) -> Vec<Library> {
    let mut out: Vec<Library> = Vec::with_capacity(child.len() + parent.len());
    for library in child.iter().chain(parent.iter()) {
        if !out.iter().any(|kept| kept.name == library.name) {
            out.push(library.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(json: &str) -> VersionJson {
        serde_json::from_str(json).expect("version parses")
    }

    #[test]
    fn scalar_fields_are_overridden_by_the_child() {
        let parent = version(
            r#"{"id":"1.21.1","mainClass":"net.minecraft.client.main.Main","assets":"17"}"#,
        );
        let child = version(
            r#"{"id":"1.21.1-fabric","mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient","inheritsFrom":"1.21.1"}"#,
        );

        let merged = merge(&child, &parent);
        assert_eq!(
            merged.main_class,
            "net.fabricmc.loader.impl.launch.knot.KnotClient"
        );
        assert_eq!(merged.assets.as_deref(), Some("17"));
    }

    #[test]
    fn arguments_are_concatenated_parent_first() {
        let parent = version(r#"{"id":"p","arguments":{"game":["--username"],"jvm":["-cp"]}}"#);
        let child = version(r#"{"id":"c","arguments":{"game":["--modLoader"]}}"#);

        let merged = merge(&child, &parent);
        let args = merged.arguments.expect("arguments survive");
        assert_eq!(args.game.len(), 2);
        assert!(matches!(&args.game[0], Argument::Plain(value) if value.as_str() == "--username"));
        assert!(matches!(&args.game[1], Argument::Plain(value) if value.as_str() == "--modLoader"));
    }

    #[test]
    fn libraries_dedup_on_exact_coordinate_only() {
        let parent = version(
            r#"{"id":"p","libraries":[{"name":"org.ow2.asm:asm:9.6","downloads":{"artifact":{"path":"a.jar"}}},{"name":"com.google.guava:guava:33.0.0","downloads":{"artifact":{"path":"g.jar"}}}]}"#,
        );
        let child = version(
            r#"{"id":"c","libraries":[{"name":"org.ow2.asm:asm:9.7","downloads":{"artifact":{"path":"b.jar"}}},{"name":"com.google.guava:guava:33.0.0","downloads":{"artifact":{"path":"g.jar"}}}]}"#,
        );

        let merged = merge(&child, &parent);

        // Different versions of the same artifact are both kept, child first:
        // the classpath order is what makes the loader's pin win.
        assert_eq!(merged.libraries.len(), 3);
        assert_eq!(merged.libraries[0].name, "org.ow2.asm:asm:9.7");
        assert_eq!(merged.libraries[1].name, "com.google.guava:guava:33.0.0");
        assert_eq!(merged.libraries[2].name, "org.ow2.asm:asm:9.6");
    }
}
