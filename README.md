# MLRC - Minecraft Launcher of Rust Command-line

MLRC installs, configures and starts Minecraft: Java Edition from the command line.
It fetches version metadata from Mojang, downloads only what that version needs,
provisions the Java runtime the version declares, and launches the game.

Everything lives next to the executable, so the installation is a folder you can move
or copy. The launcher is split into an engine (`mlrc-core`) and a CLI (`mlrc-cli`), so
other tools can reuse the engine without touching it.

## Features

- [x] Install any vanilla version, one per command, with SHA1-verified downloads from
  Mojang or BMCLAPI
- [x] Managed Java runtimes, one per major, so `JAVA_HOME` never breaks a version
- [x] Portable: one folder holds everything, and nothing is written outside it
- [x] Fabric, Quilt, Forge and NeoForge, installed through the same command
- [x] Instances: a copy of a version that keeps its own worlds and mods
- [x] Mods and modpacks from Modrinth and CurseForge
- [x] Offline play under any name, or Yggdrasil login for third-party servers
- [x] Machine-readable output: exit codes, error codes, NDJSON events
- [x] `mlrc doctor` for when something is wrong
- [ ] Microsoft account login

## Requirements

- Windows, macOS or Linux
- No Java installation required - MLRC downloads and manages its own runtimes and
  ignores `JAVA_HOME` and `PATH` unless configured otherwise
- Disk: roughly 100-200 MB per Java runtime, 50-600 MB per Minecraft version

## Install

Download an archive from the releases page and unpack it; the launcher is a single
binary. From source:

```bash
cargo build --release
./target/release/mlrc --version
```

## Usage

```bash
mlrc install 1.18.2        # asks before installing the Java 17 runtime
mlrc launch 1.18.2
mlrc use 1.18.2            # remember it for a bare `mlrc launch`
```

Not sure which version to pick? `--list` prints them, newest first, and asks how many
to show; with a loader, only the versions that loader has a build for:

```bash
mlrc install --list                       # asks: show the first (10/921) - or `all`
mlrc install --list --limit 0             # everything, no question
mlrc install --list --type release        # releases only
mlrc install --list --type april_fools    # the joke builds, 9 of them
mlrc install --list --loader neoforge --type release
```

Categories are `release`, `snapshot`, `old_beta`, `old_alpha` and `april_fools`; the
last one is derived (a snapshot released on 1 April, plus the one that shipped on 31
March), because Mojang's metadata has no marker for it.

**Modded.** Loaders are installed by the same command, and the id it prints is what
you launch:

```bash
mlrc install 1.21.1 --loader fabric       # or quilt
mlrc install 1.21.1 --loader neoforge     # or forge
mlrc launch fabric-loader-0.20.1-1.21.1
```

**A second world set.** Every version has its own folder, so an instance is what you want:

```bash
mlrc instance create survival --version fabric-loader-0.20.1-1.21.1 --memory 4G
mlrc launch --instance survival
```

**Mods.** Modrinth by default; required dependencies come with them.

```bash
mlrc mod install iris --instance survival      # also installs Sodium
mlrc mod list --instance survival
mlrc mod update --instance survival --check
mlrc mod disable lithium --instance survival    # renamed, not deleted
```

**Content beyond mods.** Resource packs, shaders and data packs install the same way,
into `resourcepacks/`, `shaderpacks/` and `datapacks/`:

```bash
mlrc pack search faithful --kind resourcepack --mc 1.21.1
mlrc pack install faithful-32x --instance survival
mlrc pack install complementary-reimagined --instance survival   # a shader
mlrc pack install terralith --kind datapack --instance survival --world MyWorld
mlrc pack list --instance survival
```

**Modpacks.** `instance import` installs the loader a pack pins, its overrides and its
mods. Modrinth `.mrpack` and CurseForge `.zip` both work:

```bash
mlrc instance import Fabulously.Optimized.mrpack
mlrc launch --instance Fabulously-Optimized
```

**CurseForge** is a second source. It needs your own key and answers by project id:

```bash
mlrc config set curseforge.api_key <key>      # or export MLRC_CURSEFORGE_KEY
mlrc mod install 238222 --source curseforge    # Just Enough Items
```

**Identity.** Offline by default, under the name you pick:

```bash
mlrc config set auth.player_name Steve        # or `mlrc launch --username Steve`
```

Or sign in to a third-party Yggdrasil server, which adds skins and access to that
service's online-mode servers. The password is asked for and never stored:

```bash
mlrc auth login --server https://littleskin.cn/api/yggdrasil --username you@example.com
mlrc auth accounts                                        # list, * marks the default
mlrc auth use <label>                                     # choose the default account
mlrc auth status
```

**Tuning.** Heap, language and download source, per run or remembered:

```bash
mlrc launch --memory 6G
mlrc config set java.xmx 6G
mlrc launch --lang zh_cn
mlrc config set general.game_language zh_cn
mlrc --mirror bmclapi install 1.12.2           # auto probes both and remembers
```

**Launch settings.** JVM flags, game arguments, a command to run first, and the brand
text the game shows in its corner - globally or per instance. `${...}` markers work here
exactly as they do in a version document:

```bash
mlrc config set launch.jvm_args "-XX:+UseG1GC -Dhere=${game_directory}"
mlrc config set launch.game_args "--demo"
mlrc config set launch.pre_launch "echo starting"
mlrc config set launch.brand "My Launcher"
mlrc instance create demo --version 1.21.1 --game-arg --demo --brand Demo
mlrc config set launch.jvm_args "-XX:+UseG1GC" --instance demo   # that instance only
mlrc config list --instance demo                                  # [instance] or [global], per key
```
### Commands

| Command | Purpose |
|---|---|
| `mlrc install <version>` | Install a version; `--list` lists what can be installed (`--type`, `--limit`), `--loader fabric\|quilt\|forge\|neoforge`, `--java=auto\|install\|skip`, `--dry-run`, `--threads N`, `--yes` |
| `mlrc launch [<version>]` | Start the game; `--instance <name>`, `--memory 4G`, `--jvm-arg <arg>`, `--lang <code>`, `--dry-run`, `--username`, `--width`/`--height`, `-- <args>` |
| `mlrc list [--all]` | Installed versions, or every version in the manifest |
| `mlrc use <version>` | Set the default version |
| `mlrc uninstall <version>` | Remove a version; the shared cache is kept |
| `mlrc instance create \| list \| remove` | Instances: a version with its own worlds and mods, sharing the parent's files (`--memory`, `--jvm-arg`, `--game-arg`, `--pre-launch`, `--brand`) |
| `mlrc mod search \| install \| list \| update \| remove \| enable \| disable` | Mods from Modrinth (or CurseForge with `--source curseforge`), filtered by the version's loader and Minecraft version; installs bring their required dependencies, `--dry-run` shows them first |
| `mlrc pack search \| install \| list \| remove` | Resource packs, shader packs and data packs; the kind is read from Modrinth, `--world` puts a data pack inside one world |
| `mlrc instance import <pack>` | Install a modpack (`.mrpack` or CurseForge `.zip`) as a new instance; `--dry-run` shows its contents first |
| `mlrc java list \| install \| remove` | Manage Java runtimes by major version or component |
| `mlrc auth login \| logout \| status` | Sign in to a Yggdrasil server, forget it, or show it |
| `mlrc config get \| set \| list` | Read or write `config.toml`; `--instance <name>` reads or writes an instance's layer (`launch.*` only) |
| `mlrc cache prune [--keep-days N]` | Drop cache objects nothing references |
| `mlrc doctor` | Check the data directory, network, runtimes and versions |

Global flags: `--data-dir`, `--json`, `--quiet`, `-v`, `--mirror`.

### Configuration

`MLRC/config.toml`:

```toml
[java]
custom = false      # true: honour java.path, JAVA_HOME and PATH
path = ""           # explicit java binary
xms = ""            # initial heap; empty leaves the JVM default
xmx = "2G"          # maximum heap; --memory and an instance's own value override it

[general]
default_version = ""
mirror = "auto"     # auto | official | bmclapi
proxy = "auto"      # auto | none | host:port; also passed to the game's JVM
game_language = ""  # e.g. zh_cn

[launch]
brand = ""               # corner text instead of MLRC; empty restores it
jvm_args = []            # e.g. ["-XX:+UseG1GC"]
game_args = []           # e.g. ["--demo"]
pre_launch = ""          # shell command, run in the game directory

[auth]
kind = "auto"            # auto | offline | yggdrasil | nide8
server = ""              # Yggdrasil API root; empty means offline only
player_name = "Player"   # offline name
```

An instance may override any of the `launch` values; an empty field inherits the global one.

Exit codes are stable: `0` success, `2` usage, `3` network, `4` authentication,
`5` Java, `7` portable directory not writable, `8` version not installed. `--json`
emits NDJSON events carrying the same error codes.

## How it works

- **Portable by default.** Everything lives next to the executable: `.minecraft` for
  the standard layout, `MLRC` for the launcher's own data. If the folder is not
  writable the launcher fails with `E_PORTABLE_READONLY` instead of relocating into the
  user profile.
- **Hashes are the authority.** Every file is checked against the hash its source publishes
  (SHA1 from Mojang, SHA512 from Modrinth, SHA256 from the injector API), or against its
  length when a source publishes no hash. A mirror is only a URL substitution; a file it
  does not carry falls back to the official URL.
- **Java is pinned per version.** Runtimes live under `MLRC/java/jdk-<major>/` and the
  host JDK is ignored unless `java.custom=true`, so a system JDK upgrade cannot break an
  installed version.
- **An engine and a CLI.** `mlrc-core` never prints and emits events; the CLI renders
  them as text or NDJSON.

## Project layout

```
crates/
|-- mlrc-core/    engine: metadata, rules, downloads, store, java, launch
`-- mlrc-cli/     binary: argument parsing, rendering, prompts, exit codes
```

[docs/DOCS.md](docs/DOCS.md) is the design and the reasoning behind it,
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) covers how the code is organised, and
[docs/API.md](docs/API.md) is the contract a front-end may depend on.
[CHANGELOG.md](CHANGELOG.md) records what changed in each release; the release
workflow publishes the matching section as the release notes.

## Mirrors and attribution

The mirror support targets [BMCLAPI](https://bmclapi2.bangbang93.com). Its terms ask
launchers to name the source in their download interface, so MLRC prints
`download source: BMCLAPI (...)` and emits a `source` event when a download goes
through it.

## Legal

Not affiliated with Mojang or Microsoft. MLRC bundles no game files; it downloads them
only when asked, and having a legitimate copy of Minecraft is still required to play
on servers that verify ownership.

## License

Apache-2.0, see [LICENSE](LICENSE).
