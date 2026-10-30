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
    /// Uses the system/environment proxy, which is what reqwest does by default.
    pub fn new(mirror: Mirror) -> MlrcResult<Self> {
        Self::with_proxy(mirror, "auto")
    }

    /// `setting` is `general.proxy`: `auto`, `none` or `host:port`.
    pub fn with_proxy(mirror: Mirror, setting: &str) -> MlrcResult<Self> {
        let mut builder = reqwest::Client::builder()
            .user_agent(format!("mlrc/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120));

        builder = match crate::fetch::proxy::choice(setting)? {
            crate::fetch::proxy::Choice::Auto => builder,
            // A proxy that is configured but dead breaks every request, and there is
            // no way to notice from inside reqwest; this is the escape hatch.
            crate::fetch::proxy::Choice::Direct => builder.no_proxy(),
            crate::fetch::proxy::Choice::Manual(proxy) => {
                let url = format!("http://{}:{}", proxy.host, proxy.port);
                builder.proxy(reqwest::Proxy::all(&url).map_err(|err| {
                    MlrcError::new(ErrorCode::Config, format!("cannot use proxy {url}: {err}"))
                })?)
            }
        };

        let inner = builder.build().map_err(|err| {
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

    /// JSON from an API that authenticates with headers of its own (CurseForge's
    /// `x-api-key`). No mirror rewriting: the API has no mirrored equivalent.
    pub async fn get_json_with_headers<T: DeserializeOwned>(
        &self,
        url: &str,
        query: &[(&str, String)],
        headers: &[(&str, String)],
        timeout: std::time::Duration,
    ) -> MlrcResult<T> {
        let mut request = self
            .inner
            .get(url)
            .header(
                reqwest::header::USER_AGENT,
                crate::mods::modrinth::user_agent(),
            )
            .timeout(timeout);

        for (name, value) in headers {
            request = request.header(*name, value);
        }

        let response = request
            .query(query)
            .send()
            .await
            .map_err(|err| net_error(url, &err))?;

        if !response.status().is_success() {
            return Err(http_error(url, response.status()));
        }

        let bytes = response.bytes().await.map_err(|err| net_error(url, &err))?;
        serde_json::from_slice(&bytes).map_err(|err| {
            MlrcError::new(
                ErrorCode::MetaShape,
                format!("cannot parse json from {url}: {err}"),
            )
            .with("url", url)
        })
    }

    /// POST a JSON body and return the status with the raw answer.
    ///
    /// Yggdrasil reports failures in the body with a 4xx status, so the caller needs
    /// both halves: the status alone loses the reason, and the body alone loses
    /// whether it was a failure at all.
    pub async fn post_json_raw(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> MlrcResult<(u16, Vec<u8>)> {
        let response = self
            .inner
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .await
            .map_err(|err| net_error(url, &err))?;

        let status = response.status().as_u16();
        let bytes = response.bytes().await.map_err(|err| net_error(url, &err))?;
        Ok((status, bytes.to_vec()))
    }

    /// JSON with query parameters. No mirror rewriting: this is for APIs that have
    /// no mirrored equivalent (Modrinth), and reqwest encodes the values properly.
    pub async fn get_json_query<T: DeserializeOwned>(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> MlrcResult<T> {
        let response = self
            .inner
            .get(url)
            .header(
                reqwest::header::USER_AGENT,
                crate::mods::modrinth::user_agent(),
            )
            .query(query)
            .send()
            .await
            .map_err(|err| net_error(url, &err))?;

        if !response.status().is_success() {
            return Err(http_error(url, response.status()));
        }

        let bytes = response.bytes().await.map_err(|err| net_error(url, &err))?;
        serde_json::from_slice(&bytes).map_err(|err| {
            MlrcError::new(
                ErrorCode::MetaShape,
                format!("cannot parse json from {url}: {err}"),
            )
            .with("url", url)
        })
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
