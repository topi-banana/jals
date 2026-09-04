#![cfg(feature = "native")]
//! The native adapter's own half of the retry: that a real HTTP status is classified the way the
//! loop needs it to be.
//!
//! Every other retry test drives a double, so this is the only place `reqwest`'s error is actually
//! produced and read. It serves the statuses from a socket rather than mocking the client, because
//! what is under test is precisely the translation from a `reqwest::Error` into a
//! [`FetchError`](jals_classpath::FetchError) — the one line where the classification exists.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use jals_classpath::{
    ExpectedDigest, ExternalArtifactResolver, ExternalArtifactSpec, ExternalLocator, NetworkPolicy,
    ReqwestFetcher, RetrySchedule,
};
use jals_storage::{CacheNamespace, CodeTree, ContentDigest, MemoryStorage};

const BODY: &[u8] = b"the artifact";

const UNAVAILABLE: &str =
    "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const MISSING: &str = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const SERVED: &str =
    "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nthe artifact";

/// A socket that answers each connection with the next scripted response and then closes.
///
/// One connection per attempt (`Connection: close` refuses keep-alive), so the connection count
/// *is* the attempt count — which is the assertion, and one no mock of the client could make.
struct ScriptedServer {
    locator: String,
    connections: Arc<AtomicUsize>,
}

impl ScriptedServer {
    fn start(script: &'static [&'static str]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("a bound address").port();
        let connections = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&connections);
        thread::spawn(move || {
            for response in script {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                Self::drain_request(&stream);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        Self {
            locator: format!("http://127.0.0.1:{port}/artifact.jar"),
            connections,
        }
    }

    /// Read the request head. A `GET` has no body, so the blank line ends it; without this the
    /// client can see the response as a reset rather than as a status.
    fn drain_request(stream: &std::net::TcpStream) {
        let Ok(clone) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(clone);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) if line == "\r\n" || line == "\n" => return,
                Ok(_) => {}
            }
        }
    }

    fn attempts(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    fn spec(&self) -> ExternalArtifactSpec {
        ExternalArtifactSpec {
            locator: ExternalLocator::new(self.locator.clone()),
            expected: ExpectedDigest::Sha256(ContentDigest::of(BODY)),
            max_bytes: 1024,
            namespace: CacheNamespace::BuildTaskArtifact,
        }
    }
}

#[test]
fn a_503_is_retried_and_the_next_response_is_served() {
    let server = ScriptedServer::start(&[UNAVAILABLE, SERVED]);
    jals_exec::tokio_rt::run(|_exec| async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = ReqwestFetcher::for_project(
            std::env::current_dir().expect("a working directory"),
            NetworkPolicy::Online,
            RetrySchedule::new(1),
        );
        let key = ExternalArtifactResolver::resolve(
            &fetcher,
            storage.artifacts_mut(),
            &server.spec(),
            &jals_progress::Progress::SILENT,
        )
        .await
        .expect("the second attempt serves the body");
        assert_eq!(key.content(), ContentDigest::of(BODY));
    })
    .expect("the native runtime bootstraps");

    assert_eq!(server.attempts(), 2, "the 503 must have cost one retry");
}

#[test]
fn a_404_is_not_retried() {
    let server = ScriptedServer::start(&[MISSING, SERVED]);
    let error = jals_exec::tokio_rt::run(|_exec| async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let fetcher = ReqwestFetcher::for_project(
            std::env::current_dir().expect("a working directory"),
            NetworkPolicy::Online,
            RetrySchedule::new(3),
        );
        ExternalArtifactResolver::resolve(
            &fetcher,
            storage.artifacts_mut(),
            &server.spec(),
            &jals_progress::Progress::SILENT,
        )
        .await
        .expect_err("a 404 is not something another attempt fixes")
    })
    .expect("the native runtime bootstraps");

    assert_eq!(server.attempts(), 1, "a 404 must be asked exactly once");
    assert!(error.contains("404"), "{error}");
    // One attempt renders exactly as it did before there was a loop.
    assert!(!error.contains("attempts"), "{error}");
}
