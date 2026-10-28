# MLRC - Minecraft Launcher of Rust Command-line

MLRC installs, configures and starts Minecraft: Java Edition from the command line.
It fetches version metadata from Mojang, downloads only what that version needs,
provisions the Java runtime the version declares, and launches the game.

Everything lives next to the executable, so the installation is a folder you can move
or copy. The launcher is split into an engine (`mlrc-core`) and a CLI (`mlrc-cli`), so
a graphical interface can reuse the same engine without touching it.

## Features

- [x] Install any vanilla version, one version per command
- [x] Managed Java runtimes, one per required major
- [x] Portable layout: no writes outside the distribution folder
- [x] Content-addressed cache with hardlinks, so versions share jars and assets
- [x] SHA1 verification of every downloaded file
- [x] Official Mojang sources and BMCLAPI, with per-file fallback
- [x] Launch the game offline with a derived player identity
- [x] Machine-readable output: stable exit codes, error codes, NDJSON events
- [x] Install health checks (`mlrc doctor`)
- [x] Fabric, Forge and NeoForge loaders: `mlrc install <mc> --loader fabric|forge|neoforge`
- [x] Named instances, each with its own game directory
- [x] Set the in-game language before launch (`--lang zh_cn`)
- [ ] Microsoft account login
- [ ] Quilt loader
- [ ] Graphical front-end

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
mlrc install 1.18.2          # asks before installing the Java 17 runtime
mlrc launch 1.18.2           # starts the game
```

```bash
mlrc install 26.3 --java=install --threads 16
mlrc use 26.3                # remember the default version
mlrc launch                  # uses the default
```

Slow connection to Mojang? Use a mirror:

```bash
mlrc --mirror bmclapi install 1.12.2
```

`--mirror auto` (the default) times both sources once and remembers the faster one.

Modded, or want separate worlds? Every version shares one game directory by default;
an instance gives you its own:

```bash
mlrc install 1.21.1 --loader fabric
mlrc instance create survival --version fabric-loader-0.20.1-1.21.1
mlrc launch --instance survival
```

Forge and NeoForge are installed by running their own installer, then launched like
any other version:

```bash
mlrc install 1.21.1 --loader forge
mlrc launch 1.21.1-forge-52.1.20

mlrc install 1.21.1 --loader neoforge
mlrc launch neoforge-21.1.265
```

Want the game itself in another language? Minecraft has no launch switch for it, so
MLRC writes the line into `options.txt` before starting:

```bash
mlrc launch --lang zh_cn
mlrc config set gameLanguage zh_cn      # every launch
```

Modded packs need more than the 2G default heap:

```bash
mlrc launch 1.21.1-forge-52.1.20 --memory 6G
mlrc config set java.xmx 6G             # every launch
mlrc instance create big --version neoforge-21.1.265 --memory 8G
```

### Commands

| Command | Purpose |
|---|---|
| `mlrc install <version>` | Install a version; `--loader fabric\|forge\|neoforge`, `--java=auto\|install\|skip`, `--dry-run`, `--threads N`, `--yes` |
| `mlrc launch [<version>]` | Start the game; `--instance <name>`, `--memory 4G`, `--jvm-arg <arg>`, `--lang <code>`, `--dry-run`, `--username`, `--width`/`--height`, `-- <args>` |
| `mlrc list [--all]` | Installed versions, or every version in the manifest |
| `mlrc use <version>` | Set the default version |
| `mlrc uninstall <version>` | Remove a version; the shared cache is kept |
| `mlrc instance create \| list \| remove` | Named game directories bound to a version (`--memory` per instance) |
| `mlrc java list \| install \| remove` | Manage Java runtimes by major version or component |
| `mlrc config get \| set \| list` | Read or write `config.toml` |
| `mlrc cache prune [--keep-days N]` | Drop cache objects nothing references |
| `mlrc doctor` | Check the data directory, network, runtimes and versions |

Global flags: `--data-dir`, `--json`, `--quiet`, `-v`, `--mirror`.

### Configuration

`data/config.toml`:

```toml
[java]
custom = false      # true: use a host JDK instead of managed runtimes
path = ""           # explicit java binary
xms = ""            # initial heap, e.g. 1G; empty leaves Java's default
xmx = "2G"          # maximum heap; a --memory flag or an instance overrides it

[general]
default_version = ""
mirror = "auto"     # auto | official | bmclapi
game_language = ""  # e.g. zh_cn; empty leaves the game's own setting
```

Exit codes are stable: `0` success, `2` usage, `3` network, `4` authentication,
`5` Java, `7` portable directory not writable, `8` version not installed. `--json`
emits NDJSON events carrying the same error codes.

## How it works

- **Portable by default.** Data lives in `data/` next to the executable. If that
  folder is not writable the launcher fails with `E_PORTABLE_READONLY` instead of
  relocating into the user profile.
- **Hashes are the authority.** Every file is checked against the SHA1 from Mojang's
  metadata. A mirror is only a URL substitution; a file it does not carry falls back
  to the official URL.
- **Java is pinned per version.** Runtimes are stored under `data/java/jdk-<major>/`
  and the host JDK is ignored unless `customJava=true`, so a system JDK upgrade cannot
  break an installed version.
- **One engine, two front-ends.** `mlrc-core` never prints and emits events; the CLI
  renders them as text or NDJSON.

## Project layout

```
crates/
|-- mlrc-core/    engine: metadata, rules, downloads, store, java, launch
`-- mlrc-cli/     binary: argument parsing, rendering, prompts, exit codes
```

[DOCS.md](DOCS.md) documents the design and the reasoning behind it.

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
