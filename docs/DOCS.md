# MLRC - design notes

MLRC installs one Minecraft: Java Edition version per command, provisions the Java
runtime that version declares, and starts the game. The engine is a library with no
terminal dependencies, so another front-end can link it without reimplementing
anything.

This is the design: what the launcher does and why. Implementation details - metadata
quirks, merge rules, module map - live in [DEVELOPMENT.md](DEVELOPMENT.md).

---

## 1. Scope

### In scope

- Installing vanilla versions from Mojang's own metadata, one version per command.
- Managing the Java runtimes those versions need.
- Launching the game, with an offline or third-party identity.
- Loaders, mods and modpacks on top of an installed version.
- A machine-readable interface: stable exit codes, error codes, NDJSON events.

### Out of scope

- Bundling or redistributing any Minecraft file; everything is fetched on demand.
- A GUI: this is a command-line launcher.
- Publishing or hosting mods; MLRC only installs from Modrinth and CurseForge.
- Mojang's own account system: Microsoft login, entitlements.

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
- `.minecraft/libraries` is shared, so removing a version must never remove libraries;
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

A key is always `section.name` and never has a second spelling: `gameLanguage`, `xmx`
and `mirror` were accepted once and are not any more, because two spellings for one
setting is how a document drifts. Typing one now fails with the key that was meant.

| Key | Values | Effect |
|---|---|---|
| `java.custom` | `false` (default), `true` | `true` allows a host JVM when no managed runtime matches |
| `java.path` | path, empty | explicit binary; only with `java.custom = true` |
| `java.xms` | `512M`, `1G`, empty | initial heap; empty leaves the JVM default |
| `java.xmx` | `2G` (default) | maximum heap; `--memory` and a fork's own value override it |
| `general.default_version` | version id, empty | what `mlrc launch` starts |
| `general.mirror` | `auto` (default), `official`, `bmclapi` | download source, see below |
| `general.proxy` | `auto` (default), `none`, `host:port` | the launcher **and** every child JVM |
| `general.game_language` | `en_us`, `zh_cn`, empty | written into `options.txt`; see section 5.4 |
| `launch.brand` | text, empty | `${launcher_name}`; the game prints it in the corner of the main menu (up to 26.0) |
| `launch.jvm_args` | space-separated flags | appended after the document's own JVM flags |
| `launch.game_args` | space-separated arguments | appended after the document's own game arguments |
| `launch.pre_launch` | shell command, empty | run in the game directory before the game starts |
| `auth.server` | Yggdrasil API root, empty | default for `mlrc auth login`; empty means offline only |
| `auth.player_name` | 1-16 chars of `[A-Za-z0-9_]`, default `Player` | offline name |
| `curseforge.api_key` | key, empty | enables the CurseForge source; `none` clears |

Environment variables, which win over the file: `MLRC_DATA_DIR` (the base directory)
and `MLRC_CURSEFORGE_KEY` (the API key, so it need not be written to disk).

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
node, has incomplete coverage, and serves maven and asset paths directly, which is
where the speedup comes from; every file falls back to the official URL on its own.
`auto` times the manifest against both hosts and writes the winner back, so the next
run does not probe again. BMCLAPI's terms require naming the source, so a run through
it prints it and emits a `source` event.

**`java.xmx` is a default, not a decision.** `--memory`, a fork's own value and the
config are tried in that order, so a single heavy modpack does not force every version
to carry its heap.

---

## 4. Install

### 4.1 What the command promises

```
1. load the manifest, cached and revalidated by age
2. fetch the version document, inherit from its parent when it declares one
3. decide which library and native files this machine needs
4. resolve Java: a managed runtime, a host runtime when allowed, or provision one
5. download everything into the content store, verify, hardlink into place
6. download the asset index and every object it names
7. record completeness, the file count, the size and the hashes it pulled
```

Idempotent: an already-complete version re-checks sizes and stops without network work
beyond the manifest. An interrupted install leaves `complete: false`, which the next
run repairs and launch refuses.

**What "verified" means.** Two different promises, kept apart on purpose:

| | Verified against | Catches |
|---|---|---|
| A hash the source publishes | SHA1 from Mojang, SHA512 from Modrinth, SHA256 from the injector API - whichever is strongest | damage in transit, a mirror serving something else, a file swapped after publication |
| No hash published | the length, when the source declares one | a truncated or padded transfer |

A hash is corruption detection, not proof of origin: it comes over the same connection
as the file, so TLS is what makes it meaningful. SHA1 is enough to notice accidental
damage and is not collision-resistant; where a source offers something stronger, the
stronger one is used. Every file, hashed or not, is written to a `.part` sibling,
flushed, and renamed into place, so an interrupted transfer never leaves a short file
where a complete one belongs. A run that accepted files without a hash says how many,
because "installed" should not hide that.

### 4.2 The Java gate

`javaVersion.majorVersion` drives everything; a document without that block is
`E_JAVA_META_MISSING` rather than a guess.

| Mode | Behaviour |
|---|---|
| interactive terminal | prompt `java 17 is not installed. download it now? [Y/n]` |
| `--java=install` | no prompt, provision silently |
| `--java=skip` | install without Java; launch fails later |
| `--yes` | accept every prompt |
| `--json` or non-interactive | never prompt: emit `java_required`, exit 5 |

Exit 5 is a request for a decision, not a failure: the event carries `component` and
`major`, so a front-end asks its own question and re-runs with `--java=install`.

Runtimes live at `MLRC/java/jdk-<major>/`, keyed by major because two components can
share one (`java-runtime-gamma` and its snapshot are both 17). Selection is a managed
runtime of the required major; otherwise a host runtime, only with `custom = true`
(explicit `java.path`, then `JAVA_HOME`, then `PATH`) and only if its major matches
exactly; otherwise the gate.

### 4.3 Loaders

Two mechanisms behind one command: Fabric and Quilt publish a version document, while
Forge and NeoForge are installers that patch the client themselves. In both cases the
result is an ordinary version, so `launch`, `mod` and `fork` need no special cases.
The mechanics are in [DEVELOPMENT.md](DEVELOPMENT.md#7-loaders).

### 4.4 Listing what can be installed

`mlrc install --list` prints the versions the manifest knows, newest first, and then
asks how many to show: `show the first (10/921)`, Enter for ten, a number for that
many, `all` for the rest. The question exists because the answer is 921 lines and a
terminal is a bad place to discover that. A pipe, `--json` and `--limit N` skip it and
behave deterministically; `--limit 0` prints everything.

`--type` narrows to one category - `release`, `snapshot`, `old_beta`, `old_alpha` or
`april_fools` - and composes with `--loader`. Coverage comes from each loader's own
source in one request: the meta APIs publish their supported game versions, and the
Forge and NeoForge maven metadata is mapped back to Minecraft ids and filtered against
the manifest, so a repository alias never appears as a version that cannot be installed.

`april_fools` is derived rather than read: the manifest marks those builds as ordinary
snapshots. A snapshot released on 1 April counts, plus the ones named in a short list
(`1.RV-Pre1` shipped on 31 March). That is a heuristic, and it is the best the metadata
allows; a normal snapshot shipped on 1 April would be misclassified.

Mods get no such list on purpose: there is no bounded answer, and `mlrc mod search` is
the question people actually have.

### 4.5 Mods and packs

`mlrc mod` searches Modrinth and picks a version for the loader and Minecraft version
of an installed version; required dependencies come with it. CurseForge is a second
source, keyed by the user, and answers per project id.

What the launcher guarantees: a disabled mod is renamed, not deleted; a second install
of the same file is a no-op; an update never downgrades a release to a beta; an
existing jar is never overwritten silently; and a pack archive cannot write outside the
game directory. Details in [DEVELOPMENT.md](DEVELOPMENT.md#8-mods).

---

## 5. Launch

### 5.1 Identity

Three sources, in order: a stored Yggdrasil account, `auth.player_name`, `--username`
(which forces offline mode).

**Offline.** The UUID is MD5 of `OfflinePlayer:<name>` with the RFC 4122 version-3 and
variant bits applied; the token is a placeholder, so online-mode servers reject it,
which is the point. Names follow Mojang's rule - 1-16 characters of `[A-Za-z0-9_]` -
because a name the game accepts but a server rejects is a trap.

**Yggdrasil.** `mlrc auth login --server <url>` posts the credentials to
`<url>/authserver/authenticate`, stores the token and profile in `MLRC/auth.json`
(`0600` on Unix), and downloads authlib-injector. Launch then passes the token, the
profile name and UUID, `--userType mojang`, and `-javaagent:<jar>=<url>`.

The injector is not optional: it redirects the client's own session requests to that
server. Without it the game authenticates locally and then fails to join the service's
online-mode servers, with an error that blames the server. The jar is fetched once,
verified against the published SHA-256, then reused from disk so launches stay
offline-capable.

The token has a lifetime, and the launcher treats it as one: before starting the game it
asks the server whether the token is still valid, refreshes it when the answer is no,
and continues with what is on disk when the server cannot be reached - playing alone
should not need a working network. `mlrc auth logout` revokes the token server-side and
deletes the file regardless of whether the server answered; the local session is gone
either way.

The file is plaintext, like every launcher's profile file, and written through a
temporary file with `0600` set before the first byte, so a crash cannot leave half a
document and another user cannot read the token even for an instant. On Unix that is the
whole story; on Windows the file inherits the ACL of the folder it lives in, which is
private in a user profile and is not in a shared folder. The token never appears in
output: the session's `Debug` implementation redacts it, so a log line, a panic message
or a `{:?}` in an error cannot leak it. `--accessToken` is masked in `launch --dry-run`
and in verbose logs.

### 5.2 Game directory and forks

The game directory is the version directory, which is the isolation model of the
launcher this project imitates: one version, one set of worlds.

A second set for the same Minecraft version is a **fork**: a two-line document with
`inheritsFrom`, a client jar hardlinked from its parent, and `MLRC/instance.json`.
Nothing is copied, and the library tree and assets stay shared.

```
mlrc instance create survival --version 1.21.1
mlrc launch --instance survival
```

Because a fork is a real version id, `list`, `uninstall`, `mod` and `launch <id>`
accept it without knowing what it is.

### 5.3 Launch settings

Five settings shape a launch, and each exists globally and per fork: `launch.brand`,
`launch.jvm_args`, `launch.game_args`, `launch.pre_launch`, plus the heap. A fork's file
carries the same struct, and an empty field inherits the global one, so a fork only
records what it actually changes.

Arguments are appended after the document's own, which is what makes `--demo` work
without displacing anything Mojang put there. `${...}` markers are expanded in them
exactly as in the version document, so `-Dhere=${game_directory}` means what it looks
like; the table is the same one section 5.1 describes.

`launch.pre_launch` runs through the shell in the game directory before the JVM starts.
Its output is inherited rather than captured, and a non-zero status stops the launch
with `E_PRE_LAUNCH`: a command that exists to prepare state should not be ignored when
it fails.

**The window title is not settable.** Minecraft chooses its own title, and it reads no
property for it; changing it needs bytecode patching or a Java agent, which this
launcher does not ship. What the game *does* read is the brand string
(`-Dminecraft.launcher.brand`), and that is `launch.brand`. Mojang stopped displaying it
in 26.1, so on newer versions the property is passed and has nowhere to show.

### 5.4 Game language

Minecraft has no launch switch for it; it reads `lang:<code>` from `options.txt` in the
game directory. `--lang zh_cn` rewrites that single line, creating the file when
missing and leaving every other setting untouched. `general.game_language` does the
same for every launch; `none` clears it. Codes are Minecraft's own lowercase
identifiers; anything else is a usage error rather than a silent fallback.

### 5.5 Process

The game is spawned with inherited stdio and the version directory as working
directory. Output is not piped: a pipe loses colour, adds buffering and forces decoding
of a console stream that mixes UTF-8 with the legacy code page. The exit code is
returned unchanged.

`-Djava.io.tmpdir` points into `MLRC/tmp`, so loader caches and image caches stay in the
distribution folder. Access tokens are masked everywhere, including `-v` and `--dry-run`.

---

## 6. Output contract

### 6.1 Commands

```
mlrc install <version> [--loader fabric|quilt|forge|neoforge[:<version>]]
             [--java=auto|install|skip] [--yes] [--dry-run] [--threads N]
mlrc install --list [--loader fabric|quilt|forge|neoforge]
                    [--type release|snapshot|old_beta|old_alpha|april_fools] [--limit N]
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

The code is the contract; a front-end maps codes to its own strings, which is why C1
forbids translated text in the engine. The full contract - events, ordering, decisions,
cancellation, recovery - is in [API.md](API.md). Messages have a fixed shape, enforced by a test:
one lowercase sentence, no trailing period, no `error:` prefix, what failed and what to
do about it. Details go in `context` - ids, urls, status codes, counts - and the one
reserved key is `hint`, printed on its own line because it is advice, not data.

### 6.4 NDJSON

`--json` writes one object per line, flushed immediately:

```json
{"schema":1,"event":"phase","phase":"download","total":96}
{"schema":1,"event":"file_started","path":"1.18.2.jar","total_bytes":1024}
{"schema":1,"event":"file_finished","path":"1.18.2.jar","ok":true}
{"schema":1,"event":"installed","id":"1.18.2","files":96,"bytes":1234,"reused":0}
{"schema":1,"event":"error","code":"E_USAGE","message":"...","context":{"version":"1.21.1"}}
```

Two layers: the engine's own events streamed as they happen (the same ones a Rust caller
receives through the sink), then one command event that says how it ended. A file that
was already in the content store is materialized without events, so `phase.total` and
the number of `file_finished` lines agree.

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

## 7. Known risks

| Risk | Handling |
|---|---|
| A mirror serves a stale or wrong file | SHA1 from the official document decides; mismatch is `E_NET_HASH` |
| A proxy that does not forward | `general.proxy = none`, and `doctor` shows which path is taken |
| The JVM runs with a restricted token | `doctor` probes a write and names the cause; loader remapping (fabric, neoforge) needs it. A sandbox, a CI agent, antivirus self-protection, or an executable and JDK inside a confined folder all produce this |
| A version is deleted with a game running | removal retries, then fails with a `hint` rather than a half-deleted tree |
| A modpack writes outside the game directory | entry paths are rejected unless they stay inside |
| Two Minecraft versions need different Javas | one managed runtime per major, keyed by major |
| The shared library tree is touched by another launcher | accepted: libraries are content-addressed and re-verified by SHA1 on the next install |
| A loader artifact has no published hash | length checked, written atomically, and the run reports how many such files there were |
| The portable folder is somewhere shared | the token file inherits that folder's ACL on Windows; on Unix it is `0600` regardless |
| A distribution cannot write next to itself | `E_PORTABLE_READONLY` today; a managed mode is reserved in `DataMode` and not implemented |

---

## 8. Decisions worth remembering

- **The version directory is the game directory.** Isolation by construction; a fork is
  the answer to "another world set", because that is a new version id.
- **Managed Java, keyed by major.** The host JDK is ignored by default; one runtime per
  major serves every version that needs it.
- **Content-addressed cache with hardlinks.** One copy of a jar, many directory entries,
  and an explicit `objects.txt` list rather than link counts for what may be deleted.
- **Official hashes are the only authority.** Mirrors move bytes, never trust.
- **The engine never prints and never prompts.** Interaction is the caller's job, which
  is what lets another front-end link the same crate.
- **Offline mode is first-class.** It makes the launcher testable without an account and
  is the only way to play on a LAN without Mojang's services.
