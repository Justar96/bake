//! What the updater's proof tests own: a release host on the loopback
//! interface, ephemeral signing keys, and fake release archives.
//!
//! Nothing here is a product seam. The tests sign with keys they generate
//! and serve over `http` to `127.0.0.1`, which `Source::allow_loopback_http`
//! permits and a release build never sets.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::digest::{SHA256, digest};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

/// A directory under the system temp directory, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A private directory for one test.
pub fn temp_dir(name: &str) -> TempDir {
    let path = std::env::temp_dir().join(format!(
        "bake-update-test-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a temporary directory");
    TempDir(path)
}

/// An ephemeral Ed25519 key pair: the tests' release owner.
pub struct Keys {
    pair: Ed25519KeyPair,
}

impl Keys {
    pub fn new() -> Self {
        let rng = SystemRandom::new();
        let document = Ed25519KeyPair::generate_pkcs8(&rng).expect("a key pair");
        Self {
            pair: Ed25519KeyPair::from_pkcs8(document.as_ref()).expect("the key pair"),
        }
    }

    /// The public half, base64 DER SubjectPublicKeyInfo, as
    /// `distribution/host/native-release-key.pub` holds it.
    pub fn public(&self) -> String {
        let mut spki = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        spki.extend_from_slice(self.pair.public_key().as_ref());
        BASE64.encode(spki)
    }

    /// A base64 signature over exactly `bytes`.
    pub fn sign(&self, bytes: &[u8]) -> String {
        BASE64.encode(self.pair.sign(bytes).as_ref())
    }
}

/// A release host on `127.0.0.1`, serving whatever the test put in it.
pub struct Server {
    address: SocketAddr,
    routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let address = listener.local_addr().expect("the port");
        let routes: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::new(Mutex::new(HashMap::new()));
        let running = Arc::new(AtomicBool::new(true));
        let thread = {
            let routes = Arc::clone(&routes);
            let running = Arc::clone(&running);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if !running.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { break };
                    answer(stream, &routes);
                }
            })
        };
        Self {
            address,
            routes,
            running,
            thread: Some(thread),
        }
    }

    /// The route base a [`crate::Source`] takes.
    pub fn base(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Serve `bytes` at `/<path>`; an unserved path answers 404.
    pub fn put(&self, path: &str, bytes: impl Into<Vec<u8>>) {
        self.routes
            .lock()
            .expect("the routes")
            .insert(format!("/{path}"), bytes.into());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        // Wake the accept loop so the thread can see the flag and stop.
        if let Ok(stream) = TcpStream::connect(self.address) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn answer(mut stream: TcpStream, routes: &Mutex<HashMap<String, Vec<u8>>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("the request"));
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    // Read and discard the headers, so the client sees a complete answer.
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 2 {
        line.clear();
    }
    let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
    let body = routes.lock().expect("the routes").get(&path).cloned();
    let response = match body {
        Some(body) => {
            let mut head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            head.extend_from_slice(&body);
            head
        }
        None => {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
    };
    let _ = stream.write_all(&response);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
    // Let the client finish reading before the socket closes.
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest);
}

/// The lowercase hex SHA-256 of `bytes`, as a manifest states it.
pub fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A manifest with one artifact per entry of `artifacts`.
pub fn manifest_json(version: &str, artifacts: &[(&str, &[u8])]) -> String {
    let entries: Vec<String> = artifacts
        .iter()
        .map(|(target, bytes)| {
            format!(
                "\"{target}\":{{\"file\":\"bake-rs-v{version}-{target}.tar.gz\",\"sha256\":\"{}\",\"size\":{}}}",
                sha256(bytes),
                bytes.len()
            )
        })
        .collect();
    format!(
        "{{\"schema\":\"bake-native-release/1\",\"channel\":\"stable\",\"version\":\"{version}\",\"published\":\"2026-10-20T00:00:00Z\",\"artifacts\":{{{}}}}}",
        entries.join(",")
    )
}

/// The script a fake release's `bin/bake` runs: the self-check record of
/// `version` on `target`, `--version`, and nothing else. `code` is the
/// status the self-check exits with, so a test can fail a candidate.
pub fn command_script(version: &str, target: &str, code: i32) -> String {
    format!(
        "#!/bin/sh\n\
         case \"$1\" in\n\
         --self-check) printf '{{\"kind\":\"bake-rs-self-check\",\"schema\":1,\"version\":\"{version}\",\"runtime\":\"rust\",\"target\":\"{target}\",\"docs\":{{}}}}\\n'; exit {code};;\n\
         --version) printf 'bake {version}\\n'; exit 0;;\n\
         esac\n\
         exit 2\n"
    )
}

/// A release archive in `pack-rust.ts`'s layout, whose binary is the
/// script above.
pub fn release_archive(version: &str, target: &str, reported: &str, code: i32) -> Vec<u8> {
    let script = command_script(reported, target, code);
    tar_gz(&[
        ("bin/bake", script.as_bytes()),
        ("LICENSE", b"MIT\n"),
        (
            "THIRD_PARTY_NOTICES.md",
            format!("# Notices for {version}\n").as_bytes(),
        ),
    ])
}

/// A gzipped tar; members under `bin/` are executable.
pub fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (path, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(if path.starts_with("bin/") {
            0o755
        } else {
            0o644
        });
        builder
            .append_data(&mut header, path, *bytes)
            .expect("a tar member");
    }
    builder
        .into_inner()
        .expect("the tar")
        .finish()
        .expect("the gzip")
}

/// Every file under `dir`, by path relative to it, so a test can prove a
/// failed run left the working install byte for byte as it was.
pub fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    collect(dir, dir, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .strip_prefix(root)
            .map(|relative| relative.to_string_lossy().into_owned())
            .unwrap_or_default();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => collect(root, &path, files),
            Ok(kind) if kind.is_symlink() => {
                let target = std::fs::read_link(&path).unwrap_or_default();
                files.push((name, target.to_string_lossy().into_owned().into_bytes()));
            }
            Ok(_) => files.push((name, std::fs::read(&path).unwrap_or_default())),
            Err(_) => {}
        }
    }
}
