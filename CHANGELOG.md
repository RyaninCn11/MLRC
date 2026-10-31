# Changelog

User-visible changes, newest first. The release workflow reads the section that matches
the tag (`v1.1.0` reads `## [1.1.0]`) and uses it as the release body, so a tag without
a section fails the build rather than publishing empty notes.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- Minecraft Unified Pass (Nide8) login: `mlrc auth login --nide8 <32-hex server id>`.
  The server document supplies the real API root, and the game is started with the
  server's `nide8auth.jar` and the two arguments that component requires.
  `mlrc auth register` opens the server's registration page. A missing `nide8auth.jar`
  is offered for download from the service and verified against the server's published
  hash before use, and each launch re-checks it.
- Resource packs, shader packs and data packs: `mlrc pack search|install|list|remove`.
  The kind is read from Modrinth instead of demanded as a flag, and `--world` places a
  data pack inside one world. When a project publishes both a mod build and a plain one
  for the same game version (Terralith does), the build that names no mod loader is the
  one a pack folder gets.
- Launch settings, global and per fork: `launch.brand`, `launch.jvm_args`,
  `launch.game_args` and `launch.pre_launch`. `mlrc instance create` sets them with
  `--brand`, `--jvm-arg`, `--game-arg` and `--pre-launch`, and an empty field inherits
  the global value.
- `${...}` markers (`${game_directory}`, `${version_name}`, `${library_directory}`, ...)
  are expanded in the launcher's own arguments, not only in a version document.
- A pre-launch command that fails stops the launch with `E_PRE_LAUNCH` instead of
  starting the game in a state the command was meant to prepare.

### Fixed

- `mlrc config set` and `--jvm-arg` rejected any value that started with `-`, which
  made JVM flags impossible to type.
- The `doctor` warning about a read-only JVM now names a restricted token, because that
  is what it can detect; blaming antivirus first sent two investigations the wrong way.

## [1.0.0] - 2026-10-31

First release.

### Added

- Installing any vanilla version, one per command, with SHA1 verification and
  `.minecraft` + `MLRC` next to the executable as the whole storage model.
- Managed Java runtimes, one per required major, so a system JDK upgrade cannot break an
  installed version.
- Fabric, Quilt, Forge and NeoForge installs, the last two through their own installer.
- Forks: a version that inherits another and keeps its own worlds, options and mods.
- Mods from Modrinth and CurseForge, with required dependencies, updates, and
  enable/disable that renames rather than deletes.
- Modpack import for `.mrpack` and CurseForge `.zip`.
- Offline play under any name, and Yggdrasil login for third-party authentication
  servers, with authlib-injector fetched and verified on demand.
- `mlrc install --list`, filterable by loader and by category, including April Fools
  builds as their own category.
- Machine-readable output: stable exit codes, stable error codes, and NDJSON that
  streams the engine's progress events.
- `mlrc doctor` for installations that misbehave.
- A documented API contract (`docs/API.md`) for a front-end that wants to drive the
  engine without the CLI.

### Notes

- The layout is portable only: data lives beside the binary, and an unwritable folder
  stops the launcher with `E_PORTABLE_READONLY` rather than relocating.
- Microsoft account login is not implemented.

[Unreleased]: https://github.com/RyaninCn11/MLRC/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/RyaninCn11/MLRC/releases/tag/v1.0.0
