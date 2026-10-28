//! HTTP. One client, no retries yet (M2 adds them together with the mirror layer).

pub mod client;
pub mod download;
pub mod mirror;

pub use client::HttpClient;
pub use mirror::Mirror;
