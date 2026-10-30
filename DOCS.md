# MLRC - design notes

MLRC is a command-line launcher for Minecraft: Java Edition, written in Rust. It
installs a version, provisions the Java runtime that version declares, and starts the
game. The engine is a library with no terminal dependencies, so a graphical front-end
can link it without reimplementing anything.

This document describes how the launcher is built and why. It is a design document:
it records decisions and their reasoning, not schedules or progress.

---

## 1. Scope

### In scope

- Installing vanilla versions from Mojang's own metadata, one version per command.
- Managing the Java runtimes those versions need.
- Launching the game, and the identity backends the launcher supports.
- A machine-readable interface: stable exit codes, stable error codes, NDJSON events.

### Out of scope

- Bundling or redistributing any Minecraft file. Everything is fetched on demand.
- A GUI inside this repository. The engine is shaped so one can be added without
  touching it.
- Publishing or hosting mods; MLRC only installs from Modrinth and CurseForge.
- Skin services, realm browsing, chat, or anything else the game itself does.

---

## 2. Constraints

| # | Constraint | Consequence |
|---|---|---|
| C1 | English only in code and output. No translated strings in the engine. | Sources are ASCII-only and CI-checked. Localization, if any, belongs to a front-end. |
| C2 | Portable by default. Nothing is written outside the distribution folder. | Moving the folder moves the installation. Silent relocation is forbidden. |
| C3 | The CLI consumes the engine; it is not the engine. | `mlrc-core` has no `clap`, no `indicatif`, and never prints. |
| C4 | Every result is machine-readable. Human text is a rendering. | Exit codes, error codes and an event stream are the contract. |
| C5 | One version per install, nvm-style. | The unit of work is a version, not a manifest. No batch mode. |
| C6 | The host Java is never used unless explicitly enabled. | One managed runtime per major, immune to `JAVA_HOME` drift. |

C6 deserves its rationale: a machine has one `JAVA_HOME` and one `PATH`, while
Minecraft versions span Java 8 through 25. Borrowing the host JVM means an upgrade to
the system JDK silently breaks whatever version needed the old one. MLRC vendors its
own runtimes and leaves the host alone; `customJava=true` opts back in.

---

## 3. Metadata facts

| Item | Value |
|---|---|
| Manifest | `https://piston-meta.mojang.com/mc/game/version_manifest_v2.json` |
| Version document | `https://piston-meta.mojang.com/v1/packages/<sha1>/<id>.json` |
| Client jar | `https://piston-data.mojang.com/v1/objects/<sha1>/client.jar` |
| Libraries | `https://libraries.minecraft.net/<maven path>` |
| Asset index | `https://piston-meta.mojang.com/v1/packages/<sha1>/<index id>.json` |
| Asset object | `https://resources.download.minecraft.net/<first2>/<full sha1>` |
| Java runtime index | `https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json` |
| Java for 26.3 | `java-runtime-epsilon`, major 25 |
| Java for 1.18.2 | `java-runtime-beta`, major 17 |

Four properties of this data shape the code:

1. **Version ids are not sortable.** `26.3`, `1.21.11`, `26w14a`, `1.7.10`, `b1.7.3`
   all coexist. Never parse or compare them; order comes from `releaseTime`.
2. **The asset URL repeats its prefix.** The path is `<first two>/<full hash>`, not
   `<first two>/<rest of hash>`. Stripping the prefix 404s every object.
3. **`arguments.default-user-jvm` exists** since 2025. It applies when the user has
   not set JVM flags, and its first entry has no `rules` at all. The key is
   kebab-case, so a `rename_all = "camelCase"` model silently reads it as empty.
4. **Natives have two generations.** Libraries carrying `natives`/`extract` (up to
   1.18) must be unpacked into `natives/`; 1.19+ puts native jars on the classpath and
   lets LWJGL extract them. Decide from the library entry, never from the version id.

---

## 4. Architecture

```
mlrc/
|-- Cargo.toml              [workspace]
|-- crates/
|   |-- mlrc-core/          engine: no clap, no printing, no terminal assumptions
|   |   `-- src/
|   |       |-- meta/       manifest, version document, rules, inheritance  (pure)
|   |       |-- plan/       install plan                                   (pure)
|   |       |-- launch/     argument assembly, natives, process spawn      (pure + std)
|   |       |-- fetch/      http client, download queue, mirrors
|   |       |-- store/      content-addressed store, hardlink materialization
|   |       |-- java/       managed runtimes: discover, validate, install
|   |       |-- auth/       identities: offline derivation, Microsoft account
|   |       |-- port/       portable root, config, per-version state
|   |       `-- events.rs   Event enum + EventSink trait
|   `-- mlrc-cli/           binary "mlrc": clap, rendering, prompts, exit codes
```

Dependency direction is enforced by crate boundaries:

```
mlrc-cli -> mlrc-core -> (bytes-level deps only: serde, reqwest, tokio, sha1, zip)
```

A graphical front-end links `mlrc-core` directly, never the CLI. That is why the
engine has no rendering dependencies and never writes to stdout.

### 4.1 Events

The engine reports progress and decisions through a sink. Rendering, and any
interaction, belong to the caller.

```rust
pub enum Event {
    Phase { phase: String, total: Option<u64> },
    FileStarted { path: String, total_bytes: Option<u64> },
    FileProgress { path: String, bytes: u64 },
    FileFinished { path: String, ok: bool, cached: bool },
    JavaRequired { component: String, major: u32, download_bytes: u64 },
    Warning { code: String, message: String },
    Done { message: String, ok: bool },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}
```

`JavaRequired` is a question, not a prompt: the CLI renders `[Y/n]`, a GUI renders a
dialog, and `--json` renders a line plus exit code 5. The engine never reads stdin.

---

## 5. On-disk layout

Base directory resolution order:

1. `--data-dir <path>`
2. `MLRC_DATA_DIR`
3. the directory containing the executable

The base contains two folders, split by who reads them. `.minecraft` is the standard
launcher directory, so other tools and the game itself recognise it; `MLRC` is
launcher-private and safe to delete when the cache is worthless.

The executable directory is resolved with `current_exe()`. If it is not writable, the
launcher fails with `E_PORTABLE_READONLY`; it never falls back to a per-user directory
behind the user's back.

```
<dist>/
|-- mlrc.exe
|-- .minecraft/
|   |-- launcher_profiles.json          minimal stub; loader installers require it
|   |-- assets/
|   |   |-- indexes/<id>.json
|   |   `-- objects/<aa>/<full sha1>
|   |-- libraries/<maven path>          one tree, shared by every version
|   `-- versions/<id>/
|       |-- <id>.json                   version document, standard location
|       |-- <id>.jar                    client jar; a fork hardlinks its parent's
|       |-- <id>-natives/{java,jna,lwjgl,netty}
|       |-- MLRC/                       launcher-private, per version
|       |   |-- installed.json          install state
|       |   |-- objects.txt             hashes this version references
|       |   `-- instance.json           only for forks
|       |-- saves/  mods/  resourcepacks/  shaderpacks/  config/  logs/
|       `-- options.txt  servers.dat    the game directory is this same folder
`-- MLRC/
    |-- config.toml
    |-- cache/objects/<aa>/<full sha1>  one copy of every file, hardlinked into place
    |-- java/jdk-<major>/               managed runtimes, keyed by major
    |-- meta/                           manifest and runtime index caches
    |-- logs/                           installer output and the launcher's own log
    `-- tmp/                            `-Djava.io.tmpdir` for the game
```

Notes:

- The version directory doubles as its game directory: isolation by construction, and
  deleting a version deletes exactly what belonged to it.
- `MLRC/cache/objects` and `.minecraft` must share a volume for hardlinks; otherwise
  the store copies and reports a warning.
- `cache prune` trusts `versions/*/MLRC/objects.txt`, so the state file lives with the
  version instead of in a central directory.
- A fork is a version directory holding a two-line document (`inheritsFrom`), a
  hardlinked jar and `MLRC/instance.json`; nothing is copied from the parent.
- Nothing is written outside the base directory.

---

## 6. Install

### 6.1 Flow

```
1. load the manifest (cached, revalidated by age)
2. fetch the version document (cached by URL; it is content-addressed)
3. resolve inheritance, if the document declares a parent
4. evaluate library rules for this machine
5. read javaVersion -> { component, major }
6. resolve Java: managed runtime, host runtime when allowed, or provision one
7. build the file list: client jar, libraries, natives, logging config, asset index
8. download into cache/objects with SHA1 verification, materialize by hardlink
9. read the asset index, download every object, then write installed.json
```

The install is idempotent: a complete version re-checks sizes and returns without
network work beyond the manifest.

### 6.2 The Java gate

```
$ mlrc install 1.18.2
resolving 1.18.2 ... ok
this version requires Java 17 (java-runtime-beta)
java 17 is not installed. download it now? [Y/n]
```

| Mode | Behaviour |
|---|---|
| interactive terminal, no flags | prompt, default yes |
| `--java=install` | no prompt, provision silently |
| `--java=skip` | continue without Java; the version installs, launch fails later |
| `--yes` | accept every prompt |
| `--json` or non-interactive | never prompt: emit `java_required`, exit 5 |

Exit code 5 is a request for a decision, not a failure: the NDJSON line carries
`component` and `major`, so a front-end can ask its own question and re-run with
`--java=install`.

### 6.3 State, removal, cache

`versions/<id>/MLRC/installed.json` records completeness, the Java component and major, and
the file count and size. A version with `complete: false` is repaired by re-running
install and refused by launch.

`objects.txt` lists every hash the version pulled. `cache prune` keeps the union of all
such lists (plus managed runtimes) and drops the rest. A version without an object list
blocks pruning with `E_CACHE_STALE` rather than letting the cache be emptied.

`mlrc uninstall <id>` removes the version directory only, and clears the default
version if it pointed there.

### 6.4 Mirrors

A mirror is URL substitution. The official hash stays authoritative, so a slow, stale
or hostile mirror cannot corrupt an install.

| Official | BMCLAPI |
|---|---|
| `piston-meta.mojang.com` | `bmclapi2.bangbang93.com` |
| `piston-data.mojang.com` | `bmclapi2.bangbang93.com` |
| `launchermeta.mojang.com` | `bmclapi2.bangbang93.com` |
| `libraries.minecraft.net` | `bmclapi2.bangbang93.com/maven` |
| `resources.download.minecraft.net` | `bmclapi2.bangbang93.com/assets` |

Observed behaviour of BMCLAPI:

- Requests answer with a 302 to a node, so redirects must be followed.
- Coverage is incomplete: client jars redirect back to Mojang, and some small objects
  return 404. Each file falls back to the official URL individually.
- Maven and asset paths are served directly, which is where the speedup comes from.

`--mirror auto|official|bmclapi`. `auto` times the manifest request against both hosts
and stores the winner in `config.toml`. BMCLAPI's terms require naming the source in
the download UI, so a run through it prints the source and emits a `source` event.

### 6.5 Proxy

`general.proxy` is `auto` (environment variables, then the Windows internet settings),
`none` (always direct) or an explicit `host:port`.

It applies to both halves of the launcher because they fail separately:

- The HTTP client. reqwest already honours the system proxy, which is right until the
  proxy is running but not forwarding: then every request fails while a direct
  connection would have worked, and nothing in the error says why.
- Child JVMs. A JVM knows nothing about the system proxy; loader installers do their
  own downloads and the game talks to Mojang, so both get `-Dhttp(s).proxyHost` from
  the same setting. `none` therefore also means the game ignores the system proxy.

`mlrc doctor` reports the network, which is what makes a misconfigured proxy obvious
rather than mysterious.

---

## 7. Java runtimes

`javaVersion.majorVersion` drives everything. Versions without a `javaVersion` block
are rejected with `E_JAVA_META_MISSING` rather than guessed at; the block is present in
almost the whole back catalogue, missing only in a few very old versions.

Runtime on disk: `MLRC/java/jdk-<major>/`, keyed by major rather than by Mojang's
component name, because two components can share a major (`java-runtime-gamma` and
`java-runtime-gamma-snapshot` are both 17). Provenance is recorded in
`jdk-<major>/mlrc.json`.

Selection order:

1. managed runtime with the required major
2. host runtime, only when `customJava=true` (explicit `java.path`, then `JAVA_HOME`,
   then `PATH`), and only when its major is exactly the required one
3. otherwise: prompt, provision, or fail, per the install gate

Validation reads `<home>/release` (`JAVA_VERSION="17.0.15"`) instead of running
`java -version`, whose output is localized and requires a process. Parsing accepts
both `1.8.0_51` and `8u51-cacert462b08` forms.

Installation downloads Mojang's per-platform manifest into `jdk-<major>.part/` and
renames it into place, so an interrupted download never leaves a half-JDK. Executable
bits are applied on Unix.

---

## 8. Launch

### 8.1 Argument assembly

```
[java] + [default-user-jvm?] + [jvm arguments] + [logging] + [user flags]
       + [main class] + [game arguments]
```

- `default-user-jvm` is applied only when the user supplied no JVM flags; its
  `-Xms`/`-Xmx` stay as shipped.
- Documents without `arguments.jvm` (pre-1.13) get the classic
  `-Djava.library.path=<natives>` and `-cp <classpath>` synthesized.
- Documents without `arguments.game` fall back to `minecraftArguments`, split on
  whitespace.
- A missing `-Xmx` gets a `-Xmx2G` default.
- Heap precedence is `--memory` > the instance's `memory` > `java.xmx`, with the 2G
  default last; `java.xms` adds `-Xms` when configured. A requested value replaces
  whatever `default-user-jvm` carried: a modded pack that keeps the 2G default dies
  with an out-of-memory error instead of anything a user can act on.
- `--jvm-arg` appends a flag verbatim, for the `-XX:` tuning that no config key covers.
- `-Djava.io.tmpdir` points at `MLRC/tmp`: loader remapping and image caches then stay
  inside the portable tree instead of the system temp directory.
- Variables are `${name}` placeholders; unknown ones are left visible rather than
  silently emptied. `library_directory` and `classpath_separator` are provided because
  Forge and NeoForge build their module path out of them - without them the literal
  `${library_directory}` reaches the JVM and the bootstrap cannot load its modules.
- When the machine needs a proxy, the game JVM gets `-Dhttp(s).proxy*` too; otherwise
  it connects directly while the launcher's own downloads go through the proxy.

Feature flags (`is_demo_user`, `has_custom_resolution`, quick-play variants) are set by
the launcher before rule evaluation, never inferred from the version.

### 8.2 Natives

Old versions ship native jars the launcher must unpack; since 1.19 they ride on the
classpath and LWJGL unpacks what it needs itself, so an empty set is normal.

The layout follows the launcher we imitate: one root per version with four areas, each
native family in its own so two libraries cannot clobber each other's files.

```
versions/<id>/<id>-natives/
|-- java/     launcher-extracted natives; `-Djava.library.path` points here
|-- jna/      `-Djna.tmpdir`
|-- lwjgl/    `-Dorg.lwjgl.system.SharedLibraryExtractPath`
`-- netty/    `-Dio.netty.native.workdir`
```

Mojang's documents point all four properties at one directory; the launcher rewrites
the three that name a family so each has its own, which is what the launcher this
project imitates does.

### 8.3 Process

The game is spawned with inherited stdio and the game directory as working directory.
Output is not piped: a pipe would lose colour, add buffering, and force decoding of a
console stream that mixes UTF-8 with the legacy code page. The game's exit code is
returned to the caller unchanged.

### 8.4 Identity

Offline identity derives the UUID as MD5 of `OfflinePlayer:<name>` with the RFC 4122
version-3 and variant bits applied. The access token is a placeholder; online-mode
servers reject it, which is the point. `--uuid` is passed without dashes.

### 8.5 Game directory and forks

The game directory is the mutable part of a setup: saves, options, resource packs and
mods. Here it is the version's own directory, `versions/<id>/`, which is the isolation
model of the launcher this project imitates: one version, one set of worlds.

A second set of worlds for the same Minecraft version is a **fork**, a version that
inherits another one:

```
mlrc instance create survival --version 1.21.1
mlrc launch --instance survival
```

Creating one copies nothing. The fork is a two-line version document with
`inheritsFrom`, a client jar hardlinked from its parent, and `MLRC/instance.json`; the
library tree, assets and the parent's files stay shared. Removing a fork deletes its
directory and nothing else.

Because a fork is a real version id, `mlrc list` shows it, `mlrc uninstall` removes it,
and every version-facing command (`mod`, `launch <id>`) accepts it without knowing what
a fork is.

### 8.6 Game language

Minecraft has no command-line switch for its language; it reads `lang:<code>` from
`options.txt` in the game directory. `mlrc launch --lang zh_cn` rewrites that single
line before the process starts, creating the file when it is missing and leaving every
other setting untouched. `general.game_language` applies the same default to every
launch, and `none` clears it.

Codes are Minecraft's own lowercase identifiers (`en_us`, `zh_cn`, `pt_br`). Anything
else is rejected as a usage error rather than written into the file, where the game
would silently fall back and the user would not know why.

---

## 9. Version documents

### 9.1 Rules

Start denied, iterate in order, the last matching rule wins; `os` and `features`
inside one rule are AND-ed. An absent rule list means always allowed, which matters
for the majority of libraries.

- `os.name`: `windows`, `osx`, `linux`
- `os.arch`: `x86`, `x86_64`, `arm64`
- `os.version`: a regex, in practice a prefix such as `^10\.`; the Windows kernel
  version is taken from `ver`
- `os.versionRange`: `min`/`max`, used by 26.x to pick a garbage collector

### 9.2 Inheritance

`inheritsFrom` stacks a loader on top of a vanilla version:

- scalars: the child wins
- `arguments`: parent then child, concatenated
- `libraries`: deduplicated by exact coordinate, child first, so a loader that pins a
  newer ASM keeps its jar ahead of the vanilla one on the classpath
- `downloads`: field-level merge, child wins

Merging rewrites the pointer: the merged document inherits from whatever the parent
inherited from, never from the parent itself, or resolution would never terminate.

### 9.3 Libraries without a `downloads` block

Loader documents often publish Maven coordinates instead of artifacts:

```json
{"name": "net.fabricmc:fabric-loader:0.20.1", "url": "https://maven.fabricmc.net/"}
```

Both forms collapse into the same artifact: the coordinate becomes a repository path
(`group/artifact/version/artifact-version[-classifier].jar`) appended to the base URL.
Some of these carry no hash at all; those skip the content-addressed store and are
written straight to their destination, still retried, just unverified.

### 9.4 Loaders

Two mechanisms behind one command.

Fabric and Quilt publish a version document: `mlrc install <mc> --loader fabric|quilt`
fetches the profile from the loader's meta API, caches it under the id the loader chose
(`fabric-loader-<loader>-<mc>`, `quilt-loader-<loader>-<mc>`), and lets `inheritsFrom`
pull in the vanilla client, libraries and assets.

The two are one code path: same payload shape, different host and API version
(`meta.fabricmc.net/v2`, `meta.quiltmc.org/v3`). Fabric flags stable releases, Quilt
only says so in the version string, so the newest loader is picked by preferring a
release and skipping `-beta`/`-alpha` names.

Forge and NeoForge are installers. They patch the vanilla client through a processor
chain that a launcher cannot sensibly reimplement, so MLRC stages a launcher-shaped
directory (`launcher_profiles.json`, `versions/<mc>/<mc>.{json,jar}`), runs the
official installer with `--installClient`, and adopts the result: the produced version
document goes into the metadata cache, the installer's libraries are copied, and the
vanilla libraries are hardlinked in from the parent version instead of being stored
twice. The installer's own output is kept at `MLRC/logs/installer-<id>.log`, because
it is the only explanation when one fails.

Three details make that workable rather than merely possible:

- The installer's libraries are downloaded *first*, by our client, from the configured
  source. NeoForge's installer ignores `--mirror`, so on a network where its maven is
  blocked it would otherwise fail no matter what.
- The child JVM inherits the proxy the launcher detected (environment variables first,
  then the Windows internet settings). A JVM that does not know about the proxy hangs
  on every download while the launcher itself is fine.
- Progress is reported as it happens: the note names the log file, and a heartbeat
  every 15 seconds says the installer is still working. Its output is streamed to that
  log, not buffered until exit.

Either way the resulting id is what `launch` and `uninstall` take, and launch resolves
the chain from cached documents, so a loader install starts without network access.

### 9.5 Model

Every field defaults and unknown fields are ignored: Mojang adds fields between
releases, and a launcher that fails on the unknown is a broken launcher.

---

## 10. Output contract

### 10.1 Commands

```
mlrc install <version> [--loader fabric|quilt|forge|neoforge[:<version>]]
             [--java=auto|install|skip] [--yes] [--dry-run] [--threads N]
mlrc launch [<version>] [--instance <name>] [--lang <code>] [--memory <size>]
            [--jvm-arg <arg>] [--offline] [--username <n>]
            [--width N] [--height N] [--dry-run] [-- <game args>]
mlrc list [--all] [--json]
mlrc use <version>
mlrc uninstall <version>
mlrc instance create <name> --version <id> [--memory <size>] | list | remove <name>
mlrc mod search <query> [--mc <ver>] [--loader <name>] [--instance <n>] [--limit N]
mlrc mod install <project...> [--source modrinth|curseforge] [--dry-run] [--instance <n>] [--version <id>]
mlrc mod list [--instance <n>] [--version <id>]
mlrc mod update [--check] [--instance <n>] [--version <id>]
mlrc mod remove <project|file...> [--instance <n>] [--version <id>]
mlrc mod disable <project...> [--instance <n>] [--version <id>]
mlrc mod enable <project...> [--instance <n>] [--version <id>]
mlrc instance import <pack>.mrpack|<pack>.zip [--dry-run] [--name <n>] [--java=auto|install|skip]
mlrc java list | install <component|major> | remove <major>
mlrc auth login | logout | status
mlrc config get <key> | set <key> <value> | list
mlrc cache prune [--keep-days N]
mlrc doctor
```

Global flags: `--data-dir`, `--json`, `--quiet`, `-v`, `--mirror`.

### 10.2 Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 2 | usage error |
| 3 | network or download failure |
| 4 | authentication failure |
| 5 | Java requirement unsatisfied |
| 6 | metadata parse failure |
| 7 | portable data directory not writable |
| 8 | version not installed |
| 130 | interrupted |

### 10.3 Error codes

```
E_USAGE              E_NET_HTTP           E_NET_HASH
E_META_SHAPE         E_PORTABLE_READONLY  E_CONFIG
E_VERSION_UNKNOWN    E_VERSION_INCOMPLETE E_CACHE_STALE
E_JAVA_MISSING       E_JAVA_TOO_OLD       E_JAVA_META_MISSING
E_JAVA_IN_USE        E_AUTH_ENTITLEMENT   E_NOT_IMPLEMENTED
E_LOADER_UNAVAILABLE E_MOD_NOT_FOUND      E_MOD_CONFLICT
E_MOD_RESTRICTED     E_DOCTOR_FAILED      E_IO
```

Messages are English and final. The code is the contract; a front-end maps codes to
its own strings, which is why C1 forbids translated text in the engine.

The message itself has a shape, enforced by `tests/error_style.rs`:

- one lowercase sentence, no trailing period, no `error:` prefix, no newlines;
- it says what failed and, when the next step is known, what to do about it;
- a command inside it is backticked, so it is unambiguous where the sentence ends.

Details go in `context`, never in the sentence: ids, urls, status codes, counts. The
one reserved key is `hint`, printed on its own line - it is advice, not data. The CLI
renders context as `key=value` pairs; JSON output keeps the object.

### 10.4 NDJSON

`--json` writes one object per line, flushed immediately:

```json
{"schema":1,"event":"phase","phase":"download","total":96}
{"schema":1,"event":"java_required","component":"java-runtime-beta","major":17}
{"schema":1,"event":"file_finished","path":"1.18.2.jar","ok":true,"cached":false}
{"schema":1,"event":"done","message":"installed 1.18.2","ok":true}
```

`doctor` differs on purpose: one event carries the whole report, because a consumer
asking "is this installation healthy" should not have to reassemble an answer from a
stream.

```json
{"schema":1,"event":"doctor","ok":false,"failed":1,"warned":1,
 "checks":[{"name":"network","status":"fail","detail":"..."}]}
```

Errors arrive as their own event with the same code the exit status would carry:

```json
{"schema":1,"event":"error","code":"E_USAGE","message":"...","context":{"version":"1.21.1"}}
```

`schema` is versioned so a front-end can refuse an engine it does not understand.

### 10.5 Human output

English, one line per action, no colour. Access tokens are masked everywhere,
including in `-v` logs and `--dry-run` command lines.

### 10.6 Mods

`mlrc mod` speaks Modrinth's v2 API: search, and per-project versions filtered by
`game_versions` and `loaders`. CurseForge is available as a second source with
`--source curseforge`.

The two sources differ in ways the launcher has to absorb:

- CurseForge authenticates every request with `x-api-key`, and a key belongs to
  whoever registered it. MLRC never ships one: `curseforge.api_key` (or
  `MLRC_CURSEFORGE_KEY`, which wins) supplies it, and an empty key simply disables the
  source.
- Its search endpoint answers 403 even for keys that may read mods and files, so
  lookups take a numeric project id. A slug is rejected with the reason instead of a
  confusing failure.
- Authors may forbid third-party downloads. Such a file has no `downloadUrl`, and the
  install stops with `E_MOD_RESTRICTED` rather than silently doing nothing.

- The loader and Minecraft version come from an installed version rather than from
  parsing ids. `inheritsFrom` is the authority, which is also what makes
  `neoforge-21.1.265` resolvable at all. `--mc` and `--loader` override them.
- A version without a loader is a usage error: there is no mods directory to fill.
- Versions are taken newest first but `version_type: release` is preferred, so
  `mod install iris` does not quietly pick a beta.
- Required dependencies are installed too, depth-first and de-duplicated by project,
  so a dependency cycle cannot loop and a shared dependency lands once.
- Files go into `<game dir>/mods/`, are written through a `.part` sibling and are
  checked against Modrinth's SHA1. A file that is already there is left alone, which
  is what makes a second `mod install` a no-op rather than a re-download.
- What was installed is recorded in `mods/.mlrc-mods.json`: project slug and id,
  version, file name, and the source it came from. Neither site puts a project identity
  inside the jar, so nothing else could answer "which project is this?" later.
- `mod update` re-resolves every recorded project against the recorded platform,
  replaces outdated jars (write, swap, delete the old name) and reports the rest as up
  to date. `--check` stops before downloading.
- `mod remove` accepts a slug, a project id or a file name, and also removes jars the
  manifest never saw. An unknown name is `E_MOD_NOT_FOUND`.
- `mod disable` / `mod enable` rename the jar to `.jar.disabled` and back: loaders only
  read `*.jar`, so nothing is deleted and no second directory is needed. An update
  keeps a disabled mod disabled.
- `instance import <pack>` accepts both pack formats: a `.mrpack` whose index lists
  download URLs, and a CurseForge `.zip` whose `manifest.json` lists project and file
  ids. The latter needs the API key, installs the loader the pack pins, resolves every
  id through the API, and skips the files the pack marks optional. Entry paths are
  rejected unless they stay inside the game directory, so an archive cannot write
  elsewhere.
- Whether a CurseForge file is newer is decided by file id, not by release type: ids
  grow with upload time, while "prefer release" would report a beta the pack pinned as
  an update target in reverse. Installing still prefers a release when one exists.
- `mod install --dry-run` resolves the project and its dependencies and prints the
  file list without downloading, which is the cheap way to answer "what would this
  pull in".
- Downloads are attempted three times before failing: one CDN timeout should not cost
  the user the whole command.
- `instance import --dry-run` reports the pack's kind, Minecraft version, loader,
  override count and file list without writing anything. A CurseForge pack shows
  `projectID/fileID` pairs: resolving names would cost one request per file, which is
  not what a preview is for.
- Pack-provided Modrinth files are recorded without a project id, which makes `mod
  update` skip them and say why; CurseForge pack files do carry ids and update
  normally.

---

## 11. Testing

| Layer | Content |
|---|---|
| Unit | rule matrix, argument templating, inheritance merge, offline UUID, java version parsing, proxy selection, memory values, loader detection, manifest bookkeeping, pack path safety |
| Fixtures | real documents in the repository: 1.6.4 (legacy arguments), 1.12.2, 1.18.2 (extract rules), 1.21.x, 26.3 |
| Integration | `crates/mlrc-core/tests/install_flow.rs`: a real HTTP server on 127.0.0.1, a seeded manifest cache, then a full install. Asserts the layout, the bytes on disk, `installed.json`, `objects.txt`; that a corrupted download fails with `E_NET_HASH` and leaves no complete state; that a second install restores a deleted file from the store; that a truncated response fails the run and the next one finishes the job; and that the event stream keeps the documented shape |
| Style | `crates/mlrc-core/tests/error_style.rs` scans every `MlrcError::new` call and rejects a capital letter, a trailing period, an `error:` prefix, a newline, or an unbackticked `mlrc ...` command |
| CI | `fmt`, `clippy -D warnings` and `test` on Windows and Linux, an ASCII-only source check that enforces C1, and a release build on Windows, Linux and macOS whose binary is uploaded as a workflow artifact |

The integration test never touches the internet: the engine reads its manifest cache
before asking the network, so the fixture seeds it with URLs pointing at the local
server. Downloads force `proxy = none`, because a machine that routes 127.0.0.1
through a proxy would otherwise make the test depend on that proxy.

The event test is the contract test: it asserts the tag and field names each event
serializes to, because a front-end parses JSON and the Rust types are not the promise.
Warnings may precede the download phase - a missing runtime is reported before anything
is fetched - so the test looks for the phase rather than assuming position zero.

---

## 12. Known risks

| Risk | Mitigation |
|---|---|
| A shared public client id is revoked | configurable client id; a self-registered Azure application is documented |
| Mojang changes the metadata shape | every field defaulted, unknown fields ignored, `doctor` reports unhealthy pieces |
| A mirror is stale or incomplete | official hash authoritative, per-file fallback to the origin |
| A loader installer cannot reach its maven | loader metadata is fetched with a 10 second timeout and falls back to BMCLAPI; the installer's libraries are pre-downloaded through the configured source, the installer is handed `--mirror` when BMCLAPI is in use, and its full output is kept in `MLRC/logs/installer-<id>.log` |
| A machine needs a proxy | child JVMs (installers and the game) get `-Dhttp(s).proxy*` from `general.proxy`; `none` bypasses a broken proxy without touching the environment |
| Modrinth is unreachable or changes its shape | failures surface as `E_NET_HTTP` / `E_META_SHAPE` with the URL; nothing is written into `mods/` unless the SHA1 matches |
| The portable directory is read-only | fail with `E_PORTABLE_READONLY`, never relocate silently |
| Hardlinks are unsupported (FAT32, SMB) | degrade to copy and warn |
| Non-ASCII text creeps into sources | CI gate |
| A front-end drifts from the engine | NDJSON schema tests; the engine has no rendering dependencies |

---

## 13. Design decisions

- **Per-version install, no batch mode.** The unit of work mirrors `nvm install`.
- **Managed Java, keyed by major.** `MLRC/java/jdk-<major>`, host JDK ignored by
  default.
- **Content-addressed cache with hardlinks.** One copy of a jar, many directory
  entries.
- **Explicit reference lists.** `objects.txt` per version decides what `cache prune`
  may delete; link counting is not portable enough to rely on.
- **The version directory is the game directory.** Isolation by construction: a fork is
  the answer to "another world set for the same version", because that is a new version
  id, not a second directory attached to an existing one.
- **Offline mode is first-class.** It makes the launcher testable without an account,
  and it is the only way to play on a LAN without Mojang's services.
