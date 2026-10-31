//! What kind of content is being installed, and where it goes.
//!
//! Four kinds share one pipeline: a project, a file, a hash, a directory. Only the
//! directory and the source's own type name differ, which is why this is an enum and
//! not four code paths.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mod,
    ResourcePack,
    Shader,
    DataPack,
}

impl Kind {
    pub fn parse(text: &str) -> Option<Self> {
        let normalised: String = text
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .collect();

        match normalised.as_str() {
            "mod" | "mods" => Some(Self::Mod),
            "resourcepack" | "resourcepacks" | "resource" => Some(Self::ResourcePack),
            "shader" | "shaders" | "shaderpack" | "shaderpacks" => Some(Self::Shader),
            "datapack" | "datapacks" | "data" => Some(Self::DataPack),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mod => "mod",
            Self::ResourcePack => "resourcepack",
            Self::Shader => "shader",
            Self::DataPack => "datapack",
        }
    }

    /// Modrinth's `project_type` value, which happens to match `as_str`.
    pub const fn project_type(self) -> &'static str {
        self.as_str()
    }

    /// The folder inside the game directory.
    pub const fn folder(self) -> &'static str {
        match self {
            Self::Mod => "mods",
            Self::ResourcePack => "resourcepacks",
            Self::Shader => "shaderpacks",
            Self::DataPack => "datapacks",
        }
    }

    /// Where a file of this kind belongs.
    ///
    /// Datapacks are the one kind with two homes: the game has no global folder for
    /// them, so without a world name they land in `<game>/datapacks/` (which mods such
    /// as Paxi read) and with one they land inside that world, where vanilla looks.
    pub fn dir(self, game_dir: &Path, world: Option<&str>) -> PathBuf {
        match (self, world) {
            (Self::DataPack, Some(world)) => game_dir.join("saves").join(world).join("datapacks"),
            _ => game_dir.join(self.folder()),
        }
    }

    /// Only mods are swapped by a loader at startup; the rest are read by the game.
    pub const fn is_toggleable(self) -> bool {
        matches!(self, Self::Mod)
    }
}

/// The kinds this launcher installs.
pub const ALL: [Kind; 4] = [Kind::Mod, Kind::ResourcePack, Kind::Shader, Kind::DataPack];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_forgiving_but_finite() {
        assert_eq!(Kind::parse("mod"), Some(Kind::Mod));
        assert_eq!(Kind::parse("Resource-Pack"), Some(Kind::ResourcePack));
        assert_eq!(Kind::parse("shaderpacks"), Some(Kind::Shader));
        assert_eq!(Kind::parse("data_pack"), Some(Kind::DataPack));
        assert_eq!(Kind::parse("plugin"), None);
    }

    #[test]
    fn each_kind_has_one_folder() {
        let game = Path::new("C:/game");
        assert_eq!(Kind::Mod.dir(game, None), game.join("mods"));
        assert_eq!(
            Kind::ResourcePack.dir(game, None),
            game.join("resourcepacks")
        );
        assert_eq!(Kind::Shader.dir(game, None), game.join("shaderpacks"));
        assert_eq!(Kind::DataPack.dir(game, None), game.join("datapacks"));
    }

    #[test]
    fn a_datapack_can_belong_to_a_world() {
        let game = Path::new("C:/game");
        assert_eq!(
            Kind::DataPack.dir(game, Some("survival")),
            game.join("saves/survival/datapacks")
        );
        // A world name means nothing to the other kinds.
        assert_eq!(
            Kind::Shader.dir(game, Some("survival")),
            game.join("shaderpacks")
        );
    }
}
