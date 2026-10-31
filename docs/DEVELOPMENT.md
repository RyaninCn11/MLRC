# MLRC - development notes

How the code is organised and the parts of the metadata that shape it. The design -
what the launcher promises and why - is in [DOCS.md](DOCS.md); what a front-end may
depend on is in [API.md](API.md).

---

## 1. Crates

```
mlrc-cli  ->  mlrc-core  ->  bytes-level dependencies only (serde, reqwest, tokio, zip)
```

```
crates/mlrc-core/src/
  meta/       manifest, version document, rules, inheritance        pure
  plan/       install plan: what this machine needs                 pure
  launch/     arguments, natives, spawning                          pure + std
  fetch/      http client, download queue, mirrors, proxy
              checksum.rs: SHA1/SHA256/SHA512, strongest wins
  store/      content-addressed store, hardlink materialization
  java/       managed runtimes: discover, validate, install
  auth/       offline derivation, Yggdrasil
  mods/       sources, manifest, modpacks
  loader/     fabric/quilt profiles, forge/neoforge installers
  port/       base directory, config, per-version state
  events.rs   Event enum + EventSink trait
  install.rs  the install pipeline
  instance.rs instances
  doctor.rs   the health checks

crates/mlrc-cli/src/
  cli.rs       clap definitions
  commands.rs  one handler per command
  render.rs    human output, error formatting, token masking
  progress.rs  the sink a command runs with: bar, NDJSON, or nothing;
               json_sink.rs holds the engine-event to NDJSON mapping
```

`mlrc-core` has no clap, no indicatif, no `unsafe`, and never writes to stdout. A
front-end links it directly; that boundary is why the engine has no rendering
dependencies.

The engine reports progress through a sink:

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

`JavaRequired` is a question, not a prompt: the CLI renders `[Y/n]`, a caller with its
own UI shows a dialog, `--json` emits a line plus exit 5. The engine never reads stdin.

---

## 2. Metadata that shapes the code

| Item | Value |
|---|---|
| Manifest | `piston-meta.mojang.com/mc/game/version_manifest_v2.json` |
| Version document | `piston-meta.mojang.com/v1/packages/<sha1>/<id>.json` |
| Client jar | `piston-data.mojang.com/v1/objects/<sha1>/client.jar` |
| Libraries | `libraries.minecraft.net/<maven path>` |
| Asset index | `piston-meta.mojang.com/v1/packages/<sha1>/<index id>.json` |
| Asset object | `resources.download.minecraft.net/<first2>/<full sha1>` |
| Java runtime index | `launchermeta.mojang.com/v1/products/java-runtime/<key>/all.json` |

Four properties surprise every implementation:

1. **Version ids are not sortable.** `26.3`, `1.21.11`, `26w14a`, `1.7.10`, `b1.7.3`
   coexist. Never parse or compare them; order comes from `releaseTime`.
2. **The asset URL repeats its prefix.** The path is `<first two>/<full hash>`, not
   `<first two>/<rest of hash>`. Stripping the prefix 404s every object.
3. **`arguments.default-user-jvm` exists** since 2025, applies when the user set no JVM
   flags, and its first entry has no `rules` at all. The key is kebab-case, so a
   `rename_all = "camelCase"` model reads it as empty without complaining.
4. **Natives have two generations**, see section 6.

---

## 3. Rules

Start denied, iterate in order, the last matching rule wins; `os` and `features` inside
one rule are AND-ed. An absent rule list means always allowed, which is what most
libraries rely on.

- `os.name`: `windows`, `osx`, `linux`
- `os.arch`: `x86`, `x86_64`, `arm64`
- `os.version`: a regex, in practice a prefix such as `^10\.`; the Windows kernel version
  comes from `ver`
- `os.versionRange`: `min`/`max`, used by 26.x to pick a garbage collector

---

## 4. Inheritance

`inheritsFrom` stacks a loader on a vanilla version:

- scalars: the child wins
- `arguments`: parent then child, concatenated
- `libraries`: deduplicated by exact coordinate, child first, so a loader pinning a
  newer ASM keeps its jar ahead of the vanilla one
- `downloads`: field-level merge, child wins

Merging rewrites the pointer to whatever the parent inherited from, never to the parent
itself, or resolution never terminates. The merged document is what gets cached, and
what `mod` reads to learn the platform: an instance's id says nothing about its loader.

---

## 5. Libraries without a `downloads` block

Loader documents often publish Maven coordinates instead of artifacts:

```json
{"name": "net.fabricmc:fabric-loader:0.20.1", "url": "https://maven.fabricmc.net/"}
```

Both forms collapse into one artifact: the coordinate becomes a repository path
(`group/artifact/version/artifact-version[-classifier].jar`) appended to the base URL.
Some carry no hash; those skip the content store and are written straight to their
destination, retried but unverified.

---

## 6. Natives

Two generations. Libraries carrying `natives`/`extract` (up to 1.18) are unpacked by the
launcher; 1.19+ puts native jars on the classpath and lets LWJGL extract them. Decide
from the library entry, never from the version id.

```
versions/<id>/<id>-natives/
|-- java/     launcher-extracted natives; `-Djava.library.path` points here
|-- jna/      `-Djna.tmpdir`
|-- lwjgl/    `-Dorg.lwjgl.system.SharedLibraryExtractPath`
`-- netty/    `-Dio.netty.native.workdir`
```

Mojang's documents point all four properties at one directory; the launcher rewrites
the three that name a family so each has its own, as the launcher we imitate does.

**Everything the launcher unpacks goes into `java/`,** including LWJGL's DLLs for 1.18
and older. The family split is for libraries that unpack themselves at runtime; a
document that extracts natives loads them through `java.library.path`, and files placed
in `lwjgl/` are invisible to it. The failure is `[LWJGL] Failed to load a library`,
followed by `NoClassDefFoundError` on `RenderSystem` - there is a regression test for
exactly this.

---

## 7. Loaders

### Fabric and Quilt

`--loader fabric|quilt` fetches the profile from the loader's meta API, caches it under
the id the loader chose (`fabric-loader-<loader>-<mc>`, `quilt-loader-...`), and lets
`inheritsFrom` pull in the vanilla client, libraries and assets.

One code path: same payload shape, different host and API version - `meta.fabricmc.net/v2`
and `meta.quiltmc.org/v3`. Fabric flags stable releases; Quilt only says so in the
version string, so the newest loader is picked by preferring a release and skipping
`-beta`/`-alpha`.

### Forge and NeoForge

They patch the vanilla client through a processor chain no launcher should reimplement.
MLRC stages a launcher-shaped directory - `launcher_profiles.json` plus
`versions/<mc>/<mc>.{json,jar}` - runs the official installer with `--installClient`,
and adopts the result: the produced document goes into the version directory, its
libraries merge into the shared tree, and the vanilla libraries are hardlinked rather
than stored twice. The installer's output is kept at `MLRC/logs/installer-<id>.log`,
because it is the only explanation when one fails.

Three details make that workable:

- The installer's libraries are downloaded first, by our client, from the configured
  source. NeoForge's installer ignores `--mirror`, so on a network where its maven is
  blocked it would otherwise fail regardless.
- The child JVM gets the proxy the launcher detected (section 3 of the design notes).
- `java.io.tmpdir` is redirected into `MLRC/tmp`; a locked-down system temp directory
  otherwise breaks the installer mid-run.

`install_installer` passes `--mirror` when BMCLAPI is configured and streams the
installer's output with heartbeats, because it can be silent for minutes.

### What each loader supports

`install --list --loader` needs the coverage, not one probe per Minecraft version, so
each loader asks its own source:

- Fabric and Quilt: `/v2/versions/game` and `/v3/versions/game`, one request each.
- Forge: every maven version starts with the Minecraft version, so the prefix is the
  answer (`26.3-66.1.2`, `1.7.10-10.13.4.1614-1.7.10`).
- NeoForge: two schemes. Legacy entries are `1.20.1-47.1.106`; modern ones are
  `21.1.80` or `26.3.0.99`, where the Minecraft version is the prefix and the manifest
  decides - the longest matching id wins, so `26.1.2.124` is 26.1.2 and not 26.1.

The result is filtered against the manifest, so a repository alias never shows up as a
version that cannot be installed.

---

## 8. Mods

`mods/` holds three things: HTTP clients for the two sources, the installed-mod
manifest, and the pack importer.

- **Platform resolution** walks `inheritsFrom` (see section 4) rather than parsing ids. This is
  the only way `neoforge-21.1.265` resolves at all.
- **CurseForge** authenticates with `x-api-key` and never ships a key. Its search
  endpoint answers 403 even for keys allowed to read mods, so lookups take a numeric
  project id; a slug is rejected with the reason instead of a confusing failure.
- **Version choice**: newest first, but `release` preferred, so `mod install iris` does
  not quietly pick a beta.
- **Dependencies** are installed depth-first and de-duplicated by project, so a cycle
  cannot loop and a shared dependency lands once.
- **Files** are written through a `.part` sibling and checked against the published
  SHA1. An existing file is left alone, which makes a second install a no-op.
- **`mods/.mlrc-mods.json`** records project id and slug, version, file name and source.
  Neither site puts a project identity inside the jar, so nothing else could answer
  "which project is this?".
- **Update** compares CurseForge file ids, not release type, when deciding whether a
  file is newer; otherwise a beta would downgrade a release. A disabled mod stays
  disabled.
- **Disable** renames to `.jar.disabled`, because loaders only read `*.jar`.
- **Pack import** accepts `.mrpack` (index lists URLs) and CurseForge `.zip` (manifest
  lists ids, needs the key, skips optional files). Entry paths are rejected unless they
  stay inside the game directory, on every platform - a drive designator is rejected
  even where it is not absolute.

---

## 9. Where to change what

| Change | Files |
|---|---|
| A config key | `port/config.rs`: `KEYS`, `get`, `set`, `canonical`, then the table in the design notes |
| An error code | `error.rs`: enum, `as_str`, `exit_code`; the message shape is checked by `tests/error_style.rs` |
| An event | `events.rs`, the mapping in `mlrc-cli/src/json_sink.rs`, and `docs/API.md` |
| A CLI command | `mlrc-cli/src/cli.rs` and `commands.rs` |
| Something a user will notice | `CHANGELOG.md` under `[Unreleased]`; tagging renames that section to the version, and the release workflow refuses a tag without one |
| A loader | `loader/` and the `--loader` value list in `cli.rs` |
| A metadata quirk | `meta/`, and a note in section 2 above |
| What a hash may be | `fetch/checksum.rs`; the download queue in `fetch/download.rs` decides between hash, length and neither |
| What a front-end may rely on | `docs/API.md`, and the test that keeps the error style honest |

---

## 10. Style gates

- **ASCII-only sources.** `crates/**/*.rs` must contain no non-ASCII byte; CI greps for
  it, and the rule exists because C1 forbids translated text in the engine.
- **Error message shape.** One lowercase sentence, no trailing period, no `error:`
  prefix, no newlines, commands backticked. `tests/error_style.rs` scans every
  `MlrcError::new` literal and fails on drift; it also asserts that at least 60 literals
  were parsed, so a refactor that stops matching cannot silently pass.
- **No printing, no prompting, no `unsafe`.** `mlrc-core` has `#![forbid(unsafe_code)]`
  and renders nothing; `render.rs` is the only place that writes to the terminal.
- **Portability.** Nothing is written outside the base directory, and no path assumes a
  Windows separator or a drive letter.

---

## 11. Tests

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

| Layer | What it pins |
|---|---|
| Unit | rules and version ranges, inheritance merge, memory sizes, option rewriting, path safety, Yggdrasil response mapping, injector selection, error style, ASCII sources |
| `tests/install_flow.rs` | a real HTTP server on 127.0.0.1 with a seeded manifest: layout, bytes on disk, `installed.json`, `objects.txt`, `E_NET_HASH` on a corrupted download, store restore of a deleted file, truncated-response recovery, the event shape |
| `tests/auth_flow.rs` | a local Yggdrasil server: login, failure body as the message, validate, refresh, stored session |
| `tests/error_style.rs` | the message shape and the literal count |

Test data goes under `target/it/<pid>-<counter>/`, never `%TEMP%`, and the servers bind
`127.0.0.1:0` so parallel runs cannot collide. Tests force `proxy = none` so an
environment proxy cannot redirect them.

Not covered by CI: the paths that talk to Mojang's real hosts, and anything needing an
account. Those run by hand.

CI runs `fmt`, `clippy -D warnings` and `test` on Windows and Linux, the ASCII gate on
Linux, and a release build on Windows, Linux and macOS whose binary is uploaded as a
workflow artifact.
