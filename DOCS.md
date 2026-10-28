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
- Mod management beyond running a version that already has a loader installed.
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

Data root resolution order:

1. `--data-dir <path>`
2. `MLRC_DATA_DIR`
3. `<directory of mlrc>/data`

The executable directory is resolved with `current_exe()` and `canonicalize`. If it is
not writable, the launcher fails with `E_PORTABLE_READONLY`; it never falls back to a
per-user directory behind the user's back.

```
<dist>/
|-- mlrc.exe
`-- data/
    |-- config.toml
    |-- meta/
    |   |-- version_manifest_v2.json
    |   |-- java-runtime-index.json
    |   `-- versions/<id>/<id>.json
    |-- cache/objects/<aa>/<full sha1>     one copy of every file
    |-- assets/
    |   |-- indexes/<id>.json
    |   `-- objects/<aa>/<full sha1>
    |-- java/jdk-<major>/                  managed runtimes, keyed by major
    |-- versions/<id>/
    |   |-- <id>.jar
    |   |-- <id>.json
    |   |-- libraries/                     hardlinks into cache/objects
    |   |-- natives/
    |   |-- logging/
    |   |-- objects.txt                    hashes this version references
    |   `-- installed.json                 install state
    |-- minecraft/                         shared game directory
    `-- logs/
```

Notes:

- `cache/objects` and `versions/<id>` should share a volume for hardlinks; otherwise
  the store copies and reports a warning.
- `minecraft/` is the game directory: saves, options, resource packs, mods.
- Nothing is written outside `data/`.

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

`versions/<id>/installed.json` records completeness, the Java component and major, and
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

---

## 7. Java runtimes

`javaVersion.majorVersion` drives everything. Versions without a `javaVersion` block
are rejected with `E_JAVA_META_MISSING` rather than guessed at; the block is present in
almost the whole back catalogue, missing only in a few very old versions.

Runtime on disk: `data/java/jdk-<major>/`, keyed by major rather than by Mojang's
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
- Variables are `${name}` placeholders; unknown ones are left visible rather than
  silently emptied.

Feature flags (`is_demo_user`, `has_custom_resolution`, quick-play variants) are set by
the launcher before rule evaluation, never inferred from the version.

### 8.2 Natives

When the plan contains native jars, `.dll`/`.so`/`.dylib`/`.jnilib` entries are
extracted flat into `versions/<id>/natives/`, skipping `META-INF/`. When it does not,
the directory is simply empty. Modern versions rely on LWJGL extracting its own
libraries from the classpath.

### 8.3 Process

The game is spawned with inherited stdio and the game directory as working directory.
Output is not piped: a pipe would lose colour, add buffering, and force decoding of a
console stream that mixes UTF-8 with the legacy code page. The game's exit code is
returned to the caller unchanged.

### 8.4 Identity

Offline identity derives the UUID as MD5 of `OfflinePlayer:<name>` with the RFC 4122
version-3 and variant bits applied. The access token is a placeholder; online-mode
servers reject it, which is the point. `--uuid` is passed without dashes.

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

### 9.3 Model

Every field defaults and unknown fields are ignored: Mojang adds fields between
releases, and a launcher that fails on the unknown is a broken launcher.

---

## 10. Output contract

### 10.1 Commands

```
mlrc install <version> [--java=auto|install|skip] [--yes] [--dry-run] [--threads N]
mlrc launch [<version>] [--offline] [--username <n>] [--width N] [--height N]
            [--dry-run] [-- <game args>]
mlrc list [--all] [--json]
mlrc use <version>
mlrc uninstall <version>
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
```

Messages are English and final. The code is the contract; a front-end maps codes to
its own strings, which is why C1 forbids translated text in the engine.

### 10.4 NDJSON

`--json` writes one object per line, flushed immediately:

```json
{"schema":1,"event":"phase","phase":"download","total":96}
{"schema":1,"event":"java_required","component":"java-runtime-beta","major":17}
{"schema":1,"event":"file_finished","path":"1.18.2.jar","ok":true,"cached":false}
{"schema":1,"event":"done","message":"installed 1.18.2","ok":true}
```

`schema` is versioned so a front-end can refuse an engine it does not understand.

### 10.5 Human output

English, one line per action, no colour. Access tokens are masked everywhere,
including in `-v` logs and `--dry-run` command lines.

---

## 11. Testing

| Layer | Content |
|---|---|
| Unit | rule matrix, argument templating, inheritance merge, offline UUID, java version parsing, default-user-jvm parsing |
| Fixtures | real documents in the repository: 1.6.4 (legacy arguments), 1.12.2, 1.18.2 (extract rules), 1.21.x, 26.3 |
| Integration | a mock HTTP server: install, assert layout and hashes, resume after a simulated crash |
| Contract | NDJSON snapshots, so front-end compatibility is tested rather than promised |
| CI | `fmt`, `clippy -D warnings`, `test` on Windows and Linux, and an ASCII-only source check that enforces C1 |

---

## 12. Known risks

| Risk | Mitigation |
|---|---|
| A shared public client id is revoked | configurable client id; a self-registered Azure application is documented |
| Mojang changes the metadata shape | every field defaulted, unknown fields ignored, `doctor` reports unhealthy pieces |
| A mirror is stale or incomplete | official hash authoritative, per-file fallback to the origin |
| The portable directory is read-only | fail with `E_PORTABLE_READONLY`, never relocate silently |
| Hardlinks are unsupported (FAT32, SMB) | degrade to copy and warn |
| Non-ASCII text creeps into sources | CI gate |
| A front-end drifts from the engine | NDJSON schema tests; the engine has no rendering dependencies |

---

## 13. Design decisions

- **Per-version install, no batch mode.** The unit of work mirrors `nvm install`.
- **Managed Java, keyed by major.** `data/java/jdk-<major>`, host JDK ignored by
  default.
- **Content-addressed cache with hardlinks.** One copy of a jar, many directory
  entries.
- **Explicit reference lists.** `objects.txt` per version decides what `cache prune`
  may delete; link counting is not portable enough to rely on.
- **A shared game directory.** `data/minecraft/` for every version; separate instance
  directories would be an additive change, not a redesign.
- **Offline mode is first-class.** It makes the launcher testable without an account,
  and it is the only way to play on a LAN without Mojang's services.
