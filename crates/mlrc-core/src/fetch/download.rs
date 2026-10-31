//! The download queue: bounded concurrency, atomic writes, hash verification, length
//! checks, and a small number of retries.
//!
//! Integrity has two levels here, and a file gets one or the other:
//!
//! - a published hash (SHA1, or stronger when the source offers it) verifies the bytes;
//! - without a hash, the length is checked and the file is written through a `.part`
//!   sibling and renamed, so an interrupted transfer never leaves a short file in place.
//!
//! A hash is corruption detection, not proof of origin: it arrives over the same
//! connection as the file, so TLS is what makes it meaningful. A file with no hash
//! gets TLS and a length check, nothing more, and the run says so.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use sha1::{Digest, Sha1};
use tokio::io::AsyncWriteExt;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::events::{Event, EventSink};
use crate::fetch::HttpClient;
use crate::store::ObjectStore;

#[derive(Debug, Clone)]
pub struct DownloadTask {
    pub sha1: String,
    pub url: String,
    pub size: u64,
    /// Every place this object has to appear. Usually one, but a shared library can
    /// be referenced from several version directories.
    pub dests: Vec<PathBuf>,
    /// Client jars and libraries are fatal when missing. Asset objects are not:
    /// a handful of old sounds and icons are genuinely absent from the CDN, and a
    /// missing parrot sound must not block an install.
    pub required: bool,
}

#[derive(Debug, Default, Clone)]
pub struct DownloadSummary {
    pub files: usize,
    pub bytes: u64,
    pub reused: usize,
    /// Files whose source published no hash: length-checked and written atomically,
    /// but not verified.
    pub unverified: usize,
}

pub struct Downloader<'a> {
    pub client: &'a HttpClient,
    pub store: &'a ObjectStore,
    pub sink: &'a dyn EventSink,
    pub threads: usize,
    pub attempts: u32,
}

impl<'a> Downloader<'a> {
    pub async fn run(&self, tasks: Vec<DownloadTask>) -> MlrcResult<DownloadSummary> {
        let total = tasks.len();

        let reused = tasks
            .iter()
            .filter(|task| self.store.contains(&task.sha1, task.size))
            .count();
        let mut summary = DownloadSummary {
            reused,
            ..Default::default()
        };

        let pending = total.saturating_sub(reused);
        self.sink.emit(Event::Phase {
            phase: "download".to_string(),
            total: Some(pending as u64),
        });

        let mut stream = Box::pin(
            futures_util::stream::iter(tasks)
                .map(|task| async move {
                    let outcome = self.download_one(&task).await;
                    (task, outcome)
                })
                .buffer_unordered(self.threads.max(1)),
        );

        while let Some((task, outcome)) = stream.next().await {
            outcome?;
            summary.files += 1;
            summary.bytes += task.size;
            if task.sha1.is_empty() {
                summary.unverified += 1;
            }
        }

        if summary.unverified > 0 {
            self.sink.emit(Event::Warning {
                code: "E_NET_UNVERIFIED".to_string(),
                message: format!(
                    "{} file(s) came from a source that publishes no hash; only their length was checked",
                    summary.unverified
                ),
            });
        }

        Ok(summary)
    }

    async fn download_one(&self, task: &DownloadTask) -> MlrcResult<()> {
        let label = task
            .dests
            .first()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| task.url.clone());

        if !task.sha1.is_empty() && self.store.contains(&task.sha1, task.size) {
            for dest in &task.dests {
                self.store.materialize(&task.sha1, dest)?;
            }
            self.sink.emit(Event::FileFinished {
                path: label,
                ok: true,
                cached: true,
            });
            return Ok(());
        }

        self.sink.emit(Event::FileStarted {
            path: label.clone(),
            total_bytes: Some(task.size),
        });

        // A few loader libraries publish Maven coordinates and no hash. They cannot
        // live in the content-addressed store, so they are written straight to their
        // destinations, unverified but still retried.
        let object = if task.sha1.is_empty() {
            None
        } else {
            let object = self.store.path_for(&task.sha1);
            if let Some(parent) = object.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|err| {
                    MlrcError::new(
                        ErrorCode::Io,
                        format!("cannot create {}: {err}", parent.display()),
                    )
                })?;
            }
            Some(object)
        };

        if object.is_none() {
            if let Some(dest) = task.dests.first() {
                if let Some(parent) = dest.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|err| {
                        MlrcError::new(
                            ErrorCode::Io,
                            format!("cannot create {}: {err}", parent.display()),
                        )
                    })?;
                }
            }
        }

        let part = match &object {
            Some(object) => part_path(object),
            None => match task.dests.first() {
                Some(dest) => part_path(dest),
                None => return Ok(()),
            },
        };

        let mut last_error: Option<MlrcError> = None;
        for attempt in 0..self.attempts.max(1) {
            let result = match &object {
                Some(object) => self.fetch_to_store(task, object, &part).await,
                None => self.fetch_to_dests(task).await,
            };

            match result {
                Ok(()) => {
                    self.sink.emit(Event::FileFinished {
                        path: label,
                        ok: true,
                        cached: false,
                    });
                    return Ok(());
                }
                Err(err) => {
                    let _ = tokio::fs::remove_file(&part).await;

                    if is_not_found(&err) {
                        if task.required {
                            return Err(err);
                        }
                        self.sink.emit(Event::Warning {
                            code: "E_NET_MISSING_ASSET".to_string(),
                            message: format!("not available upstream, skipped: {}", task.url),
                        });
                        return Ok(());
                    }

                    last_error = Some(err);
                    if attempt + 1 < self.attempts {
                        let backoff = Duration::from_millis(250 * (1 << attempt));
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            MlrcError::new(ErrorCode::Network, format!("download failed: {}", task.url))
        }))
    }

    async fn fetch_to_store(
        &self,
        task: &DownloadTask,
        object: &Path,
        part: &Path,
    ) -> MlrcResult<()> {
        self.fetch_to(task, part).await?;
        finalize(part, object).await?;
        for dest in &task.dests {
            self.store.materialize(&task.sha1, dest)?;
        }
        Ok(())
    }

    async fn fetch_to_dests(&self, task: &DownloadTask) -> MlrcResult<()> {
        for dest in &task.dests {
            let part = part_path(dest);
            if let Err(err) = self.fetch_to(task, &part).await {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(err);
            }
            finalize(&part, dest).await?;
        }
        Ok(())
    }

    async fn fetch_to(&self, task: &DownloadTask, part: &Path) -> MlrcResult<()> {
        let mut response = self.client.get_response(&task.url).await?;
        let mut file = tokio::fs::File::create(part).await.map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot create {}: {err}", part.display()),
            )
        })?;

        let mut written: u64 = 0;
        let mut hasher = Sha1::new();
        while let Some(chunk) = response.chunk().await.map_err(|err| {
            MlrcError::new(
                ErrorCode::Network,
                format!("interrupted while reading {}: {err}", task.url),
            )
        })? {
            written += chunk.len() as u64;
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(|err| {
                MlrcError::new(
                    ErrorCode::Io,
                    format!("cannot write {}: {err}", part.display()),
                )
            })?;
        }
        file.flush().await.ok();
        // Flush to the device before the rename: the rename is what makes the file
        // visible, and it must not make a half-written one visible after a crash.
        file.sync_all().await.ok();

        // A source that publishes a length gets it checked; a truncated response is
        // the common failure, and it is worth a clear error rather than a hash one.
        if task.size > 0 && written != task.size {
            return Err(MlrcError::new(
                ErrorCode::SizeMismatch,
                format!(
                    "length mismatch for {}: expected {} bytes, got {written}",
                    task.url, task.size
                ),
            )
            .with("url", task.url.clone())
            .with("expected", task.size)
            .with("actual", written));
        }

        if !task.sha1.is_empty() {
            let actual = hex(&hasher.finalize());
            if actual != task.sha1 {
                return Err(MlrcError::new(
                    ErrorCode::HashMismatch,
                    format!(
                        "hash mismatch for {}: expected {}, got {actual}",
                        task.url, task.sha1
                    ),
                )
                .with("url", task.url.clone())
                .with("expected", task.sha1.clone())
                .with("actual", actual));
            }
        }

        Ok(())
    }
}

/// Moves the finished `.part` file into place.
///
/// On Unix the rename replaces the destination. Windows refuses to, so an existing
/// file is removed first; the window between the two is a few microseconds and the
/// only alternative is leaving a stale file behind.
async fn finalize(part: &Path, dest: &Path) -> MlrcResult<()> {
    match tokio::fs::rename(part, dest).await {
        Ok(()) => Ok(()),
        Err(err) if exists(&err) => {
            tokio::fs::remove_file(dest).await.map_err(|remove| {
                MlrcError::new(
                    ErrorCode::Io,
                    format!("cannot replace {}: {remove}", dest.display()),
                )
            })?;
            tokio::fs::rename(part, dest).await.map_err(|err| {
                MlrcError::new(
                    ErrorCode::Io,
                    format!("cannot finalize {}: {err}", dest.display()),
                )
            })
        }
        Err(err) => Err(MlrcError::new(
            ErrorCode::Io,
            format!("cannot finalize {}: {err}", dest.display()),
        )),
    }
}

fn exists(err: &std::io::Error) -> bool {
    // 17 is EEXIST, 183 is ERROR_ALREADY_EXISTS.
    matches!(err.kind(), std::io::ErrorKind::AlreadyExists)
        || matches!(err.raw_os_error(), Some(17) | Some(183))
}

fn is_not_found(err: &MlrcError) -> bool {
    err.context.get("status").and_then(|value| value.as_u64()) == Some(404)
}

fn part_path(object: &Path) -> PathBuf {
    let mut name = object
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    object.with_file_name(name)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
