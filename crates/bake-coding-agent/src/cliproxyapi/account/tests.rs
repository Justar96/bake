//! The account's sign-in, sign-out, and refresh against temporary Bake
//! homes and loopback proxies, with fake keys only. Every startup check
//! reads the home as a new process would: a fresh registry and
//! `cli::apply_account`.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
use crate::cliproxyapi::catalog::CliProxyModel;
use crate::cliproxyapi::key::normalize_api_key;
use crate::cliproxyapi::{CliProxyEndpoints, cli_proxy_endpoints};
use crate::test_support::TempDir;

const KEY_A: &str = "sk-fake-account-a";
const KEY_B: &str = "sk-fake-account-b";

fn model(id: &str) -> CliProxyModel {
    CliProxyModel {
        id: id.into(),
        api: None,
        base_url: None,
        name: id.into(),
        context_window: Some(1000),
        max_tokens: None,
        input: None,
        reasoning_efforts: Some(vec!["low".into(), "high".into()]),
        force_adaptive_thinking: false,
    }
}

fn checked(url: &str, key: &str, models: &[&str]) -> CheckedLogin {
    let endpoints: CliProxyEndpoints = cli_proxy_endpoints(url).expect("url");
    CheckedLogin {
        endpoints,
        key: normalize_api_key(key).expect("key"),
        models: models.iter().map(|id| model(id)).collect(),
    }
}

/// What a process starting on `home` would send for `cliproxyapi`: the
/// endpoint of its first model and the key, or `None` without a route.
fn startup_pair(home: &Path) -> Option<(String, Option<String>)> {
    let registry = startup_registry(home);
    pair(&registry)
}

/// The registry a process starting on `home` would build.
fn startup_registry(home: &Path) -> ModelRegistry {
    let mut registry = ModelRegistry::load(home);
    let _ = crate::cli::apply_account(home, &mut registry);
    registry
}

/// The `cliproxyapi` endpoint and key `registry` would send.
fn pair(registry: &ModelRegistry) -> Option<(String, Option<String>)> {
    let model = registry
        .models()
        .into_iter()
        .find(|model| model.provider == CLIPROXYAPI_ID)?;
    Some((
        model.base_url,
        registry.provider_api_key(CLIPROXYAPI_ID).expect("key"),
    ))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

fn login(home: &TempDir, url: &str, key: &str) {
    CliProxyAccount::new(home.path())
        .commit_login(&checked(url, key, &["gpt-a"]))
        .expect("login");
}

fn startup_warnings(home: &TempDir) -> Vec<String> {
    let mut registry = ModelRegistry::load(home.path());
    crate::cli::apply_account(home.path(), &mut registry)
        .into_iter()
        .collect()
}

const URL_A: &str = "http://127.0.0.1:1111";
const URL_B: &str = "http://127.0.0.1:2222";

fn pair_a() -> Option<(String, Option<String>)> {
    Some((format!("{URL_A}/v1"), Some(KEY_A.into())))
}

fn pair_b() -> Option<(String, Option<String>)> {
    Some((format!("{URL_B}/v1"), Some(KEY_B.into())))
}

// A sign-in saves the key and route to auth.json and settings.json under
// one generation, and they survive a restart; settings.json never holds
// the key.
#[test]
fn a_sign_in_survives_a_restart() {
    let home = TempDir::new("account-login");
    assert_eq!(startup_pair(home.path()), None);
    let committed = CliProxyAccount::new(home.path())
        .commit_login(&checked(URL_A, KEY_A, &["gpt-a", "gpt-b"]))
        .expect("login");
    assert_eq!(committed.models, 2);
    assert!(committed.warnings.is_empty());
    assert_eq!(startup_pair(home.path()), pair_a());
    let settings = std::fs::read_to_string(home.join("settings.json")).expect("settings");
    assert!(!settings.contains(KEY_A), "the key reached settings.json");
    let route = &read_json(&home.join("settings.json"))["cliproxyapi"];
    let record = &read_json(&home.join("auth.json"))["cliproxyapi"];
    assert_eq!(record["key"], KEY_A);
    assert_eq!(route["account"], record["account"]);
    assert_eq!(route["account"].as_str().map(str::len), Some(32));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join("auth.json"))
            .expect("auth")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // `--api-key` still comes first, as for every provider.
    let mut registry = startup_registry(home.path());
    registry.set_runtime_api_key(CLIPROXYAPI_ID, "sk-fake-runtime");
    assert_eq!(
        pair(&registry).and_then(|(_, key)| key),
        Some("sk-fake-runtime".into())
    );
    let shown = format!(
        "{committed:?} {:?} {:?}",
        read_account(home.path()),
        registry
    );
    assert!(!shown.contains(KEY_A), "{shown}");
}

// A sign-in stopped between its key and its route, as a
// crash would, leaves a route and a key of different sign-ins, which no
// reader uses; the next sign-in recovers.
#[test]
fn a_sign_in_stopped_between_its_writes_leaves_the_account_off() {
    let home = TempDir::new("account-stop");
    login(&home, URL_A, KEY_A);
    CliProxyAccount::new(home.path())
        .with_inject(Inject::StopAfter(Step::Key))
        .commit_login(&checked(URL_B, KEY_B, &["gpt-a"]))
        .expect_err("stopped");
    assert_eq!(startup_pair(home.path()), None);
    let warnings = startup_warnings(&home);
    assert!(
        warnings.iter().any(|w| w.contains("different sign-ins")),
        "{warnings:?}"
    );
    login(&home, URL_B, KEY_B);
    assert_eq!(startup_pair(home.path()), pair_b());
}

// A power loss may keep either file's write and lose
// the other's; each mix is a matching pair or an account that is off.
#[test]
fn any_mix_of_two_sign_ins_files_is_a_pair_or_off() {
    let home = TempDir::new("account-mix");
    login(&home, URL_A, KEY_A);
    let (settings_a, auth_a) = (
        std::fs::read(home.join("settings.json")).expect("s"),
        std::fs::read(home.join("auth.json")).expect("a"),
    );
    login(&home, URL_B, KEY_B);
    let (settings_b, auth_b) = (
        std::fs::read(home.join("settings.json")).expect("s"),
        std::fs::read(home.join("auth.json")).expect("a"),
    );
    for (settings, auth, expected) in [
        (&settings_a, &auth_a, pair_a()),
        (&settings_a, &auth_b, None),
        (&settings_b, &auth_a, None),
        (&settings_b, &auth_b, pair_b()),
    ] {
        std::fs::write(home.join("settings.json"), settings).expect("s");
        std::fs::write(home.join("auth.json"), auth).expect("a");
        assert_eq!(startup_pair(home.path()), expected);
    }
}

// A reader that reads the route before another
// process's sign-in and the key after it finds the account off, never the
// old route with the new key.
#[test]
fn a_reader_spanning_a_sign_in_never_pairs_the_old_route_with_the_new_key() {
    let home = TempDir::new("account-reader");
    login(&home, URL_A, KEY_A);
    let view = read_account_with_gap(home.path(), || login(&home, URL_B, KEY_B));
    assert!(matches!(view, AccountView::Disabled(_)), "{view:?}");
    // The reverse order in time is a plain read of the new pair.
    let AccountView::Bound(bound) = read_account(home.path()) else {
        panic!("bound");
    };
    assert_eq!(bound.key(), KEY_B);
}

// Pi's direct `auth.json` writer replaces the record
// without the binding, so the saved route is no longer used with any key,
// on the next start or the session's next reload.
#[test]
fn a_direct_key_write_turns_the_account_off() {
    let home = TempDir::new("account-direct");
    login(&home, URL_A, KEY_A);
    let runtime = ModelRuntime::new(startup_registry(home.path()));
    assert_eq!(pair(&runtime.snapshot()), pair_a());
    AuthStorage::load(&auth_path(home.path()))
        .set_api_key(CLIPROXYAPI_ID, "sk-fake-direct")
        .expect("direct");
    assert_eq!(startup_pair(home.path()), None);
    let account = CliProxyAccount::new(home.path());
    let warning = runtime
        .try_update(|registry| account.apply_to(registry))
        .expect("apply");
    assert!(warning.is_some());
    assert_eq!(pair(&runtime.snapshot()), None);
    // A direct settings writer keeps the member: the pair still matches.
    let home = TempDir::new("account-direct-settings");
    login(&home, URL_A, KEY_A);
    SettingsStore::new(home.path())
        .set_steering_mode(bake_agent::QueueMode::All)
        .expect("other setting");
    assert_eq!(startup_pair(home.path()), pair_a());
}

// A route write that settings refuse restores the previous key, or removes
// the new key when there was none. The
// previous key comes back only while auth.json still holds this sign-in's
// own record; otherwise the account stays off.
#[test]
fn a_failed_route_write_restores_only_what_it_still_owns() {
    let home = TempDir::new("account-rollback");
    login(&home, URL_A, KEY_A);
    let error = CliProxyAccount::new(home.path())
        .with_inject(Inject::Fail(Step::Route))
        .commit_login(&checked(URL_B, KEY_B, &["gpt-b"]))
        .expect_err("refused");
    assert!(
        error.to_string().contains("previous sign-in is back"),
        "{error}"
    );
    assert!(!error.to_string().contains(KEY_B));
    assert_eq!(startup_pair(home.path()), pair_a());

    let error = CliProxyAccount::new(home.path())
        .with_inject(Inject::FailRouteAndRestore)
        .commit_login(&checked(URL_B, KEY_B, &["gpt-b"]))
        .expect_err("refused");
    assert!(error.to_string().contains("stays off"), "{error}");
    assert_eq!(startup_pair(home.path()), None);

    let home = TempDir::new("account-rollback-direct");
    login(&home, URL_A, KEY_A);
    let error = CliProxyAccount::new(home.path())
        .with_inject(Inject::DirectKeyThenFailRoute)
        .commit_login(&checked(URL_B, KEY_B, &["gpt-b"]))
        .expect_err("refused");
    assert!(error.to_string().contains("changed meanwhile"), "{error}");
    assert_eq!(
        read_json(&home.join("auth.json"))["cliproxyapi"]["key"],
        "sk-fake-direct-writer"
    );
    assert_eq!(startup_pair(home.path()), None);

    // With no account before, a failed route leaves no key behind and no
    // route.
    let home = TempDir::new("account-rollback-fresh");
    CliProxyAccount::new(home.path())
        .with_inject(Inject::Fail(Step::Route))
        .commit_login(&checked(URL_B, KEY_B, &["gpt-b"]))
        .expect_err("refused");
    assert!(
        read_json(&home.join("auth.json"))
            .get("cliproxyapi")
            .is_none()
    );
    assert_eq!(startup_pair(home.path()), None);
    // A refused key write changes nothing.
    let before = std::fs::read(home.join("auth.json")).expect("auth");
    CliProxyAccount::new(home.path())
        .with_inject(Inject::Fail(Step::Key))
        .commit_login(&checked(URL_B, KEY_B, &["gpt-b"]))
        .expect_err("refused");
    assert_eq!(std::fs::read(home.join("auth.json")).expect("auth"), before);
}

// Writes that reached their files without a directory flush are
// committed, finished, and each reported.
#[test]
fn unsynced_writes_are_committed_and_reported() {
    let home = TempDir::new("account-unsynced");
    let committed = CliProxyAccount::new(home.path())
        .with_inject(Inject::Unsynced)
        .commit_login(&checked(URL_A, KEY_A, &["gpt-a"]))
        .expect("login");
    assert_eq!(committed.warnings.len(), 2, "{:?}", committed.warnings);
    assert_eq!(startup_pair(home.path()), pair_a());
    let out = CliProxyAccount::new(home.path())
        .with_inject(Inject::Unsynced)
        .logout()
        .expect("logout");
    assert_eq!(out.warnings.len(), 2, "{:?}", out.warnings);
    assert!(out.removed_route && out.removed_key);
}

// Refused before any write: a settings.json that is not an
// object, and a models.json override, which is the person's own. A
// read-only auth.json refuses the key, so nothing changes.
#[test]
fn malformed_stores_and_a_manual_override_write_nothing() {
    let home = TempDir::new("account-malformed");
    std::fs::write(home.join("settings.json"), "[1]").expect("settings");
    CliProxyAccount::new(home.path())
        .commit_login(&checked(URL_A, KEY_A, &["gpt-a"]))
        .expect_err("refused");
    assert_eq!(
        std::fs::read_to_string(home.join("settings.json")).expect("s"),
        "[1]"
    );
    // The key was written first, but no route names its generation.
    assert_eq!(startup_pair(home.path()), None);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new("account-read-only");
        std::fs::create_dir(home.join("locked")).expect("dir");
        let auth = home.join("auth.json");
        std::fs::write(&auth, "{}").expect("auth");
        std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o400)).expect("ro");
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o500))
            .expect("read-only home");
        let refused = std::fs::write(home.join("probe"), "x").is_err();
        if refused {
            CliProxyAccount::new(home.path())
                .commit_login(&checked(URL_A, KEY_A, &["gpt-a"]))
                .expect_err("refused");
        }
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700))
            .expect("writable again");
        if refused {
            assert_eq!(std::fs::read_to_string(&auth).expect("a"), "{}");
            assert!(!home.join("settings.json").exists());
        }
    }

    let home = TempDir::new("account-override");
    std::fs::write(
        home.join("models.json"),
        r#"{ "providers": { "cliproxyapi": { "baseUrl": "http://mine.test/v1", "api": "openai-responses", "apiKey": "x", "models": [{ "id": "m" }] } } }"#,
    )
    .expect("models");
    assert_eq!(
        CliProxyAccount::new(home.path()).commit_login(&checked(URL_A, KEY_A, &["gpt-a"])),
        Err(AccountError::ManualOverride)
    );
    assert!(!home.join("auth.json").exists() && !home.join("settings.json").exists());
}

// A sign-out removes the route first, leaving `null`, then the key; a stop
// between the two leaves no route.
#[test]
fn a_sign_out_survives_a_restart() {
    let home = TempDir::new("account-logout");
    login(&home, URL_A, KEY_A);
    let out = CliProxyAccount::new(home.path()).logout().expect("logout");
    assert!(out.removed_route && out.removed_key && out.key_error.is_none());
    assert_eq!(startup_pair(home.path()), None);
    assert_eq!(
        read_json(&home.join("settings.json"))["cliproxyapi"],
        Value::Null
    );
    assert!(
        read_json(&home.join("auth.json"))
            .get("cliproxyapi")
            .is_none()
    );

    let home = TempDir::new("account-logout-never-signed-in");
    CliProxyAccount::new(home.path()).logout().expect("logout");
    assert_eq!(startup_pair(home.path()), None);

    let home = TempDir::new("account-logout-stop");
    login(&home, URL_A, KEY_A);
    CliProxyAccount::new(home.path())
        .with_inject(Inject::StopAfter(Step::SignOutRoute))
        .logout()
        .expect_err("stopped");
    assert_eq!(startup_pair(home.path()), None);

    let home = TempDir::new("account-logout-key-fails");
    login(&home, URL_A, KEY_A);
    let out = CliProxyAccount::new(home.path())
        .with_inject(Inject::Fail(Step::SignOutKey))
        .logout()
        .expect("route removed");
    assert!(out.key_error.is_some() && !out.removed_key);
    assert_eq!(startup_pair(home.path()), None);

    let home = TempDir::new("account-logout-route-fails");
    login(&home, URL_A, KEY_A);
    CliProxyAccount::new(home.path())
        .with_inject(Inject::Fail(Step::SignOutRoute))
        .logout()
        .expect_err("refused");
    assert_eq!(startup_pair(home.path()), pair_a());
}

/// A loopback proxy for one test. It records each request's head and
/// body, and answers every request with `status` and `body`. When `hold`
/// is set, it tells the first `Notify` about each request and answers
/// only after the second is notified. [`Self::stop`] ends it and waits for
/// its task. Dropping it, as a failing test does, aborts the task.
struct TestProxy {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

type Hold = (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);

impl TestProxy {
    async fn start(status: u16, body: Value, hold: Option<Hold>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let (shutdown, mut stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            loop {
                let mut socket = tokio::select! {
                    _ = &mut stopped => return,
                    accepted = listener.accept() => match accepted {
                        Ok((socket, _)) => socket,
                        Err(_) => return,
                    },
                };
                let request = tokio::select! {
                    _ = &mut stopped => return,
                    request = read_request(&mut socket) => request,
                };
                record.lock().expect("seen").push(request);
                if let Some((started, release)) = &hold {
                    started.notify_one();
                    tokio::select! {
                        _ = &mut stopped => return,
                        () = release.notified() => {}
                    }
                }
                let bytes = body.to_string().into_bytes();
                let reason = if status == 200 { "OK" } else { "Error" };
                let head = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&bytes).await;
                let _ = socket.shutdown().await;
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}"),
            seen,
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }

    /// Ends the proxy and waits until its task has.
    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.await.expect("the proxy task");
        }
    }
}

impl Drop for TestProxy {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Reads one HTTP request: the head up to its blank line, then as many
/// body bytes as `content-length` names. A peer that closes early ends it.
async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
        match socket.read(&mut buf).await {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&bytes).into_owned(),
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
        }
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < head_end + length {
        match socket.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn proxy(body: Value) -> TestProxy {
    TestProxy::start(200, body, None).await
}

fn bearers(seen: &Mutex<Vec<String>>) -> Vec<String> {
    seen.lock()
        .expect("seen")
        .iter()
        .filter_map(|head| {
            head.lines().find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("authorization: bearer ")
                    .map(str::to_owned)
            })
        })
        .collect()
}

// Refreshing the saved model list: a listed model keeps saved efforts its
// listing leaves out, a model in use stays, a dropped one goes; the
// request carries the account's key. Only the signed-in account is refused, and an aborted fetch writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_keeps_efforts_and_models_in_use() {
    let home = TempDir::new("account-refresh");
    let account = CliProxyAccount::new(home.path());
    let runtime = ModelRuntime::new(startup_registry(home.path()));
    let refreshed = refresh_account(account.clone(), runtime.clone(), Vec::new(), None)
        .await
        .expect("refresh");
    assert_eq!(refreshed.outcome, RefreshOutcome::NotSignedIn);
    let server = proxy(json!({ "data": [{ "id": "gpt-a" }, { "id": "gpt-new" }] })).await;
    let url = server.url.clone();
    account
        .commit_login(&checked(&url, KEY_A, &["gpt-a", "gpt-used", "gpt-gone"]))
        .expect("login");
    let generation = read_json(&home.join("settings.json"))["cliproxyapi"]["account"].clone();
    let refreshed = refresh_account(
        account.clone(),
        runtime.clone(),
        vec!["gpt-used".into()],
        None,
    )
    .await
    .expect("refresh");
    assert_eq!(refreshed.outcome, RefreshOutcome::Updated(3));
    let route = &read_json(&home.join("settings.json"))["cliproxyapi"];
    let ids: Vec<&str> = route["models"]
        .as_array()
        .expect("models")
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect();
    assert_eq!(ids, ["gpt-a", "gpt-new", "gpt-used"]);
    assert_eq!(
        route["models"][0]["reasoningEfforts"],
        json!({ "low": "low", "high": "high" })
    );
    assert_eq!(route["account"], generation);
    assert!(
        runtime
            .snapshot()
            .model(CLIPROXYAPI_ID, "gpt-new")
            .is_some()
    );
    assert_eq!(bearers(&server.seen), [KEY_A]);
    let before = std::fs::read(home.join("settings.json")).expect("settings");
    let controller = bake_ai::AbortController::new();
    controller.abort();
    assert!(
        refresh_account(account, runtime, Vec::new(), Some(controller.signal()))
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(home.join("settings.json")).expect("settings"),
        before
    );
    server.stop().await;
}

// A refresh in a session whose runtime still holds an older
// sign-in reads the route and the key together from the files, so another
// process's newer sign-in is refreshed with its own key at its own proxy,
// and the old proxy sees nothing; the runtime then holds the new pair.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_with_a_stale_runtime_uses_the_files_pair() {
    let home = TempDir::new("account-refresh-stale");
    let a = proxy(json!({ "data": [{ "id": "gpt-a" }] })).await;
    let b = proxy(json!({ "data": [{ "id": "gpt-a" }, { "id": "gpt-b" }] })).await;
    let (url_a, url_b) = (a.url.clone(), b.url.clone());
    login(&home, &url_a, KEY_A);
    let runtime = ModelRuntime::new(startup_registry(home.path()));
    login(&home, &url_b, KEY_B);
    let refreshed = refresh_account(
        CliProxyAccount::new(home.path()),
        runtime.clone(),
        Vec::new(),
        None,
    )
    .await
    .expect("refresh");
    assert_eq!(refreshed.outcome, RefreshOutcome::Updated(2));
    assert!(bearers(&a.seen).is_empty());
    assert_eq!(bearers(&b.seen), [KEY_B]);
    assert_eq!(
        pair(&runtime.snapshot()),
        Some((format!("{url_b}/v1"), Some(KEY_B.into())))
    );
    a.stop().await;
    b.stop().await;
}

// A refresh writes only while the route is the one it fetched
// for; a sign-in that lands meanwhile is kept.
#[test]
fn a_refresh_commit_keeps_a_newer_route() {
    let home = TempDir::new("account-refresh-race");
    login(&home, URL_A, KEY_A);
    let fetched_for = read_json(&home.join("settings.json"))["cliproxyapi"].clone();
    login(&home, URL_B, KEY_B);
    let mut warnings = Vec::new();
    let wrote = CliProxyAccount::new(home.path())
        .commit_refresh(&fetched_for, json!([{ "id": "gpt-z" }]), &mut warnings)
        .expect("commit");
    assert!(!wrote);
    assert_eq!(startup_pair(home.path()), pair_b());
    // An unsynced refresh write is committed and reported.
    let fetched_for = read_json(&home.join("settings.json"))["cliproxyapi"].clone();
    let wrote = CliProxyAccount::new(home.path())
        .with_inject(Inject::Unsynced)
        .commit_refresh(&fetched_for, json!([{ "id": "gpt-z" }]), &mut warnings)
        .expect("commit");
    assert!(wrote);
    assert_eq!(warnings.len(), 1);
}

const CHILD_HOME: &str = "BAKE_ACCOUNT_TEST_HOME";
const CHILD_INDEX: &str = "BAKE_ACCOUNT_TEST_INDEX";
const ROUNDS: usize = 6;

/// A child of the cross-process test; does nothing in an ordinary run.
#[test]
#[ignore = "run by the cross-process test in a child process"]
fn account_child_process() {
    let (Ok(home), Ok(index)) = (std::env::var(CHILD_HOME), std::env::var(CHILD_INDEX)) else {
        return;
    };
    let home = Path::new(&home);
    let account = CliProxyAccount::new(home);
    let index: u16 = index.parse().expect("index");
    for round in 0..ROUNDS {
        if round % 3 == 2 {
            account.logout().expect("logout");
        } else {
            account
                .commit_login(&checked(
                    &format!("http://127.0.0.1:{}", 10_000 + index),
                    &format!("sk-fake-child-{index}"),
                    &["gpt-a"],
                ))
                .expect("login");
        }
        // Another writer of another provider, without the account lock.
        AuthStorage::load(&auth_path(home))
            .set_api_key(&format!("other-{index}-{round}"), "sk-fake-other")
            .expect("other write");
        // A reader without the account lock: a pair or nothing.
        if let AccountView::Bound(bound) = read_account(home) {
            let port = bound.route["baseURL"]
                .as_str()
                .and_then(|url| url.strip_prefix("http://127.0.0.1:"))
                .and_then(|rest| rest.split('/').next())
                .and_then(|port| port.parse::<u16>().ok())
                .expect("port");
            assert_eq!(bound.key(), format!("sk-fake-child-{}", port - 10_000));
        }
    }
}

// Sign-ins, sign-outs, and lock-free reads of several processes,
// interleaved with their writes to other providers, find a route with its
// own key or none, and lose no other write.
#[test]
fn independent_processes_keep_each_route_with_its_own_key() {
    const CHILDREN: u16 = 4;
    let home = TempDir::new("account-processes");
    struct Children(Vec<std::process::Child>);
    impl Drop for Children {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
            }
            for child in &mut self.0 {
                let _ = child.wait();
            }
        }
    }
    // Keep each child's libtest failure output, and settle every sibling
    // before asserting so a failing child cannot leave writers behind.
    let mut children = Children(Vec::new());
    for index in 0..CHILDREN {
        let output =
            std::fs::File::create(home.join(&format!("child-{index}.log"))).expect("child output");
        children.0.push(
            Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "cliproxyapi::account::tests::account_child_process",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD_HOME, home.path())
                .env(CHILD_INDEX, index.to_string())
                .stdin(Stdio::null())
                .stdout(output.try_clone().expect("clone child output"))
                .stderr(output)
                .spawn()
                .expect("spawn"),
        );
    }
    let started = std::time::Instant::now();
    let mut outcomes = vec![None; usize::from(CHILDREN)];
    loop {
        for (child, outcome) in children.0.iter_mut().zip(&mut outcomes) {
            if outcome.is_none() {
                *outcome = child.try_wait().expect("wait");
            }
        }
        if outcomes.iter().all(Option::is_some)
            || started.elapsed() >= std::time::Duration::from_secs(60)
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    drop(children);
    let failures: Vec<_> = outcomes
        .iter()
        .enumerate()
        .filter(|(_, outcome)| !outcome.is_some_and(|status| status.success()))
        .map(|(index, outcome)| {
            let output = std::fs::read_to_string(home.join(&format!("child-{index}.log")))
                .expect("read child output");
            format!("child {index}: {outcome:?} (None means the 60 s bound expired)\n{output}")
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    match startup_pair(home.path()) {
        None => {}
        Some((url, key)) => {
            let port = url
                .trim_start_matches("http://127.0.0.1:")
                .trim_end_matches("/v1")
                .parse::<u16>()
                .expect("port");
            assert_eq!(key, Some(format!("sk-fake-child-{}", port - 10_000)));
        }
    }
    let auth = read_json(&home.join("auth.json"));
    for index in 0..CHILDREN {
        for round in 0..ROUNDS {
            assert!(auth.get(format!("other-{index}-{round}")).is_some());
        }
    }
}

// A bound key is usable only through a verified account read. With
// settings.json's member gone (deleted, or lost to a power loss) there is
// no route, and a settings.json that cannot be read turns CLIProxyAPI off.
#[test]
fn a_bound_key_never_reaches_an_unbound_route() {
    {
        let home = TempDir::new("account-bound");
        login(&home, URL_A, KEY_A);
        // The member is absent.
        std::fs::write(home.join("settings.json"), "{}").expect("settings");
        assert_eq!(startup_pair(home.path()), None);
        // An older settings.json with an unbound route of its own.
        std::fs::write(
            home.join("settings.json"),
            json!({ "cliproxyapi": { "baseURL": URL_B, "api": "openai-responses", "models": [{ "id": "gpt-a" }] } })
                .to_string(),
        )
        .expect("settings");
        assert_eq!(startup_pair(home.path()), None);
        // Malformed.
        std::fs::write(home.join("settings.json"), "{ not json").expect("settings");
        assert_eq!(startup_pair(home.path()), None);
        assert!(!startup_warnings(&home).is_empty());
        // No generic reader sees the bound key.
        let auth = AuthStorage::load(&auth_path(home.path()));
        assert!(auth.read(CLIPROXYAPI_ID).is_none());
        let registry = startup_registry(home.path());
        assert!(
            registry
                .provider_api_key(CLIPROXYAPI_ID)
                .expect("key")
                .is_none()
        );
    }
}

// A request held at one proxy, a sign-in to
// another proxy with the same model id published to the running session,
// then a request made with the caller's stale model. Each request sends
// its own route's key to its own proxy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_request_and_the_next_each_keep_their_own_pair() {
    use bake_ai::{SimpleStreamOptions, TranscriptContext};
    let home = TempDir::new("account-two-proxies");
    let started_a = Arc::new(tokio::sync::Notify::new());
    let release_a = Arc::new(tokio::sync::Notify::new());
    let error = json!({ "error": { "message": "test" } });
    let a = TestProxy::start(
        500,
        error.clone(),
        Some((started_a.clone(), release_a.clone())),
    )
    .await;
    let b = TestProxy::start(500, error, None).await;
    let (url_a, url_b) = (a.url.clone(), b.url.clone());
    login(&home, &url_a, KEY_A);
    let runtime = ModelRuntime::new(startup_registry(home.path()));
    let apis = Arc::new(bake_ai::ApiRegistry::with_builtins());
    let stream_fn = runtime.stream_fn_owned(apis, None);
    let stale = runtime
        .snapshot()
        .model(CLIPROXYAPI_ID, "gpt-a")
        .expect("model");
    // No retries: each proxy answers one request, and the first proxy is
    // released once.
    let once = || SimpleStreamOptions {
        base: bake_ai::StreamOptions {
            max_retries: Some(0),
            ..Default::default()
        },
        ..Default::default()
    };
    let first = stream_fn(&stale, &TranscriptContext::default(), once()).expect("stream");
    started_a.notified().await;
    login(&home, &url_b, KEY_B);
    let account = CliProxyAccount::new(home.path());
    runtime
        .try_update(|registry| account.apply_to(registry))
        .expect("publish");
    let second = stream_fn(&stale, &TranscriptContext::default(), once()).expect("stream");
    let _ = second.result().await;
    release_a.notify_one();
    let _ = first.result().await;
    assert_eq!(bearers(&a.seen), [KEY_A]);
    assert_eq!(bearers(&b.seen), [KEY_B]);
    a.stop().await;
    b.stop().await;
}

// A restoration that reached auth.json without a directory flush
// is reported as such in the partial outcome.
#[test]
fn an_unsynced_restoration_is_reported() {
    let home = TempDir::new("account-restore-unsynced");
    login(&home, URL_A, KEY_A);
    let error = CliProxyAccount::new(home.path())
        .with_inject(Inject::FailRouteThenUnsyncedRestore)
        .commit_login(&checked(URL_B, KEY_B, &["gpt-b"]))
        .expect_err("refused");
    let AccountError::Partial { outcome, .. } = &error else {
        panic!("{error:?}");
    };
    assert!(outcome.contains("previous sign-in is back"), "{outcome}");
    assert!(outcome.contains("may not be durable"), "{outcome}");
    assert_eq!(startup_pair(home.path()), pair_a());
}

// Pi's `--api-key` precedence: the runtime key is the person's explicit
// choice for this process, so requests and refreshes both send it, to
// whichever route is current, before and after another sign-in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_and_refreshes_send_the_same_runtime_key() {
    use bake_ai::{SimpleStreamOptions, TranscriptContext};
    const RUNTIME: &str = "sk-fake-runtime";
    let home = TempDir::new("account-runtime-key");
    let a = proxy(json!({ "data": [{ "id": "gpt-a" }, { "id": "gpt-x" }] })).await;
    let b = proxy(json!({ "data": [{ "id": "gpt-a" }, { "id": "gpt-y" }] })).await;
    let (url_a, url_b) = (a.url.clone(), b.url.clone());
    login(&home, &url_a, KEY_A);
    let mut registry = startup_registry(home.path());
    registry.set_runtime_api_key(CLIPROXYAPI_ID, RUNTIME);
    let runtime = ModelRuntime::new(registry);
    let apis = Arc::new(bake_ai::ApiRegistry::with_builtins());
    let stream_fn = runtime.stream_fn_owned(apis, None);
    let account = CliProxyAccount::new(home.path());
    for url in [&url_a, &url_b] {
        if url == &url_b {
            login(&home, &url_b, KEY_B);
            runtime
                .try_update(|registry| account.apply_to(registry))
                .expect("publish");
        }
        let model = runtime
            .snapshot()
            .model(CLIPROXYAPI_ID, "gpt-a")
            .expect("model");
        let options = SimpleStreamOptions {
            base: bake_ai::StreamOptions {
                max_retries: Some(0),
                ..Default::default()
            },
            ..Default::default()
        };
        let _ = stream_fn(&model, &TranscriptContext::default(), options)
            .expect("stream")
            .result()
            .await;
        let refreshed = refresh_account(account.clone(), runtime.clone(), Vec::new(), None)
            .await
            .expect("refresh");
        assert_eq!(refreshed.outcome, RefreshOutcome::Updated(2));
    }
    assert_eq!(bearers(&a.seen), [RUNTIME, RUNTIME]);
    assert_eq!(bearers(&b.seen), [RUNTIME, RUNTIME]);
    a.stop().await;
    b.stop().await;
}

// The account's diagnostics show neither the key nor
// a saved route's header values.
#[test]
fn diagnostics_show_no_key_or_header_value() {
    const HEADER: &str = "sentinel-header-value-0123";
    let home = TempDir::new("account-redaction");
    login(&home, URL_A, KEY_A);
    let mut settings = read_json(&home.join("settings.json"));
    settings["cliproxyapi"]["headers"] = json!({ "x-test": HEADER });
    std::fs::write(home.join("settings.json"), settings.to_string()).expect("settings");
    let view = read_account(home.path());
    assert!(matches!(view, AccountView::Bound(_)), "{view:?}");
    let shown = format!(
        "{view:?} {:?} {:?}",
        crate::settings::Settings::load(home.path()),
        startup_registry(home.path()),
    );
    assert!(!shown.contains(KEY_A), "{shown}");
    assert!(!shown.contains(HEADER), "{shown}");
    assert!(shown.contains("headers"), "{shown}");
}

// A refresh brings the session's runtime to the account the
// files hold even when the catalog needs no write. Another process signed
// in to a second proxy listing the same models; the refresh fetches from
// that proxy with its key, and the next request goes there too. A
// disabled or signed-out account is reported and taken off the runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_with_an_unchanged_catalog_still_syncs_the_runtime() {
    let home = TempDir::new("account-refresh-sync");
    let catalog = json!({ "data": [{ "id": "gpt-a" }] });
    let a = proxy(catalog.clone()).await;
    let b = proxy(catalog).await;
    login(&home, &a.url, KEY_A);
    let runtime = ModelRuntime::new(startup_registry(home.path()));
    let held = runtime.snapshot();
    login(&home, &b.url, KEY_B);
    let account = CliProxyAccount::new(home.path());
    let refreshed = refresh_account(account.clone(), runtime.clone(), Vec::new(), None)
        .await
        .expect("refresh");
    assert_eq!(refreshed.outcome, RefreshOutcome::Unchanged);
    assert!(bearers(&a.seen).is_empty());
    assert_eq!(bearers(&b.seen), [KEY_B]);
    assert_eq!(
        pair(&runtime.snapshot()),
        Some((format!("{}/v1", b.url), Some(KEY_B.into())))
    );
    // A snapshot taken before keeps its own pair.
    assert_eq!(
        pair(&held),
        Some((format!("{}/v1", a.url), Some(KEY_A.into())))
    );
    // Disabled by a direct key write: reported, and off in the runtime.
    AuthStorage::load(&auth_path(home.path()))
        .set_api_key(CLIPROXYAPI_ID, "sk-fake-direct")
        .expect("direct");
    let refreshed = refresh_account(account.clone(), runtime.clone(), Vec::new(), None)
        .await
        .expect("refresh");
    assert!(
        matches!(refreshed.outcome, RefreshOutcome::Disabled(_)),
        "{refreshed:?}"
    );
    assert_eq!(pair(&runtime.snapshot()), None);
    // Signed out elsewhere.
    login(&home, &b.url, KEY_B);
    runtime
        .try_update(|registry| account.apply_to(registry))
        .expect("apply");
    account.logout().expect("logout");
    let refreshed = refresh_account(account, runtime.clone(), Vec::new(), None)
        .await
        .expect("refresh");
    assert_eq!(refreshed.outcome, RefreshOutcome::NotSignedIn);
    assert_eq!(pair(&runtime.snapshot()), None);
    a.stop().await;
    b.stop().await;
}

// A refresh keeps the models in use (the new-session default with the
// current model): the default differs from the current model and
// the proxy no longer lists it; the refresh keeps it with its efforts and
// limits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_keeps_the_default_model_the_proxy_dropped() {
    let home = TempDir::new("account-refresh-default");
    let server = proxy(json!({ "data": [{ "id": "gpt-a" }, { "id": "gpt-new" }] })).await;
    let mut default = model("gpt-default");
    default.context_window = Some(64_000);
    default.max_tokens = Some(8_000);
    default.reasoning_efforts = Some(vec!["low".into(), "max".into()]);
    let mut login = checked(&server.url, KEY_A, &["gpt-a"]);
    login.models.push(default);
    CliProxyAccount::new(home.path())
        .commit_login(&login)
        .expect("login");
    let keep = crate::cliproxyapi::cli_proxy_models_in_use(
        Some((CLIPROXYAPI_ID, "gpt-a")),
        Some((CLIPROXYAPI_ID, "gpt-default")),
        None,
    );
    let runtime = ModelRuntime::new(startup_registry(home.path()));
    let refreshed = refresh_account(
        CliProxyAccount::new(home.path()),
        runtime.clone(),
        keep,
        None,
    )
    .await
    .expect("refresh");
    assert_eq!(refreshed.outcome, RefreshOutcome::Updated(3));
    let route = &read_json(&home.join("settings.json"))["cliproxyapi"];
    let kept = route["models"]
        .as_array()
        .expect("models")
        .iter()
        .find(|model| model["id"] == "gpt-default")
        .expect("the default stays")
        .clone();
    assert_eq!(kept["contextWindow"], 64_000);
    assert_eq!(kept["maxTokens"], 8_000);
    assert_eq!(
        kept["reasoningEfforts"],
        json!({ "low": "low", "max": "max" })
    );
    assert!(
        runtime
            .snapshot()
            .model(CLIPROXYAPI_ID, "gpt-default")
            .is_some()
    );
    server.stop().await;
}

// A refresh whose route write landed but whose directory flush
// failed reports it apart from its other warnings, so a quiet background
// refresh can still say the write may not be durable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsynced_refresh_write_is_reported_apart() {
    let home = TempDir::new("account-refresh-unsynced");
    let a = proxy(json!({ "data": [{ "id": "gpt-a" }, { "id": "gpt-b" }] })).await;
    login(&home, &a.url, KEY_A);
    let runtime = ModelRuntime::new(startup_registry(home.path()));
    let account = CliProxyAccount::new(home.path()).with_inject(Inject::Unsynced);
    let refreshed = refresh_account(account, runtime, Vec::new(), None)
        .await
        .expect("refresh");
    assert_eq!(refreshed.outcome, RefreshOutcome::Updated(2));
    assert_eq!(refreshed.unsynced.len(), 1, "{refreshed:?}");
    assert!(refreshed.warnings.is_empty(), "{refreshed:?}");
    a.stop().await;
}
