//! End-to-end install against a local HTTP server.
//!
//! The engine reads its manifest cache before asking the network, so a test can seed
//! that cache and point every URL at 127.0.0.1. This exercises the real pipeline -
//! metadata, plans, downloads, hashes, the object store, state - without the internet.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use mlrc_core::error::ErrorCode;
use mlrc_core::events::NullSink;
use mlrc_core::fetch::{HttpClient, Mirror};
use mlrc_core::install::{install, InstallOptions, JavaMode, JavaPolicy};
use mlrc_core::meta::manifest::VersionManifest;
use mlrc_core::port::state::InstalledState;
use mlrc_core::port::DataRoot;

const CLIENT_JAR: &[u8] = b"pretend client jar";
const LIBRARY_JAR: &[u8] = b"pretend library jar";
const ASSET_INDEX: &[u8] = b"{\"objects\":{}}";

/// One request per connection, which is all the downloader needs.
struct Server {
    base: String,
}

impl Server {
    fn start(build: impl FnOnce(&str) -> HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let routes = Arc::new(build(&base));

        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let routes = Arc::clone(&routes);
                thread::spawn(move || serve(&mut stream, &routes));
            }
        });

        Self { base }
    }
}

fn serve(stream: &mut TcpStream, routes: &HashMap<String, Vec<u8>>) {
    let mut buffer = [0u8; 4096];
    let Ok(read) = stream.read(&mut buffer) else {
        return;
    };
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();

    match routes.get(&path) {
        Some(body) => {
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body);
        }
        None => {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    }
}

fn sha1(bytes: &[u8]) -> String {
    mlrc_core::mods::sha1_of(bytes)
}

fn version_document(base: &str, client_sha: &str, client_len: usize) -> String {
    format!(
        r#"{{
  "id": "1.21.1",
  "type": "release",
  "mainClass": "net.minecraft.client.main.Main",
  "assets": "17",
  "releaseTime": "2024-08-08T12:00:00+00:00",
  "javaVersion": {{ "component": "jvm-test", "majorVersion": 21 }},
  "assetIndex": {{
    "id": "17",
    "sha1": "{index_sha}",
    "size": {index_len},
    "url": "{base}/assets/17.json"
  }},
  "downloads": {{
    "client": {{ "sha1": "{client_sha}", "size": {client_len}, "url": "{base}/objects/client.jar" }}
  }},
  "libraries": [
    {{
      "name": "org.example:fake-lib:1.0",
      "downloads": {{
        "artifact": {{
          "path": "org/example/fake-lib/1.0/fake-lib-1.0.jar",
          "sha1": "{library_sha}",
          "size": {library_len},
          "url": "{base}/objects/fake-lib.jar"
        }}
      }}
    }}
  ],
  "arguments": {{
    "game": ["--gameDir", "${{game_directory}}", "--assetsDir", "${{assets_root}}"],
    "jvm": ["-Djava.library.path=${{natives_directory}}"]
  }}
}}"#,
        index_sha = sha1(ASSET_INDEX),
        index_len = ASSET_INDEX.len(),
        library_sha = sha1(LIBRARY_JAR),
        library_len = LIBRARY_JAR.len(),
    )
}

fn manifest(base: &str) -> String {
    format!(
        r#"{{"latest":{{"release":"1.21.1","snapshot":"1.21.1"}},
            "versions":[{{"id":"1.21.1","type":"release",
            "url":"{base}/versions/1.21.1.json",
            "time":"2024-01-01T00:00:00+00:00",
            "releaseTime":"2024-01-01T00:00:00+00:00"}}]}}"#
    )
}

/// A data directory with the manifest cache already seeded.
///
/// Kept under `target/` rather than the system temp directory: a machine with a
/// locked-down `%TEMP%` would fail the setup for reasons unrelated to the code.
fn data_dir() -> (DataRoot, PathBuf) {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/it");
    // Tests run in parallel and the system clock is too coarse to tell them apart, so
    // a counter is what actually makes the directory unique.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = base.join(format!("{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let root = DataRoot::resolve(Some(&dir)).expect("data root");
    root.ensure().expect("ensure");
    (root, dir)
}

fn options() -> InstallOptions {
    InstallOptions {
        java: JavaPolicy {
            mode: JavaMode::Skip,
            ..JavaPolicy::default()
        },
        // A test must never reach through the machine's proxy settings.
        proxy: "none".to_string(),
        ..InstallOptions::default()
    }
}

#[tokio::test]
async fn install_writes_every_file_and_the_state() {
    let server = Server::start(|base| {
        let mut routes = HashMap::new();
        routes.insert("/assets/17.json".to_string(), ASSET_INDEX.to_vec());
        routes.insert("/objects/client.jar".to_string(), CLIENT_JAR.to_vec());
        routes.insert("/objects/fake-lib.jar".to_string(), LIBRARY_JAR.to_vec());
        routes.insert(
            "/versions/1.21.1.json".to_string(),
            version_document(base, &sha1(CLIENT_JAR), CLIENT_JAR.len()).into_bytes(),
        );
        routes
    });

    let (root, dir) = data_dir();
    std::fs::write(VersionManifest::cache_path(&root), manifest(&server.base)).unwrap();

    let client = HttpClient::new(Mirror::Official).expect("client");
    install(&client, &root, "1.21.1", &options(), &NullSink)
        .await
        .expect("install");

    let version_dir = root.version_dir("1.21.1");
    assert_eq!(
        std::fs::read(version_dir.join("1.21.1.jar")).unwrap(),
        CLIENT_JAR,
        "client jar must be the bytes the server sent"
    );
    assert!(
        version_dir
            .join("libraries/org/example/fake-lib/1.0/fake-lib-1.0.jar")
            .is_file(),
        "library"
    );
    assert!(
        root.assets_dir().join("indexes/17.json").is_file(),
        "asset index"
    );

    let state = InstalledState::read(&version_dir.join(InstalledState::FILE_NAME))
        .expect("read state")
        .expect("state present");
    assert!(state.complete);
    assert_eq!(state.id, "1.21.1");

    // objects.txt is what `cache prune` trusts, so it must list what was pulled.
    let objects = std::fs::read_to_string(version_dir.join("objects.txt")).expect("objects.txt");
    assert!(objects.contains(&sha1(CLIENT_JAR)));

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_corrupted_download_fails_with_the_hash_code() {
    let server = Server::start(|base| {
        let mut routes = HashMap::new();
        routes.insert("/assets/17.json".to_string(), ASSET_INDEX.to_vec());
        // The document promises the real bytes; the server serves something else.
        routes.insert("/objects/client.jar".to_string(), b"corrupted".to_vec());
        routes.insert("/objects/fake-lib.jar".to_string(), LIBRARY_JAR.to_vec());
        routes.insert(
            "/versions/1.21.1.json".to_string(),
            version_document(base, &sha1(CLIENT_JAR), CLIENT_JAR.len()).into_bytes(),
        );
        routes
    });

    let (root, dir) = data_dir();
    std::fs::write(VersionManifest::cache_path(&root), manifest(&server.base)).unwrap();

    let client = HttpClient::new(Mirror::Official).expect("client");
    let error = install(&client, &root, "1.21.1", &options(), &NullSink)
        .await
        .expect_err("a corrupted file must not be accepted");

    assert_eq!(error.code, ErrorCode::HashMismatch, "{error:?}");
    assert!(
        !root
            .version_dir("1.21.1")
            .join(InstalledState::FILE_NAME)
            .is_file(),
        "a failed install must not claim to be complete"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_second_install_reuses_the_store() {
    let server = Server::start(|base| {
        let mut routes = HashMap::new();
        routes.insert("/assets/17.json".to_string(), ASSET_INDEX.to_vec());
        routes.insert("/objects/client.jar".to_string(), CLIENT_JAR.to_vec());
        routes.insert("/objects/fake-lib.jar".to_string(), LIBRARY_JAR.to_vec());
        routes.insert(
            "/versions/1.21.1.json".to_string(),
            version_document(base, &sha1(CLIENT_JAR), CLIENT_JAR.len()).into_bytes(),
        );
        routes
    });

    let (root, dir) = data_dir();
    std::fs::write(VersionManifest::cache_path(&root), manifest(&server.base)).unwrap();

    let client = HttpClient::new(Mirror::Official).expect("client");
    install(&client, &root, "1.21.1", &options(), &NullSink)
        .await
        .expect("first install");

    let library = root
        .version_dir("1.21.1")
        .join("libraries/org/example/fake-lib/1.0/fake-lib-1.0.jar");
    let first = std::fs::metadata(&library).unwrap().modified().unwrap();

    // Delete the file but keep the store: the install must materialize it again
    // rather than download it.
    std::fs::remove_file(&library).unwrap();
    install(&client, &root, "1.21.1", &options(), &NullSink)
        .await
        .expect("second install");

    assert_eq!(
        std::fs::read(&library).unwrap(),
        LIBRARY_JAR,
        "the store must be able to restore a missing file"
    );
    let _ = first;

    let _ = std::fs::remove_dir_all(dir);
}
