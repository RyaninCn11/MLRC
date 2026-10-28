//! Proxy detection for child JVMs.
//!
//! The launcher's own HTTP client knows about system and environment proxies, but a
//! `java` child does not: without `-Dhttp(s).proxyHost` it connects directly and, on
//! a machine that needs a proxy, every download just times out. Loader installers run
//! their own downloads in a child JVM, so they need the same settings.

use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proxy {
    pub host: String,
    pub port: u16,
}

/// Detected once per process: environment first, then the Windows system settings.
pub fn detect() -> Option<&'static Proxy> {
    static PROXY: OnceLock<Option<Proxy>> = OnceLock::new();
    PROXY.get_or_init(detect_uncached).as_ref()
}

/// `-Dhttp.proxyHost=...` style arguments, empty when no proxy is configured.
pub fn java_args() -> Vec<String> {
    match detect() {
        Some(proxy) => vec![
            format!("-Dhttp.proxyHost={}", proxy.host),
            format!("-Dhttp.proxyPort={}", proxy.port),
            format!("-Dhttps.proxyHost={}", proxy.host),
            format!("-Dhttps.proxyPort={}", proxy.port),
        ],
        None => Vec::new(),
    }
}

fn detect_uncached() -> Option<Proxy> {
    from_env().or_else(system_proxy)
}

fn from_env() -> Option<Proxy> {
    for key in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        if let Some(proxy) = parse(value.trim()) {
            return Some(proxy);
        }
    }
    None
}

/// Accepts `host:port`, `scheme://host:port` and the per-protocol
/// `http=host:port;https=host:port` form Windows uses.
pub fn parse(value: &str) -> Option<Proxy> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let without_scheme = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .unwrap_or(value);

    let entry = if without_scheme.contains('=') {
        // Windows writes one entry per protocol; HTTPS is the one that matters here.
        let mut https = None;
        let mut http = None;
        for part in without_scheme.split(';') {
            let Some((scheme, address)) = part.split_once('=') else {
                continue;
            };
            match scheme.trim() {
                "https" => https = Some(address.trim()),
                "http" => http = Some(address.trim()),
                _ => {}
            }
        }
        https.or(http)?
    } else {
        without_scheme.split(';').next()?.trim()
    };

    let (host, port) = entry.rsplit_once(':')?;
    Some(Proxy {
        host: host.trim().to_string(),
        port: port.trim().parse().ok()?,
    })
}

#[cfg(windows)]
fn system_proxy() -> Option<Proxy> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;

    let enabled: u32 = key.get_value("ProxyEnable").ok()?;
    if enabled == 0 {
        return None;
    }

    let server: String = key.get_value("ProxyServer").ok()?;
    parse(&server)
}

#[cfg(not(windows))]
fn system_proxy() -> Option<Proxy> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_host_and_port() {
        assert_eq!(
            parse("127.0.0.1:7890"),
            Some(Proxy {
                host: "127.0.0.1".to_string(),
                port: 7890
            })
        );
    }

    #[test]
    fn scheme_and_per_protocol_forms() {
        assert_eq!(parse("http://localhost:8080").map(|p| p.port), Some(8080));
        assert_eq!(
            parse("http=127.0.0.1:1080;https=127.0.0.1:7890").map(|p| p.port),
            Some(7890)
        );
    }

    #[test]
    fn nonsense_is_rejected() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("no-port"), None);
        assert_eq!(parse("host:not-a-port"), None);
    }
}
