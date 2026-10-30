# MLRC - design notes

MLRC installs one Minecraft: Java Edition version per command, provisions the Java
runtime that version declares, and starts the game. The engine is a library with no
terminal dependencies, so a graphical front-end can link it without reimplementing
anything.

This document records the design and the reasoning behind it. No schedules, no
progress: if something here is not built yet, it says so.

---

## 1. Shape

### Constraints

| # | Constraint | Consequence |
|---|---|---|
| C1 | English only in code and output | Sources are ASCII-only and CI-checked; localization belongs to a front-end |
| C2 | Portable: nothing outside the distribution folder | Moving the folder moves the installation; silent relocation is forbidden |
| C3 | The CLI consumes the engine, it is not the engine | `mlrc-core` has no clap, no indicatif, and never prints |
| C4 | Every result is machine-readable | Exit codes, error codes and an event stream are the contract |
| C5 | One version per install, nvm-style | The unit of work is a version; no batch mode |
| C6 | The host Java is never used unless enabled | One managed runtime per major, immune to `JAVA_HOME` drift |

C6 earns its rationale: a machine has one `JAVA_HOME` and one `PATH`, while Minecraft
spans Java 8 to 25. Borrowing the host JVM means a system upgrade silently breaks
whatever version needed the old one. `java.custom = true` opts back in.

### Crates

```
mlrc-cli  ->  mlrc-core  ->  bytes-level dependencies only (serde, reqwest, tokio, zip)
```

```
crates/mlrc-core/     engine
  meta/               manifest, version document, rules, inheritance   (pure)
  plan/               install plan                                     (pure)
  launch/             arguments, natives, process                      (pure + std)
  fetch/              http client, download queue, mirrors, proxy
  store/              content-addressed store, hardlinks
  java/               managed runtimes
  auth/               identities: offline, Yggdrasil
  mods/               sources, manifests, modpacks
  port/               base directory, config, per-version state
crates/mlrc-cli/      binary "mlrc": clap, rendering, prompts, exit codes
```

A front-end links `mlrc-core`, never the CLI. That is why the engine has no rendering
dependencies and never writes to stdout.

### Events

The engine reports progress through a sink; rendering and interaction belong to the
caller.

```rust
pub enum Event {
    Phase { phase: String, total: Option<u64> },
    FileStarted { path: String, total_bytes: Option<u64> },
    FileProgress { path: String, bytes: u64 },
    FileFinished { path: String, ok: bool, cached: bool },
    JavaRequired { component: String, major: u32, download_bytes: u64 },
    Warning { code: String, message: String },
    Note { message: String },
    Done { message: String, ok: bool },
}

pub trait EventSink: Send + Sync { fn emit(&self, event: Event); }
```

`JavaRequired` is a question, not a prompt: the CLI renders `[Y/n]`, a GUI renders a
dialog, `--json` emits a line plus exit code 5. The engine never reads stdin.

### Out of scope

- Bundling or redistributing any Minecraft file; everything is fetched on demand.
- A GUI inside this repository; the engine is shaped so one can be added.
- Publishing or hosting mods; MLRC only installs from Modrinth and CurseForge.
- Mojang's own account system: Microsoft login, entitlements.

---

## 2. On disk

The base directory is `--data-dir`, else `MLRC_DATA_DIR`, else the directory of the
executable. Two folders, split by who reads them.

```
<base>/
|-- mlrc.exe
|-- .minecraft/                        the standard launcher directory
|   |-- launcher_profiles.json         minimal stub; loader installers require it
|   |-- assets/{indexes,objects}
|   |-- libraries/<maven path>         one tree, shared by every version
|   `-- versions/<id>/
|       |-- <id>.json                  the version document, standard location
|       |-- <id>.jar                   client jar; a fork hardlinks its parent's
|       |-- <id>-natives/{java,jna,lwjgl,netty}
|       |-- MLRC/{installed.json, objects.txt, instance.json}
|       |-- saves/ mods/ resourcepacks/ config/ logs/
|       `-- options.txt                the game directory is this same folder
`-- MLRC/                              launcher-private
    |-- config.toml
    |-- auth.json                      Yggdrasil session, when one is stored
    |-- cache/objects/<aa>/<sha1>      one copy of every file, hardlinked into place
    |-- java/jdk-<major>/
    |-- injector/authlib-injector-<build>.jar
    |-- meta/  logs/  tmp/
```

Notes:

- The version directory doubles as its game directory. Isolation by construction, and
  `mlrc uninstall` deletes exactly what belonged to that version.
- `.minecraft/libraries` is shared, so deleting a version must never delete libraries;
  it does not, it only removes the version directory.
- `cache/objects` and `.minecraft` must share a volume for hardlinks; on another
  volume the store copies and warns.
- Every install writes `MLRC/objects.txt`, the list of hashes it pulled. `cache prune`
  trusts those lists, never link counts, and refuses to run when one is missing
  (`E_CACHE_STALE`) rather than guessing.

---

## 3. Configuration

`MLRC/config.toml`, every key in one place. `mlrc config get|set|list` reads and
writes it; `set` validates before writing.

| Key | Values | Effect |
|---|---|---|
| `java.custom` | `false` (default), `true` | `true` allows a host JVM when no managed runtime matches |
| `java.path` | path, empty | explicit binary; only with `custom = true` |
| `java.xms` | `512M`, `1G`, empty | initial heap; empty leaves the JVM default |
| `java.xmx` | `2G` (default) | maximum heap; `--memory` and a fork's own value override it |
| `general.default_version` | version id, empty | what `mlrc launch` starts |
| `general.mirror` | `auto` (default), `official`, `bmclapi` | download source, see §4.4 |
| `general.proxy` | `auto` (default), `none`, `host:port` | the launcher **and** every child JVM |
| `general.game_language` | `en_us`, `zh_cn`, empty | written into `options.txt`; see §5.6 |
| `auth.server` | Yggdrasil API root, empty | default for `mlrc auth login`; empty means offline only |
| `auth.player_name` | 1-16 chars of `[A-Za-z0-9_]`, default `Player` | offline name |
| `curseforge.api_key` | key, empty | enables the CurseForge source; `none` clears |

Environment variables, which win over the file:

- `MLRC_DATA_DIR` - the base directory.
- `MLRC_CURSEFORGE_KEY` - the API key, so it need not be written to disk.

Three of these deserve their reasoning:

**Proxy reaches child JVMs.** The HTTP client honours the system proxy, which is right
until the proxy runs but does not forward: then every request fails while a direct
connection works, and no error says why. `none` is that escape hatch. Child JVMs know
nothing about the system proxy, so loader installers and the game get
`-Dhttp(s).proxyHost` from the same key - `none` therefore also means the game ignores
it. `mlrc doctor` reports the network, which is what makes a misconfigured proxy
obvious.

**The mirror is URL substitution, not trust.** Official hashes stay authoritative, so
a stale or hostile mirror cannot corrupt an install. BMCLAPI answers with a 302 to a
node (redirects must be followed), has incomplete coverage (client jars redirect back
to Mojang, some small objects 404), and serves maven and asset paths directly, which
is where the speedup comes from. Each file falls back to the official URL on its own.
`auto` times the manifest against both hosts and remembers the winner. BMCLAPI's terms
require naming the source, so a run through it prints it and emits a `source` event.

**`general.mirror` is written by `auto`.** The resolved choice is stored, so the next
run is instant instead of probing again.

---

## 4. Install

### 4.1 Pipeline

```
1. load the manifest (cached, revalidated by age)
2. fetch the version document (content-addressed, so cached by URL)
3. resolve inheritance, if the document declares a parent
4. evaluate library rules for this machine
5. read javaVersion -> { component, major }
6. resolve Java: managed runtime, host runtime when allowed, or provision one
7. build the file list: client jar, libraries, natives, logging config, asset index
8. download into cache/objects with SHA1 verification, materialize by hardlink
9. read the asset index, download every object, write installed.json and objects.txt
```

Idempotent: an already-complete version re-checks sizes and stops without network work
beyond the manifest.

### 4.2 The Java gate

`javaVersion.majorVersion` drives everything; a document without that block is
`E_JAVA_META_MISSING` rather than a guess. Runtimes live at `MLRC/java/jdk-<major>/`,
keyed by major because two components can share one (`java-runtime-gamma` and its
snapshot are both 17). Provenance is in `jdk-<major>/mlrc.json`.

Selection: a managed runtime of the required major; otherwise a host runtime, only
with `custom = true` (explicit `java.path`, then `JAVA_HOME`, then `PATH`), and only
if its major matches exactly; otherwise the gate.

| Mode | Behaviour |
|---|---|
| interactive terminal | prompt `java 17 is not installed. download it now? [Y/n]` |
| `--java=install` | no prompt, provision silently |
| `--java=skip` | install without Java; launch fails later |
| `--yes` | accept every prompt |
| `--json` or non-interactive | never prompt: emit `java_required`, exit 5 |

Exit 5 is a request for a decision, not a failure: the event carries `component` and
`major`, so a front-end asks its own question and re-runs with `--java=install`.

Versions are validated by reading `<home>/release` (`JAVA_VERSION="17.0.15"`) instead
of running `java -version`, whose output is localized and costs a process. Both
`1.8.0_51` and `8u51-cacert462b08` forms are accepted. Installation unpacks into
`jdk-<major>.part/` and renames into place, so an interrupted download never leaves a
half-JDK.

### 4.3 State and removal

`versions/<id>/MLRC/installed.json` records completeness, the Java component and major,
file count and size. `complete: false` is repaired by re-running install and refused by
launch. `mlrc uninstall <id>` removes the version directory only and clears the default
version if it pointed there.

### 4.4 Loaders

Two mechanisms behind one command.

**Fabric and Quilt publish a document.** `--loader fabric|quilt` fetches the profile
from the loader's meta API, caches it under the id the loader chose
(`fabric-loader-<loader>-<mc>`), and lets `inheritsFrom` pull in the vanilla client,
libraries and assets. One code path: same payload, different host and API version
(`meta.fabricmc.net/v2`, `meta.quiltmc.org/v3`). Fabric flags releases; Quilt only says
so in the version string, so the newest loader is picked by preferring a release and
skipping `-beta`/`-alpha`.

**Forge and NeoForge are installers.** They patch the vanilla client through a
processor chain no launcher should reimplement, so MLRC stages a launcher-shaped
directory (`launcher_profiles.json`, `versions/<mc>/<mc>.{json,jar}`), runs the
official installer with `--installClient`, and adopts the result: the document goes
into the version directory, its libraries merge into the shared tree, and the vanilla
libraries are hardlinked rather than stored twice. Its output is kept at
`MLRC/logs/installer-<id>.log`, because it is the only explanation when one fails.

Three details make that workable:

- The installer's libraries are downloaded first, by our client, from the configured
  source. NeoForge's installer ignores `--mirror`, so on a network where its maven is
  blocked it would otherwise fail regardless.
- The child JVM gets the proxy the launcher detected (see §3).
- `java.io.tmpdir` is redirected into `MLRC/tmp`; a locked-down system temp directory
  otherwise breaks the installer mid-run.

### 4.5 Mods and packs

`mlrc mod` speaks Modrinth's v2 API for search and version selection; CurseForge is a
second source. What the launcher has to absorb:

- **The platform comes from an installed version, never from parsing ids.**
  `inheritsFrom` is the authority, which is also the only way `neoforge-21.1.265`
  resolves. `--mc`/`--loader` override it; a version without a loader is a usage error.
- **CurseForge needs a user-supplied key** (`x-api-key`). MLRC never ships one. Its
  search endpoint answers 403 even for keys allowed to read mods, so lookups take a
  numeric project id and a slug is rejected with the reason.
- **Release preferred over newest.** `mod install iris` must not quietly pick a beta.
- **Dependencies are installed too**, depth-first, de-duplicated by project, so a cycle
  cannot loop and a shared dependency lands once.
- **Files are written through a `.part` sibling and verified** against the published
  SHA1; an existing file is left alone, which makes a second install a no-op.
- **What was installed is recorded** in `mods/.mlrc-mods.json`: project id and slug,
  version, file name, source. Neither site puts a project identity inside the jar, so
  nothing else could answer "which project is this?".
- **Updates compare file ids**, not release type, when deciding whether a CurseForge
  file is newer; otherwise a beta would downgrade a release. `--check` stops before
  downloading. A disabled mod stays disabled.
- **Disable is a rename** to `.jar.disabled`, because loaders only read `*.jar`.
- **Pack import** accepts `.mrpack` (index lists URLs) and CurseForge `.zip` (manifest
  lists ids, needs the key, skips optional files). Entry paths must stay inside the
  game directory, so an archive cannot write elsewhere.

---

## 5. Launch

### 5.1 Arguments

The document's `arguments.jvm` and `arguments.game` are expanded against a variable
table (`${auth_player_name}`, `${version_name}`, `${game_directory}`, `${assets_root}`,
`${natives_directory}`, `${classpath}`, `${library_directory}`, `${classpath_separator}`
...). The last two are what Forge and NeoForge use to build their module path; without
them the literal `${library_directory}` reaches the JVM and the bootstrap cannot find
its modules.

`classpath_separator` is `;` on Windows and `:` elsewhere. The classpath is the
version jar followed by the shared library tree.

Documents without `arguments` fall back to `minecraftArguments`, split on whitespace.

### 5.2 Rules and inheritance

Rules start denied, are evaluated in order, last match wins; `os` and `features` inside
one rule are AND-ed. An absent rule list means always allowed, which matters for most
libraries. `os.version` is a regex, in practice a prefix; 26.x uses `os.versionRange`
to pick a garbage collector.

`inheritsFrom` stacks a loader on a vanilla version:

- scalars: the child wins
- `arguments`: parent then child, concatenated
- `libraries`: deduplicated by exact coordinate, child first, so a loader pinning a
  newer ASM keeps its jar ahead of the vanilla one
- `downloads`: field-level merge, child wins

Merging rewrites the pointer to whatever the parent inherited from, never to the parent
itself, or resolution never terminates.

Loader documents often publish coordinates instead of artifacts
(`{"name": "net.fabricmc:fabric-loader:0.20.1", "url": "https://maven.fabricmc.net/"}`).
Both forms collapse into one artifact: the coordinate becomes a repository path
appended to the base URL. Some carry no hash; those skip the store and are written
straight to their destination, retried but unverified.

### 5.3 Identity

Three sources, in order: a stored Yggdrasil account, `auth.player_name`, `--username`
(which forces offline mode).

**Offline.** The UUID is MD5 of `OfflinePlayer:<name>` with the RFC 4122 version-3 and
variant bits applied; the token is a placeholder, so online-mode servers reject it,
which is the point. Names follow Mojang's rule - 1-16 characters of `[A-Za-z0-9_]` -
because a name the game accepts but a server rejects is a trap.

**Yggdrasil.** `mlrc auth login --server <url>` posts to
`<url>/authserver/authenticate`, stores the token and profile in `MLRC/auth.json`
(`0600` on Unix), and downloads authlib-injector. Launch then passes the token, the
profile name and UUID, `--userType mojang`, and `-javaagent:<jar>=<url>`.

The injector is not optional: it redirects the client's own session requests to that
server. Without it the game authenticates locally and then fails to join the service's
online-mode servers, with an error that blames the server.

Every Yggdrasil failure is a 403, and `errorMessage` is the only place that separates a
wrong password from an expired token, so the body becomes the message of
`E_AUTH_FAILED` (exit 4). The jar is fetched once, verified against the published
SHA-256, then reused from disk so launches stay offline-capable.

### 5.4 Natives

Two generations. Libraries carrying `natives`/`extract` (up to 1.18) are unpacked by
the launcher; 1.19+ puts native jars on the classpath and lets LWJGL extract them, so
an empty set is normal. Decide from the library entry, never from the version id.

```
versions/<id>/<id>-natives/
|-- java/     launcher-extracted natives; `-Djava.library.path` points here
|-- jna/      `-Djna.tmpdir`
|-- lwjgl/    `-Dorg.lwjgl.system.SharedLibraryExtractPath`
`-- netty/    `-Dio.netty.native.workdir`
```

Mojang's documents point all four properties at one directory; the launcher rewrites
the three that name a family so each has its own, as the launcher we imitate does.

Everything the launcher unpacks goes into `java/`, including LWJGL's DLLs for 1.18 and
older. The family split is for libraries that unpack themselves at runtime; a document
that extracts natives loads them through `java.library.path`, and files placed in
`lwjgl/` are invisible to it - `Failed to load a library`, then `NoClassDefFoundError`
on `RenderSystem`.

### 5.5 Process and game directory

The game is spawned with inherited stdio and the version directory as working
directory. Output is not piped: a pipe loses colour, adds buffering and forces decoding
of a console stream that mixes UTF-8 with the legacy code page. The exit code is
returned unchanged. Access tokens are masked everywhere, including `-v` and `--dry-run`.

The game directory is the version directory, which is the isolation model of the
launcher this project imitates: one version, one set of worlds. A second set for the
same Minecraft version is a **fork**: a two-line document with `inheritsFrom`, a client
jar hardlinked from its parent, and `MLRC/instance.json`.

```
mlrc instance create survival --version 1.21.1
mlrc launch --instance survival
```

Nothing is copied. Because a fork is a real version id, `list`, `uninstall`, `mod` and
`launch <id>` accept it without knowing what it is.

`-Djava.io.tmpdir` points into `MLRC/tmp`, so loader caches and image caches stay in the
distribution folder.

### 5.6 Game language

Minecraft has no launch switch for it; it reads `lang:<code>` from `options.txt` in the
game directory. `--lang zh_cn` rewrites that single line, creating the file when
missing and leaving every other setting untouched. `general.game_language` does the
same for every launch; `none` clears it. Codes are Minecraft's own lowercase
identifiers; anything else is a usage error rather than a silent fallback.

---

## 6. Output contract

### 6.1 Commands

```
mlrc install <version> [--loader fabric|quilt|forge|neoforge[:<version>]]
             [--java=auto|install|skip] [--yes] [--dry-run] [--threads N]
mlrc launch [<version>] [--instance <name>] [--lang <code>] [--memory <size>]
            [--jvm-arg <arg>] [--offline] [--username <n>]
            [--width N] [--height N] [--dry-run] [-- <game args>]
mlrc list [--all] | use <version> | uninstall <version>
mlrc instance create <name> --version <id> [--memory <size>] | list | remove <name>
mlrc instance import <pack>.mrpack|<pack>.zip [--dry-run] [--name <n>] [--java=...]
mlrc mod search <query> [--mc <ver>] [--loader <name>] [--instance <n>] [--limit N]
mlrc mod install <project...> [--source modrinth|curseforge] [--dry-run] [--instance <n>]
mlrc mod list | update [--check] | remove <project|file...> | disable | enable
mlrc java list | install <component|major> | remove <major>
mlrc auth login [--server <url>] [--username <n>] [--password-stdin] | logout | status
mlrc config get <key> | set <key> <value> | list
mlrc cache prune [--keep-days N]
mlrc doctor
```

Global flags: `--data-dir`, `--json`, `--quiet`, `-v`, `--mirror`.

### 6.2 Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 2 | usage error |
| 3 | network or download failure |
| 4 | authentication failure |
| 5 | Java requirement unsatisfied |
| 6 | metadata parse failure |
| 7 | portable directory not writable |
| 8 | version not installed |
| 130 | interrupted |

### 6.3 Error codes

```
E_USAGE              E_NET_HTTP           E_NET_HASH
E_META_SHAPE         E_PORTABLE_READONLY  E_CONFIG
E_VERSION_UNKNOWN    E_VERSION_INCOMPLETE E_CACHE_STALE
E_JAVA_MISSING       E_JAVA_TOO_OLD       E_JAVA_META_MISSING
E_JAVA_IN_USE        E_AUTH_FAILED        E_NOT_IMPLEMENTED
E_LOADER_UNAVAILABLE E_MOD_NOT_FOUND      E_MOD_CONFLICT
E_MOD_RESTRICTED     E_DOCTOR_FAILED      E_IO
```

Messages are English and final. The code is the contract; a front-end maps codes to
its own strings, which is why C1 forbids translated text in the engine.

The message has a shape, enforced by `tests/error_style.rs`: one lowercase sentence, no
trailing period, no `error:` prefix, no newlines; it says what failed and, when the next
step is known, what to do about it; a command inside it is backticked. Details go in
`context` - ids, urls, status codes, counts - and the one reserved key is `hint`, printed
on its own line because it is advice, not data.

### 6.4 NDJSON

`--json` writes one object per line, flushed immediately:

```json
{"schema":1,"event":"phase","phase":"download","total":96}
{"schema":1,"event":"java_required","component":"java-runtime-beta","major":17}
{"schema":1,"event":"file_finished","path":"1.18.2.jar","ok":true,"cached":false}
{"schema":1,"event":"done","message":"installed 1.18.2","ok":true}
{"schema":1,"event":"error","code":"E_USAGE","message":"...","context":{"version":"1.21.1"}}
```

Errors carry the same code the exit status would. `schema` is versioned so a front-end
can refuse an engine it does not understand.

`doctor` differs on purpose: one event carries the whole report, because a consumer
asking "is this installation healthy" should not have to reassemble an answer from a
stream.

```json
{"schema":1,"event":"doctor","ok":false,"failed":1,"warned":1,
 "checks":[{"name":"network","status":"fail","detail":"..."}]}
```

---

## 7. Testing

| Layer | What it pins |
|---|---|
| Unit | rules and version ranges, inheritance merge, memory sizes, option rewriting, path safety, error-message style, ASCII sources |
| Integration (`install_flow.rs`) | a real HTTP server on 127.0.0.1, a seeded manifest cache, then a full install: layout, bytes on disk, `installed.json`, `objects.txt`, `E_NET_HASH` on a corrupted download, store restore of a deleted file, truncated-response recovery, the event shape |
| Integration (`auth_flow.rs`) | a local Yggdrasil server: login, failure body as the message, validate, refresh, stored session, injector selection |
| CI | `fmt`, `clippy -D warnings` and `test` on Windows and Linux, an ASCII-only source check that enforces C1, and a release build on Windows, Linux and macOS uploaded as a workflow artifact |

The style test is why error messages do not drift: it scans every `MlrcError::new`
literal for the shape above.

Not covered: the network paths against Mojang's real hosts run only by hand, and no test
needs an account.

---

## 8. Known risks

| Risk | Handling |
|---|---|
| A mirror serves a stale or wrong file | SHA1 from the official document decides; mismatch is `E_NET_HASH` |
| A proxy that does not forward | `general.proxy = none`, and `doctor` shows which path is taken |
| An antivirus blocks writes inside the JVM | `doctor` probes a write and names the cause; loader remapping needs it |
| A version is deleted with a game running | removal retries, then fails with a `hint` rather than a half-deleted tree |
| A modpack writes outside the game directory | entry paths are rejected unless they stay inside |
| Two Minecraft versions need different Javas | one managed runtime per major, keyed by major |
| The shared library tree is touched by another launcher | accepted: libraries are content-addressed and re-verified by SHA1 on the next install |
| A mod file changes between resolve and download | SHA1 from the version API; mismatch is `E_NET_HASH` |

---

## 9. Decisions worth remembering

- **The version directory is the game directory.** Isolation by construction; a fork is
  the answer to "another world set", because that is a new version id.
- **Managed Java, keyed by major.** The host JDK is ignored by default; one runtime per
  major serves every version that needs it.
- **Content-addressed cache with hardlinks.** One copy of a jar, many directory entries,
  and an explicit `objects.txt` list rather than link counts for what may be deleted.
- **Official hashes are the only authority.** Mirrors move bytes, never trust.
- **The engine never prints and never prompts.** Interaction is the caller's job, which
  is what lets a GUI link the same crate.
- **Offline mode is first-class.** It makes the launcher testable without an account and
  is the only way to play on a LAN without Mojang's services.
