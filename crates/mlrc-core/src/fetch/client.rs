use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::fetch::mirror::Mirror;

/// Thin wrapper over `reqwest`. It is also the single place where a mirror is
/// applied, so no caller has to remember to rewrite its own URLs.
pub struct HttpClient {
    inner: reqwest::Client,
    mirror: Mirror,
}

impl HttpClient {
    pub fn new(mirror: Mirror) -> MlrcResult<Self> {
        let inner = reqwest::Client::builder()
            .user_agent(format!("mlrc/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|err| {
                MlrcError::new(
                    ErrorCode::Network,
                    format!("cannot build http client: {err}"),
                )
            })?;
        Ok(Self { inner, mirror })
    }

    pub const fn mirror(&self) -> Mirror {
        self.mirror
    }

    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> MlrcResult<T> {
        let bytes = self.get_bytes(url).await?;
        serde_json::from_slice(&bytes).map_err(|err| {
            MlrcError::new(
                ErrorCode::MetaShape,
                format!("cannot parse json from {url}: {err}"),
            )
            .with("url", url)
        })
    }

    pub async fn get_bytes(&self, url: &str) -> MlrcResult<Vec<u8>> {
        self.get_bytes_inner(url, None).await
    }

    /// Per-request timeout, for calls that must fail fast rather than hang: a maven
    /// host that accepts a connection and then goes quiet otherwise eats the whole
    /// 120 second budget.
    pub async fn get_bytes_with_timeout(
        &self,
        url: &str,
        timeout: std::time::Duration,
    ) -> MlrcResult<Vec<u8>> {
        self.get_bytes_inner(url, Some(timeout)).await
    }

    async fn get_bytes_inner(
        &self,
        url: &str,
        timeout: Option<std::time::Duration>,
    ) -> MlrcResult<Vec<u8>> {
        let response = self.get_response_with_timeout(url, timeout).await?;
        let bytes = response.bytes().await.map_err(|err| net_error(url, &err))?;
        Ok(bytes.to_vec())
    }

    /// Raw response, for callers that stream to disk instead of buffering.
    ///
    /// A mirror that answers with an error (BMCLAPI does not carry every
    /// `piston-data` object) or refuses the connection gets one fallback attempt
    /// against the official URL. The hash check downstream is unaffected - it runs
    /// on the bytes, wherever they came from.
    pub async fn get_response(&self, url: &str) -> MlrcResult<reqwest::Response> {
        self.get_response_with_timeout(url, None).await
    }

    async fn get_response_with_timeout(
        &self,
        url: &str,
        timeout: Option<std::time::Duration>,
    ) -> MlrcResult<reqwest::Response> {
        let target = self.mirror.rewrite(url);
        let mirrored = target != url;

        match self.send(&target, timeout).await {
            Ok(response) if response.status().is_success() => Ok(response),
            Ok(response) if mirrored => {
                drop(response);
                match self.send(url, timeout).await {
                    Ok(origin) if origin.status().is_success() => Ok(origin),
                    Ok(origin) => Err(http_error(url, origin.status())),
                    Err(err) => Err(err),
                }
            }
            Ok(response) => Err(http_error(&target, response.status())),
            Err(err) if mirrored => match self.send(url, timeout).await {
                Ok(origin) if origin.status().is_success() => Ok(origin),
                Ok(origin) => Err(http_error(url, origin.status())),
                Err(_) => Err(err),
            },
            Err(err) => Err(err),
        }
    }

    async fn send(
        &self,
        url: &str,
        timeout: Option<std::time::Duration>,
    ) -> MlrcResult<reqwest::Response> {
        let mut request = self.inner.get(url);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        request.send().await.map_err(|err| net_error(url, &err))
    }
}

fn http_error(url: &str, status: reqwest::StatusCode) -> MlrcError {
    MlrcError::new(ErrorCode::Network, format!("http {status} for {url}"))
        .with("url", url)
        .with("status", status.as_u16())
}

fn net_error(url: &str, err: &reqwest::Error) -> MlrcError {
    let kind = if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connect"
    } else if err.is_decode() {
        "decode"
    } else {
        "other"
    };
    MlrcError::new(
        ErrorCode::Network,
        format!("request failed for {url}: {err}"),
    )
    .with("url", url)
    .with("kind", kind)
}
