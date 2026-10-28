//! The download queue: bounded concurrency, atomic writes, SHA1 verification,
//! and a small number of retries. Everything lands in the object store first.

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
        }

        Ok(summary)
    }

    async fn download_one(&self, task: &DownloadTask) -> MlrcResult<()> {
        let label = task
            .dests
            .first()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| task.url.clone());

        if self.store.contains(&task.sha1, task.size) {
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

        let object = self.store.path_for(&task.sha1);
        if let Some(parent) = object.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                MlrcError::new(
                    ErrorCode::Io,
                    format!("cannot create {}: {err}", parent.display()),
                )
            })?;
        }
        let part = part_path(&object);

        let mut last_error: Option<MlrcError> = None;
        for attempt in 0..self.attempts.max(1) {
            match self.fetch_to(task, &part).await {
                Ok(()) => {
                    tokio::fs::rename(&part, &object).await.map_err(|err| {
                        MlrcError::new(
                            ErrorCode::Io,
                            format!("cannot finalize {}: {err}", object.display()),
                        )
                    })?;
                    for dest in &task.dests {
                        self.store.materialize(&task.sha1, dest)?;
                    }
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

    async fn fetch_to(&self, task: &DownloadTask, part: &Path) -> MlrcResult<()> {
        let mut response = self.client.get_response(&task.url).await?;
        let mut file = tokio::fs::File::create(part).await.map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot create {}: {err}", part.display()),
            )
        })?;

        let mut hasher = Sha1::new();
        while let Some(chunk) = response.chunk().await.map_err(|err| {
            MlrcError::new(
                ErrorCode::Network,
                format!("interrupted while reading {}: {err}", task.url),
            )
        })? {
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(|err| {
                MlrcError::new(
                    ErrorCode::Io,
                    format!("cannot write {}: {err}", part.display()),
                )
            })?;
        }
        file.flush().await.ok();

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
