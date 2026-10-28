//! Mojang metadata: the manifest, version documents, rules and inheritance.

pub mod asset;
pub mod inherit;
pub mod manifest;
pub mod rule;
pub mod version;

pub const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
