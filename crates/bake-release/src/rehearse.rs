//! An install rehearsal on this host: a release archive is published to a
//! loopback host under a throwaway key and installed by the shipped
//! `distribution/host/install.sh`. The installed command must answer
//! `--version` and pass its own `--self-check`. A second, fixture version
//! then takes over, leaving `previous` naming the real release, whose
//! binary must still pass its self-check: the release a rollback returns
//! to.
//!
//! The key reaches the installer by replacing the one embedded line in a
//! private copy, never through its environment: the published installer
//! trusts exactly the key it carries.
//!
//! Ported from Bake's `scripts/release/native-rehearsal.native.ts` and
//! `native-install-fixture.ts` (revision 97ce6abba0).

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use bake_update::manifest::{Artifact, archive_name};

use crate::archive::{Member, Source, write_tar_gz};
use crate::manifest::{SigningKey, manifest_bytes, published_at};
use crate::util::{Result, Scratch, read_text, sha256_file};

/// The longest one installer run may take.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// A loopback HTTP host serving `files` by path, stopped when dropped.
struct Host {
    origin: String,
    files: Files,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Host {
    fn start() -> Result<Self> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| format!("loopback: {error}"))?;
        let origin = format!(
            "http://{}",
            listener.local_addr().map_err(|error| error.to_string())?
        );
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let files: Files = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (served, stopping) = (Arc::clone(&files), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let files = Arc::clone(&served);
                        std::thread::spawn(move || {
                            let _ = serve(stream, &files);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            origin,
            files,
            stop,
            thread: Some(thread),
        })
    }

    fn publish(&self, files: HashMap<String, Vec<u8>>) {
        if let Ok(mut served) = self.files.lock() {
            *served = files;
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Answers one HTTP/1.1 request with the file at its path, or 404.
fn serve(stream: std::net::TcpStream, files: &Files) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let body = files
        .lock()
        .ok()
        .and_then(|files| files.get(&path).cloned());
    let mut stream = stream;
    match body {
        Some(body) => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            stream.write_all(&body)?;
        }
        None => stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found",
        )?,
    }
    stream.flush()
}

/// A private copy of the shipped installer that trusts `key`.
fn installer_trusting(root: &Path, key: &str, dir: &Path) -> Result<PathBuf> {
    let source = read_text(&root.join("distribution/host/install.sh"))?;
    let mut replaced = 0;
    let lines: Vec<String> = source
        .lines()
        .map(|line| {
            if line.starts_with("native_release_key='") && line.ends_with('\'') {
                replaced += 1;
                format!("native_release_key='{key}'")
            } else {
                line.to_owned()
            }
        })
        .collect();
    if replaced != 1 {
        return Err("install.sh has no single embedded native release key line".to_owned());
    }
    let path = dir.join("install.sh");
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).map_err(|error| error.to_string())?;
    Ok(path)
}

/// The served files for one release: its signed manifest and its archive.
fn release_files(
    key: &SigningKey,
    version: &str,
    target: &str,
    archive: &Path,
) -> Result<HashMap<String, Vec<u8>>> {
    let (sha256, size) = sha256_file(archive)?;
    let file = archive_name(version, target);
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        target.to_owned(),
        Artifact {
            file: file.clone(),
            sha256,
            size,
        },
    );
    let bytes = manifest_bytes(version, &published_at(SystemTime::now()), &artifacts);
    let signature = key.sign(&bytes);
    let mut files = HashMap::new();
    files.insert("/native/stable.json".to_owned(), bytes);
    files.insert(
        "/native/stable.json.sig".to_owned(),
        format!("{signature}\n").into_bytes(),
    );
    files.insert(
        format!("/native/releases/{version}/{file}"),
        std::fs::read(archive).map_err(|error| error.to_string())?,
    );
    Ok(files)
}

/// A fixture release whose `bin/bake` is a script answering `--version`
/// and `--self-check` as the real binary does.
fn stub_release(dir: &Path, version: &str, target: &str) -> Result<PathBuf> {
    let record = serde_json::json!({
        "kind": "bake-self-check",
        "schema": 1,
        "version": version,
        "runtime": "rust",
        "target": target,
        "docs": {"cli": "f".repeat(64)},
    })
    .to_string();
    let script = format!(
        "#!/bin/sh\ncase ${{1:-}} in\n  --self-check) printf '%s\\n' '{record}' ;;\n  --version) printf 'bake %s\\n' '{version}' ;;\n  *) printf 'bake %s\\n' '{version}' ;;\nesac\n"
    );
    let members = vec![
        Member {
            path: "bin/bake".to_owned(),
            mode: 0o755,
            source: Source::Bytes(script.into_bytes()),
        },
        Member {
            path: "LICENSE".to_owned(),
            mode: 0o644,
            source: Source::Bytes(b"MIT\n".to_vec()),
        },
        Member {
            path: "THIRD_PARTY_NOTICES.md".to_owned(),
            mode: 0o644,
            source: Source::Bytes(b"# Notices\n".to_vec()),
        },
        Member {
            path: "notices/bake-cli/NOTICE".to_owned(),
            mode: 0o644,
            source: Source::Bytes(b"bake-cli\n".to_vec()),
        },
        Member {
            path: "licenses/inventory.json".to_owned(),
            mode: 0o644,
            source: Source::Bytes(b"{\"schema\":\"bake-native-licenses/1\"}\n".to_vec()),
        },
        Member {
            path: format!("licenses/{target}/THIRD-PARTY-LICENSES.txt"),
            mode: 0o644,
            source: Source::Bytes(b"licenses\n".to_vec()),
        },
    ];
    let path = dir.join(archive_name(version, target));
    write_tar_gz(&members, &path, 0)?;
    Ok(path)
}

/// What one command printed.
struct Ran {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs `program` with exactly `env` and `PATH`, bounded by `timeout`.
fn run_bounded(
    program: &Path,
    args: &[&str],
    env: &[(&str, &Path)],
    timeout: Duration,
) -> Result<Ran> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("{} could not start: {error}", program.display()))?;
    let take = |stream: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_end(&mut out);
            }
            out
        })
    };
    use std::io::Read as _;
    let stdout = take(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    );
    let stderr = take(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{} did not finish within {timeout:?}",
                program.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    Ok(Ran {
        status: status.code(),
        stdout: String::from_utf8_lossy(&stdout.join().unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned(),
    })
}

/// Where a symlink points, as written.
fn link(path: &Path) -> Result<String> {
    std::fs::read_link(path)
        .map(|target| target.to_string_lossy().into_owned())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// The next patch version after `version`'s, keeping any pre-release.
fn next_patch(version: &str) -> String {
    let (core, rest) = version
        .split_once('-')
        .map_or((version, ""), |(c, r)| (c, r));
    let mut parts: Vec<u64> = core
        .split('.')
        .filter_map(|part| part.parse().ok())
        .collect();
    if let Some(patch) = parts.last_mut() {
        *patch += 1;
    }
    let next = parts
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".");
    if rest.is_empty() {
        next
    } else {
        format!("{next}-{rest}")
    }
}

/// Rehearses installing `archive`, this checkout's release for this host.
pub fn rehearse(root: &Path, version: &str, archive: &Path) -> Result<String> {
    if !cfg!(target_os = "linux") {
        return Err("The install rehearsal runs on the Linux release platforms".to_owned());
    }
    let target = bake_update::manifest::host_target()
        .ok_or_else(|| "this host is not a Bake release platform".to_owned())?;
    let file = archive_name(version, target);
    if archive.file_name().and_then(|name| name.to_str()) != Some(file.as_str()) {
        return Err(format!(
            "{} is not {file}, this checkout's archive for this host",
            archive.display()
        ));
    }
    let scratch = Scratch::new("bake-native-rehearsal")?;
    let home = scratch.path().join("home");
    let native_root = home.join(".local/share/bake-native");
    let bin_dir = home.join(".local/bin");
    let tmp = scratch.path().join("tmp");
    for dir in [&bin_dir, &tmp] {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let key = SigningKey::ephemeral()?;
    let host = Host::start()?;
    host.publish(release_files(&key, version, target, archive)?);
    let installer = installer_trusting(root, &key.public_key(), scratch.path())?;
    let origin = PathBuf::from(&host.origin);
    let bake_home = home.join(".bake");
    let env: Vec<(&str, &Path)> = vec![
        ("HOME", &home),
        ("TMPDIR", &tmp),
        ("BAKE_DOWNLOAD_URL", &origin),
        ("BAKE_NATIVE_ROOT", &native_root),
        ("BAKE_BIN_DIR", &bin_dir),
    ];
    let sh = Path::new("sh");
    let install = |what: &str| -> Result<()> {
        let ran = run_bounded(
            sh,
            &[installer.to_string_lossy().as_ref()],
            &env,
            INSTALL_TIMEOUT,
        )?;
        if ran.status != Some(0) || !ran.stderr.is_empty() {
            return Err(format!(
                "installing {what} exited {:?}:\n{}{}",
                ran.status, ran.stdout, ran.stderr
            ));
        }
        Ok(())
    };
    install(version)?;
    if link(&native_root.join("current"))? != format!("releases/{version}") {
        return Err("the install did not make the release current".to_owned());
    }
    let command = bin_dir.join("bake");
    let as_user: Vec<(&str, &Path)> = vec![("HOME", &home), ("BAKE_HOME", &bake_home)];
    let ran = run_bounded(&command, &["--version"], &as_user, Duration::from_secs(120))?;
    if ran.status != Some(0) || !ran.stdout.contains(version) {
        return Err(format!(
            "the installed bake --version printed {:?}",
            ran.stdout
        ));
    }
    let check = |binary: &Path| -> Result<()> {
        let ran = run_bounded(
            binary,
            &["--self-check"],
            &as_user,
            Duration::from_secs(120),
        )?;
        if ran.status != Some(0) {
            return Err(format!(
                "{} --self-check exited {:?}: {}",
                binary.display(),
                ran.status,
                ran.stderr
            ));
        }
        bake_update::check::check_record(ran.stdout.trim_end(), version, target)
            .map_err(|why| format!("{} --self-check: {why}", binary.display()))
    };
    check(&command)?;
    // A fixture version takes over; the real release is what a rollback
    // returns to, and must still pass its own check.
    let next = next_patch(version);
    let stub = stub_release(scratch.path(), &next, target)?;
    host.publish(release_files(&key, &next, target, &stub)?);
    install(&next)?;
    if link(&native_root.join("current"))? != format!("releases/{next}")
        || link(&native_root.join("previous"))? != format!("releases/{version}")
    {
        return Err("the update did not keep the real release as previous".to_owned());
    }
    check(&native_root.join("releases").join(version).join("bin/bake"))?;
    Ok(format!(
        "installed {version} ({target}) from {file} with the shipped install.sh, then the fixture {next}, keeping {version} as the rollback release"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_version_follows_the_real_one() {
        assert_eq!(next_patch("0.4.2"), "0.4.3");
        assert_eq!(next_patch("1.0.9-rc.1"), "1.0.10-rc.1");
    }

    #[test]
    fn the_installer_copy_trusts_only_the_given_key() {
        let root = crate::util::repository_root();
        let scratch = Scratch::new("bake-release-installer").expect("scratch");
        let copy = installer_trusting(&root, "KEY", scratch.path()).expect("copy");
        let text = read_text(&copy).expect("text");
        assert!(text.lines().any(|line| line == "native_release_key='KEY'"));
        let shipped = read_text(&root.join("distribution/host/install.sh")).expect("shipped");
        assert_eq!(text.lines().count(), shipped.lines().count());
    }

    // The shipped installer's own version order, run by `sh`, gives the
    // answer `bake-update`'s `compare_versions` gives for every ordered pair
    // of the case table both are held to.
    #[cfg(unix)]
    #[test]
    fn the_installer_orders_versions_as_bake_update_does() {
        let root = crate::util::repository_root();
        let installer = read_text(&root.join("distribution/host/install.sh")).expect("install.sh");
        let region: Vec<&str> = installer
            .lines()
            .skip_while(|line| *line != "# BEGIN VERSION ORDER")
            .take_while(|line| *line != "# END VERSION ORDER")
            .collect();
        assert!(region.len() > 3, "install.sh has no version order region");
        let table =
            read_text(&root.join("crates/bake-update/tests/fixtures/native-version-order.txt"))
                .expect("table");
        let versions: Vec<&str> = table
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        let mut script = region.join("\n");
        script.push('\n');
        for left in &versions {
            for right in &versions {
                script.push_str(&format!(
                    "if version_newer '{left}' '{right}'; then echo yes; else echo no; fi\n"
                ));
            }
        }
        let output = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("sh");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let answers: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(ToOwned::to_owned)
            .collect();
        let mut expected = Vec::new();
        for left in &versions {
            for right in &versions {
                let newer = bake_update::version::compare_versions(left, right)
                    == std::cmp::Ordering::Greater;
                expected.push(if newer { "yes" } else { "no" }.to_owned());
            }
        }
        assert_eq!(answers.len(), expected.len());
        for (at, (got, want)) in answers.iter().zip(&expected).enumerate() {
            let (left, right) = (versions[at / versions.len()], versions[at % versions.len()]);
            assert_eq!(got, want, "version_newer {left} {right}");
        }
    }

    // The shipped installer installs a fixture release from the loopback
    // host and refuses one whose signature is another key's.
    #[test]
    fn the_shipped_installer_installs_a_signed_release_and_refuses_another_key() {
        if !cfg!(target_os = "linux") || bake_update::manifest::host_target().is_none() {
            return;
        }
        let Some(target) = bake_update::manifest::host_target() else {
            return;
        };
        let root = crate::util::repository_root();
        let scratch = Scratch::new("bake-release-install").expect("scratch");
        let archive = stub_release(scratch.path(), "9.0.0", target).expect("stub");
        let key = SigningKey::ephemeral().expect("key");
        let host = Host::start().expect("host");
        let home = scratch.path().join("home");
        let tmp = scratch.path().join("tmp");
        std::fs::create_dir_all(home.join(".local/bin")).expect("bin");
        std::fs::create_dir_all(&tmp).expect("tmp");
        let native_root = home.join(".local/share/bake-native");
        let bin_dir = home.join(".local/bin");
        let origin = PathBuf::from(&host.origin);
        let env: Vec<(&str, &Path)> = vec![
            ("HOME", &home),
            ("TMPDIR", &tmp),
            ("BAKE_DOWNLOAD_URL", &origin),
            ("BAKE_NATIVE_ROOT", &native_root),
            ("BAKE_BIN_DIR", &bin_dir),
        ];
        let other = SigningKey::ephemeral().expect("other");
        host.publish(release_files(&other, "9.0.0", target, &archive).expect("files"));
        let installer = installer_trusting(&root, &key.public_key(), scratch.path()).expect("copy");
        let ran = run_bounded(
            Path::new("sh"),
            &[installer.to_string_lossy().as_ref()],
            &env,
            INSTALL_TIMEOUT,
        )
        .expect("run");
        assert_ne!(
            ran.status,
            Some(0),
            "another key's release was installed: {}",
            ran.stdout
        );
        assert!(!native_root.join("current").exists());
        host.publish(release_files(&key, "9.0.0", target, &archive).expect("files"));
        let ran = run_bounded(
            Path::new("sh"),
            &[installer.to_string_lossy().as_ref()],
            &env,
            INSTALL_TIMEOUT,
        )
        .expect("run");
        assert_eq!(ran.status, Some(0), "{}{}", ran.stdout, ran.stderr);
        assert_eq!(
            link(&native_root.join("current")).expect("current"),
            "releases/9.0.0"
        );
    }
}
