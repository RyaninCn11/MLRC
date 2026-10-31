# MLRC - API contract

What a front-end may rely on. This is the part that is allowed to be depended on; the
command names and the human output are free to change, these are not.

Two ways in, and the first is the one to prefer:

| | When |
|---|---|
| Rust API (`mlrc-core`) | a program that can link the crate: a GUI, a service, another tool |
| CLI with `--json` | scripts, and front-ends written in another language |

The CLI is a reference consumer of the engine, not the engine's interface. Anything the
CLI can do, the engine can do; anything the engine needs, it asks for through a return
value or an event, never by reading a terminal.

---

## 1. The Rust surface

```rust
let root = DataRoot::resolve(None)?;        // --data-dir / MLRC_DATA_DIR / exe dir
root.ensure()?;                             // creates .minecraft and MLRC, proves writes

let client = HttpClient::with_proxy(Mirror::Official, "auto")?;
let sink = MySink::default();               // impl EventSink

let options = InstallOptions {
    threads: 8,
    java: JavaPolicy::Auto,                 // Auto | Install | Skip
    proxy: "auto".to_string(),
};
let state = install::install(&client, &root, "1.18.2", &options, &sink).await?;
```

| Call | Purpose |
|---|---|
| `install::install` | one version, everything it needs |
| `install::install_loader` | the same, with Fabric/Quilt/Forge/NeoForge on top |
| `install::preview` | the plan, without downloading |
| `launch::build` then `launch::spawn` | arguments, then the process |
| `mods::install`, `mods::update`, `mods::remove`, `mods::plan` | mods |
| `mods::modpack::import_modrinth` / `import_curseforge` | packs |
| `doctor::check` | the health report, as data |
| `auth::yggdrasil::{authenticate, refresh, validate}` | accounts |

Rules that hold for all of them:

- **Nothing is printed.** Progress and warnings go to the `EventSink`; return values
  carry results.
- **Nothing is prompted.** A question comes back as a value: `JavaPolicy::Auto` returns
  `E_JAVA_MISSING` with `component` and `major` in `context`, and the caller decides.
- **Nothing global is mutated.** Paths come from `DataRoot`, network settings from
  `HttpClient`, so two calls in one process do not interfere.
- **Every failure is one type.** `MlrcError { code, message, context }`; `code` is the
  contract, `message` is for humans, `context` is for machines.

---

## 2. Events

```rust
pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}
```

`emit` may be called from several worker tasks at once, so an implementation needs
interior mutability. It must not block: the download loop is waiting.

| Event | Fields | Meaning |
|---|---|---|
| `Phase` | `phase`, `total` (optional) | a new stage; `total` counts files, and is absent when unknown |
| `FileStarted` | `path`, `total_bytes` (optional) | one file begins |
| `FileProgress` | `path`, `bytes` | reserved: emitted by per-chunk progress, currently unused |
| `FileFinished` | `path`, `ok`, `cached` | one file is done; `cached` means it was already in the store |
| `JavaRequired` | `component`, `major`, `download_bytes` | a decision is needed, see section 4 |
| `Warning` | `code`, `message` | the run continues; the code is one of section 5 |
| `Note` | `message` | informational, for a log pane |
| `Done` | `message`, `ok` | the operation finished |

Ordering:

- `Phase` is emitted once per stage, before the files of that stage.
- Every `FileStarted` for a file is followed by exactly one `FileFinished` for it, even
  when the file fails: a failed file ends the operation with an error, so a front-end
  that keeps a table of in-flight files can drop the entry on either.
- `Warning` and `Note` may arrive at any point, including before the first `Phase` (a
  missing Java runtime is reported that way).
- `Done` is the last event of an operation that succeeded. A failed operation has no
  `Done`; the error is the return value.
- Events from concurrent downloads interleave. The engine does not promise an order
  across files, only within one.

---

## 3. The CLI's NDJSON

`--json` writes one JSON object per line on stdout, flushed as it is written. Every line
carries `"schema": 1`.

The CLI emits **command-level** events: one per outcome, not one per file. Per-file
progress is the Rust API's job (section 2); a front-end that wants a progress bar should link
the engine rather than scrape stdout.

| `event` | Shape | Emitted by |
|---|---|---|
| `done` | `message`, plus command-specific keys | most commands |
| `error` | `code`, `message`, `context` | any failure; also the last line before exit |
| `doctor` | `ok`, `failed`, `warned`, `check` list | `doctor`, exactly one line |
| `java_required` | `component`, `major` | `install` when a decision is needed |
| `installed` | `id`, `files`, `bytes`, `reused` | `install` |
| `plan` | the preview: id, kind, main class, files | `install --dry-run`, `mod install --dry-run` |
| `source` | `mirror`, `url` | any command that downloads through BMCLAPI |
| `launch` | `argv`, `cwd`, `main_class` | `launch` (`--dry-run` or real) |
| `version` | `id`, `type`, `channel`, `release_time`, `loader` | `install --list` |
| `config` | `key`, `value` | `config get` / `set` |
| `versions`, `instances`, `mod_search`, `mods_planned`, `mods_installed` | one array per item type | the corresponding commands |

Guarantees:

- stdout carries only NDJSON. Diagnostics, warnings and the progress bar go to stderr.
- A command ends with exactly one terminal line: `done`, the command's own success
  event, or `error`.
- An `error` line and the exit status carry the same code.
- **Secrets are masked.** `--accessToken` and similar values are printed as `***` in
  `launch` and in `-v` logs.

`schema` is the compatibility promise: a consumer that understands `1` may refuse a
different number. Fields may be added inside a schema version; existing fields keep
their names and meaning, and a removal or a change of meaning bumps the number.

---

## 4. Decisions, cancellation, recovery

**Decisions.** The engine never asks. A front-end answers in one of two ways:

- Rust: call again with `JavaPolicy::Install`, or provision the runtime itself.
- CLI: read `java_required`, then re-run with `--java=install`.

**Cancellation.** There is no cooperative cancel token yet: a caller stops an operation
by dropping the future (Rust) or killing the process (CLI). Both are safe because of
what the disk looks like at any instant:

- a download is written to `<file>.part` and renamed only when complete and verified;
- `installed.json` is written last, with `complete: false` until the assets are done;
- `objects.txt` is written with it.

**Recovery.** The next run of the same command finishes the job: complete files are
reused from the content store, `.part` files are discarded, and a version with
`complete: false` is repaired rather than trusted. Nothing asks the user to clean up by
hand.

**Interruption exit codes.** SIGINT terminates the process: Unix reports `130`, Windows
reports a system status rather than a code the launcher chooses. A front-end should
treat "no `done` line" as "did not finish", not as success.

---

## 5. Error codes

| Code | Exit | Meaning |
|---|---|---|
| `E_USAGE` | 2 | the request does not make sense |
| `E_CONFIG` | 1 | a key or value is invalid |
| `E_NET_HTTP` | 3 | the request failed: DNS, TLS, proxy, timeout, status |
| `E_NET_HASH` | 1 | the bytes do not match the published hash |
| `E_NET_SIZE` | 3 | the bytes do not match the published length |
| `E_META_SHAPE` | 6 | metadata could not be parsed |
| `E_PORTABLE_READONLY` | 7 | the base directory is not writable |
| `E_VERSION_UNKNOWN` | 8 | no such version |
| `E_VERSION_INCOMPLETE` | 8 | installed but incomplete |
| `E_JAVA_MISSING` | 5 | a decision is needed; see section 4 |
| `E_JAVA_TOO_OLD` | 5 | the only runtime found is too old |
| `E_JAVA_META_MISSING` | 5 | the document names no Java |
| `E_AUTH_FAILED` | 4 | login or token exchange failed |
| `E_LOADER_UNAVAILABLE` | 8 | the loader has no build for that version |
| `E_MOD_NOT_FOUND` | 9 | unknown project, file or mod |
| `E_MOD_CONFLICT` | 9 | two mods cannot coexist |
| `E_MOD_RESTRICTED` | 9 | the author forbids third-party downloads |
| `E_CACHE_STALE` | 1 | an install predates its object list; `cache prune` refuses |
| `E_IO` | 1 | the filesystem said no |
| `E_DOCTOR_FAILED` | 1 | at least one health check failed |

`E_NET_HASH` exits 1 rather than 3 on purpose: it is not a network condition, it is a
file that is wrong, and a retry is not the answer.

---

## 6. What is not covered

- The human output: wording, alignment, colours, the progress bar.
- Command-line flag names and subcommand shapes.
- The on-disk layout beyond what section 1 depends on (`DataRoot` owns the rest).
- Interactive prompts: they exist only in the CLI and only on a terminal.
