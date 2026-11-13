use memex_models::{Configuration, Provider, Snapshot};
use memex_provider::{discover, ApiCredential, DiscoveryError};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::Duration,
};

const SYNTHETIC_KEY: &str = "synthetic-discovery-key-not-a-real-credential";

fn settings(base: String) -> Snapshot {
    Snapshot {
        schema_version: 1,
        revision: 17,
        configuration: Configuration {
            providers: vec![Provider {
                id: "proxy".into(),
                label: "Synthetic".into(),
                api_base: base,
            }],
            ..Configuration::default()
        },
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn server(response: String, delay: Duration, auth: bool) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() < 8192);
        }
        let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /gateway/v1/models http/1.1\r\n"));
        assert_eq!(
            request.contains(&format!("authorization: bearer {SYNTHETIC_KEY}\r\n")),
            auth
        );
        assert!(!request.contains("content-length:"));
        thread::sleep(delay);
        // The client intentionally closes early on oversized bodies or deadline expiry.
        let _ = stream.write_all(response.as_bytes());
    });
    (format!("http://{address}/gateway/v1/"), handle)
}

fn response(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

#[test]
fn discovers_advertised_ids_with_exact_configuration_and_credential_binding() {
    let (base, handle) = server(
        response(
            r#"{"data":[{"id":"zeta","owned_by":"ignored"},{"id":"中文-model"},{"id":"alpha"},{"id":"zeta"}]}"#,
        ),
        Duration::ZERO,
        true,
    );
    let snapshot = settings(base);
    let credential = ApiCredential::new(
        &snapshot.configuration.providers[0],
        9,
        SYNTHETIC_KEY.into(),
    )
    .unwrap();
    let result = runtime()
        .block_on(discover(&snapshot, "proxy", Some(&credential)))
        .unwrap();
    handle.join().unwrap();
    assert_eq!(result.settings_revision, 17);
    assert_eq!(result.credential_generation, Some(9));
    assert_eq!(result.provider_id, "proxy");
    assert_eq!(
        result.api_base,
        snapshot.configuration.providers[0].api_base
    );
    assert_eq!(result.model_ids, ["alpha", "zeta", "中文-model"]);
    assert!(snapshot.configuration.models.is_empty());
    assert!(snapshot.configuration.selections.is_empty());

    let (base, handle) = server(response(r#"{"data":[]}"#), Duration::ZERO, false);
    let result = runtime()
        .block_on(discover(&settings(base), "proxy", None))
        .unwrap();
    handle.join().unwrap();
    assert!(result.model_ids.is_empty());
    assert_eq!(result.credential_generation, None);
}

#[test]
fn refuses_cross_provider_or_endpoint_credentials_before_network() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut snapshot = settings(format!(
        "http://{}/gateway/v1/",
        listener.local_addr().unwrap()
    ));
    let original = snapshot.configuration.providers[0].clone();
    let credential = ApiCredential::new(&original, 1, SYNTHETIC_KEY.into()).unwrap();
    snapshot.configuration.providers[0]
        .api_base
        .push_str("other/");
    assert_eq!(
        runtime()
            .block_on(discover(&snapshot, "proxy", Some(&credential)))
            .unwrap_err(),
        DiscoveryError::CredentialScopeMismatch
    );
    snapshot.configuration.providers[0] = original;
    snapshot.configuration.providers[0].id = "different-provider".into();
    assert_eq!(
        runtime()
            .block_on(discover(&snapshot, "different-provider", Some(&credential)))
            .unwrap_err(),
        DiscoveryError::CredentialScopeMismatch
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn refuses_redirect_auth_errors_malformed_or_unbounded_results_without_raw_diagnostics() {
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    destination.set_nonblocking(true).unwrap();
    let cases = vec![
        (
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{}/stolen\r\nContent-Length: 0\r\n\r\n",
                destination.local_addr().unwrap()
            ),
            DiscoveryError::RedirectRefused,
        ),
        (
            format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\n\r\n{SYNTHETIC_KEY}",
                SYNTHETIC_KEY.len()
            ),
            DiscoveryError::Unauthorized,
        ),
        (
            "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n".into(),
            DiscoveryError::Unavailable,
        ),
        (response(SYNTHETIC_KEY), DiscoveryError::InvalidResponse),
        (
            response(r#"{"data":[{"id":" bad id"}]}"#),
            DiscoveryError::InvalidResponse,
        ),
        (
            response(r#"{"data":[{"id":""}]}"#),
            DiscoveryError::InvalidResponse,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 1048577\r\n\r\n".into(),
            DiscoveryError::ResponseTooLarge,
        ),
        (
            format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{}",
                "x".repeat(1048577)
            ),
            DiscoveryError::ResponseTooLarge,
        ),
        (
            response(&format!(
                "{{\"data\":[{}]}}",
                vec![r#"{"id":"alpha"}"#; 1025].join(",")
            )),
            DiscoveryError::ResponseTooLarge,
        ),
    ];
    for (response, expected) in cases {
        let (base, handle) = server(response, Duration::ZERO, true);
        let snapshot = settings(base);
        let credential = ApiCredential::new(
            &snapshot.configuration.providers[0],
            1,
            SYNTHETIC_KEY.into(),
        )
        .unwrap();
        let error = runtime()
            .block_on(discover(&snapshot, "proxy", Some(&credential)))
            .unwrap_err();
        handle.join().unwrap();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains(SYNTHETIC_KEY));
    }
    assert_eq!(
        destination.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let (base, handle) = server(
        response(r#"{"data":[]}"#),
        Duration::from_millis(3300),
        false,
    );
    assert_eq!(
        runtime()
            .block_on(discover(&settings(base), "proxy", None))
            .unwrap_err(),
        DiscoveryError::Timeout
    );
    handle.join().unwrap();
}
