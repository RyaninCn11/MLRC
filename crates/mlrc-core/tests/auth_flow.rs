//! The Yggdrasil conversation, against a local server that speaks the protocol.
//!
//! The shapes here are the ones the reference server sends, including the failure
//! body: a 403 with `errorMessage` is the only place the reason exists, so a launcher
//! that reads the status alone cannot tell a wrong password from an expired token.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use mlrc_core::auth::yggdrasil::{self, Session};
use mlrc_core::error::ErrorCode;
use mlrc_core::fetch::mirror::Mirror;
use mlrc_core::fetch::HttpClient;
use mlrc_core::port::DataRoot;

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn data_dir() -> PathBuf {
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/it")
        .join(format!("auth-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create");
    path
}

fn client() -> HttpClient {
    // "none" keeps a proxy configured in the environment out of the picture.
    HttpClient::with_proxy(Mirror::Official, "none").expect("client")
}

struct Server {
    base: String,
}

impl Server {
    /// `routes` maps a path to the status and body it answers with.
    fn start(routes: HashMap<&'static str, (u16, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let routes: HashMap<String, (u16, Vec<u8>)> = routes
            .into_iter()
            .map(|(path, (status, body))| (path.to_string(), (status, body.as_bytes().to_vec())))
            .collect();
        let routes = Arc::new(routes);

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

fn serve(stream: &mut TcpStream, routes: &HashMap<String, (u16, Vec<u8>)>) {
    let mut buffer = [0u8; 8192];
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

    let (status, body) = match routes.get(&path) {
        Some((status, body)) => (*status, body.clone()),
        None => (404, Vec::new()),
    };

    let reason = match status {
        200 => "OK",
        204 => "No Content",
        403 => "Forbidden",
        _ => "Not Found",
    };

    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
}

#[tokio::test]
async fn login_stores_the_profile_and_the_token() {
    let server = Server::start(HashMap::from([(
        "/authserver/authenticate",
        (
            200,
            r#"{"accessToken":"token-1","clientToken":"client-1","availableProfiles":[{"id":"b50ad385829d3141a2167e7d7539ba7f","name":"Steve"}],"selectedProfile":{"id":"b50ad385829d3141a2167e7d7539ba7f","name":"Steve"}}"#,
        ),
    )]));

    let session = yggdrasil::authenticate(
        &client(),
        &server.base,
        "steve@example.test",
        "secret",
        None,
    )
    .await
    .expect("authenticate");

    assert_eq!(session.name, "Steve");
    assert_eq!(session.uuid, "b50ad385-829d-3141-a216-7e7d7539ba7f");
    assert_eq!(session.access_token, "token-1");
    assert_eq!(session.client_token, "client-1");
    assert_eq!(session.server, server.base);

    // The identity the game receives is the token-backed one, not an offline name.
    let identity = session.identity();
    assert_eq!(identity.user_type, "mojang");
    assert_eq!(identity.access_token, "token-1");
}

#[tokio::test]
async fn a_wrong_password_reports_the_servers_reason() {
    let server = Server::start(HashMap::from([(
        "/authserver/authenticate",
        (
            403,
            r#"{"error":"ForbiddenOperationException","errorMessage":"Invalid credentials. Invalid username or password."}"#,
        ),
    )]));

    let error = yggdrasil::authenticate(&client(), &server.base, "steve", "wrong", None)
        .await
        .expect_err("must fail");

    assert_eq!(error.code, ErrorCode::AuthFailed);
    assert_eq!(
        error.message,
        "Invalid credentials. Invalid username or password."
    );
}

#[tokio::test]
async fn validate_says_whether_the_token_still_works() {
    let good = Server::start(HashMap::from([("/authserver/validate", (204, ""))]));
    let bad = Server::start(HashMap::from([(
        "/authserver/validate",
        (
            403,
            r#"{"error":"ForbiddenOperationException","errorMessage":"Invalid token."}"#,
        ),
    )]));

    let session = Session {
        server: good.base.clone(),
        username: "steve".to_string(),
        name: "Steve".to_string(),
        uuid: "b50ad385-829d-3141-a216-7e7d7539ba7f".to_string(),
        access_token: "token-1".to_string(),
        client_token: "client-1".to_string(),
    };

    assert_eq!(
        yggdrasil::validate(&client(), &session)
            .await
            .expect("validate"),
        Some(true)
    );

    let expired = Session {
        server: bad.base.clone(),
        ..session
    };
    assert_eq!(
        yggdrasil::validate(&client(), &expired)
            .await
            .expect("validate"),
        Some(false)
    );
}

#[tokio::test]
async fn refresh_replaces_the_token_and_can_rename_the_profile() {
    let server = Server::start(HashMap::from([(
        "/authserver/refresh",
        (
            200,
            r#"{"accessToken":"token-2","clientToken":"client-2","selectedProfile":{"id":"b50ad385829d3141a2167e7d7539ba7f","name":"Steve_2"}}"#,
        ),
    )]));

    let session = Session {
        server: server.base.clone(),
        username: "steve".to_string(),
        name: "Steve".to_string(),
        uuid: "b50ad385-829d-3141-a216-7e7d7539ba7f".to_string(),
        access_token: "token-1".to_string(),
        client_token: "client-1".to_string(),
    };

    let refreshed = yggdrasil::refresh(&client(), &session)
        .await
        .expect("refresh");
    assert_eq!(refreshed.access_token, "token-2");
    assert_eq!(refreshed.client_token, "client-2");
    assert_eq!(refreshed.name, "Steve_2");
    assert_eq!(refreshed.uuid, "b50ad385-829d-3141-a216-7e7d7539ba7f");
}

#[tokio::test]
async fn a_stored_session_survives_a_restart() {
    let base = data_dir();
    let root = DataRoot::resolve(Some(&base)).expect("root");
    root.ensure().expect("ensure");

    assert!(Session::load(&root).expect("load").is_none());

    let session = Session {
        server: "https://example.invalid/api/yggdrasil".to_string(),
        username: "steve@example.test".to_string(),
        name: "Steve".to_string(),
        uuid: "b50ad385-829d-3141-a216-7e7d7539ba7f".to_string(),
        access_token: "token-1".to_string(),
        client_token: "client-1".to_string(),
    };
    session.save(&root).expect("save");

    let loaded = Session::load(&root).expect("load").expect("present");
    assert_eq!(loaded.name, "Steve");
    assert_eq!(loaded.access_token, "token-1");
    // The write goes through a temporary file and leaves nothing behind.
    assert!(!Session::file(&root).with_extension("json.part").exists());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_newest_injector_is_the_one_used() {
    let base = data_dir();
    let root = DataRoot::resolve(Some(&base)).expect("root");
    root.ensure().expect("ensure");

    assert!(yggdrasil::cached_injector(&root).expect("lookup").is_none());

    let dir = root.injector_dir();
    std::fs::create_dir_all(&dir).expect("dir");
    for build in [10u64, 42, 7] {
        std::fs::write(dir.join(format!("authlib-injector-{build}.jar")), b"jar").expect("write");
    }
    std::fs::write(dir.join("notes.txt"), b"ignore me").expect("write");

    let injector = yggdrasil::cached_injector(&root)
        .expect("lookup")
        .expect("found");
    assert_eq!(injector.build, 42);
    assert_eq!(
        injector.argument("https://example.invalid/api/yggdrasil"),
        format!(
            "-javaagent:{}={}",
            injector.path.display(),
            "https://example.invalid/api/yggdrasil"
        )
    );

    let _ = std::fs::remove_dir_all(&base);
}
