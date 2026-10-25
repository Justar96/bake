//! The live commands against a real session, a temporary Bake home, and a
//! loopback CLIProxyAPI, with fake keys only. Each test owns its home, its
//! proxy (stopped and awaited before the test ends), and its port.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bake_tui_view::dialog::{DialogAnswer, DialogKind, DialogRequest, SecretText};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

const KEY: &str = "sk-fake-live-0123";

/// A Bake home this test alone owns, removed when dropped.
struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "bake-tui-home-{name}-{}-{}",
            std::process::id(),
            bake_ai::now_ms()
        ));
        std::fs::create_dir(&dir).expect("a fresh home");
        Self(dir)
    }

    fn files(&self) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<_> = std::fs::read_dir(&self.0)
            .expect("home")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_file())
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    std::fs::read(entry.path()).expect("read"),
                )
            })
            .filter(|(name, _)| !name.ends_with(".lock"))
            .collect();
        files.sort();
        files
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A loopback proxy answering every request with the model list `body`.
/// With `hold`, it reports each request from the `hold_from`th (counting
/// from 0) on `reached` and answers it only once `release` is notified;
/// a held request whose client hangs up is reported on `closed`.
/// [`Self::stop`] ends its task and waits.
struct Proxy {
    url: String,
    heads: Arc<Mutex<Vec<String>>>,
    closed: std_mpsc::Receiver<()>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

type Hold = (std_mpsc::Sender<()>, Arc<Notify>);

impl Proxy {
    fn start(
        runtime: &tokio::runtime::Runtime,
        body: serde_json::Value,
        hold: Option<Hold>,
    ) -> Self {
        Self::start_holding_from(runtime, body, hold, 0)
    }

    fn start_holding_from(
        runtime: &tokio::runtime::Runtime,
        body: serde_json::Value,
        hold: Option<Hold>,
        hold_from: usize,
    ) -> Self {
        let listener = runtime
            .block_on(TcpListener::bind("127.0.0.1:0"))
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&heads);
        let (shutdown, mut stopped) = tokio::sync::oneshot::channel::<()>();
        let (hung_up, closed) = std_mpsc::channel();
        let task = runtime.spawn(async move {
            loop {
                let mut socket = tokio::select! {
                    _ = &mut stopped => return,
                    accepted = listener.accept() => match accepted {
                        Ok((socket, _)) => socket,
                        Err(_) => return,
                    },
                };
                let head = tokio::select! {
                    _ = &mut stopped => return,
                    head = read_head(&mut socket) => head,
                };
                let index = {
                    let mut heads = record.lock().expect("heads");
                    heads.push(head);
                    heads.len() - 1
                };
                if let Some((reached, release)) = hold.as_ref().filter(|_| index >= hold_from) {
                    let _ = reached.send(());
                    let mut byte = [0u8; 1];
                    tokio::select! {
                        _ = &mut stopped => return,
                        () = release.notified() => {}
                        // A complete request was read, so the client's next
                        // read result is its hangup.
                        _ = socket.read(&mut byte) => {
                            let _ = hung_up.send(());
                            continue;
                        }
                    }
                }
                let bytes = body.to_string().into_bytes();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&bytes).await;
                let _ = socket.shutdown().await;
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}"),
            heads,
            closed,
            shutdown: Some(shutdown),
            task: Some(task),
        }
    }

    fn stop(mut self, runtime: &tokio::runtime::Runtime) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            runtime.block_on(task).expect("the proxy task");
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// One request's head, read until its blank line (a GET has no body).
async fn read_head(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 2048];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        match socket.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn models() -> serde_json::Value {
    json!({ "data": [{ "id": "gpt-live" }, { "id": "gpt-other" }] })
}

/// A started port over a session whose home is `home`.
fn home_harness(home: &Home, gate: Option<commands::CommitGate>) -> Harness {
    panicking_harness(home, gate, None).0
}

/// [`home_harness`] with `--offline`.
fn offline_harness(home: &Home) -> Harness {
    let (runtime, faux, mut live) = setup(Vec::new(), Vec::new(), Some("test-key"));
    live.home = Some(home.0.clone());
    live.offline = true;
    let (sender, updates) = std_mpsc::channel();
    let mut port = LivePort::start(live, move |update| sender.send(update).is_ok());
    port.begin();
    Harness {
        runtime,
        faux,
        port: Some(port),
        updates,
    }
}

/// [`home_harness`], whose driver panics when it refuses the prompt
/// `panic_on`, as a driver bug would, with whatever it owns still running.
/// The `Weak` is gone once nothing holds the port's update callback: the
/// driver and every task it started have ended.
fn panicking_harness(
    home: &Home,
    gate: Option<commands::CommitGate>,
    panic_on: Option<u64>,
) -> (Harness, std::sync::Weak<()>) {
    let held = Arc::new(());
    let callback_alive = Arc::downgrade(&held);
    let (runtime, faux, mut live) = setup(Vec::new(), Vec::new(), Some("test-key"));
    live.home = Some(home.0.clone());
    live.commit_gate = gate;
    let (sender, updates) = std_mpsc::channel();
    let mut port = LivePort::start(live, move |update| {
        let _held = &held;
        if let RuntimeUpdate::Refused { id, .. } = &update {
            assert_ne!(Some(*id), panic_on, "a driver bug");
        }
        sender.send(update).is_ok()
    });
    port.begin();
    (
        Harness {
            runtime,
            faux,
            port: Some(port),
            updates,
        },
        callback_alive,
    )
}

impl Harness {
    /// The next dialog the runtime opens.
    fn dialog(&self) -> DialogRequest {
        let seen = self.until(|update| matches!(update, RuntimeUpdate::Dialog(Some(_))));
        match seen.last() {
            Some(RuntimeUpdate::Dialog(Some(request))) => request.clone(),
            _ => unreachable!(),
        }
    }

    fn reply(&self, id: u64, answer: DialogAnswer) {
        self.port().request(Request::Dialog { id, answer });
    }

    /// Updates until the command named by prompt `id` ends.
    fn command_end(&self, id: u64) -> Vec<RuntimeUpdate> {
        self.until(|update| {
            matches!(update, RuntimeUpdate::Admitted(admitted) if *admitted == id)
                || matches!(update, RuntimeUpdate::Refused { id: refused, .. } if *refused == id)
        })
    }
}

fn entered(text: &str) -> DialogAnswer {
    DialogAnswer::Entered(SecretText::from(text.to_owned()))
}

fn refusal(updates: &[RuntimeUpdate]) -> Option<String> {
    updates.iter().find_map(|update| match update {
        RuntimeUpdate::Refused { reason, .. } => Some(reason.clone()),
        _ => None,
    })
}

fn no_key_in(updates: &[RuntimeUpdate]) {
    let shown = format!("{updates:?}");
    assert!(!shown.contains(KEY), "{shown}");
}

fn auth_key(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join("auth.json")).ok()?;
    let auth: serde_json::Value = serde_json::from_str(&text).ok()?;
    auth["cliproxyapi"]["key"].as_str().map(str::to_owned)
}

// Pi `handleModelCommand` (regression 7443): an exact model is set at
// once; otherwise the list opens, a stale answer is dropped, and the
// chosen model is in effect and shown in the status.
#[test]
fn model_sets_an_exact_match_or_opens_the_list() {
    let h = harness(Vec::new(), Vec::new());
    h.submit_id(1, Submission::FollowUp("/model faux/faux-2".into()));
    let updates = h.command_end(1);
    assert!(matches!(updates.last(), Some(RuntimeUpdate::Admitted(1))));
    let updates = h.until(|u| matches!(u, RuntimeUpdate::Status(_)));
    assert!(committed(&updates).contains(&Row::Info("Model: faux/faux-2".into())));
    assert!(matches!(
        updates.last(),
        Some(RuntimeUpdate::Status(SessionStatus { model: Some(m), .. })) if m == "faux/faux-2"
    ));
    h.submit_id(2, Submission::FollowUp("/model".into()));
    let dialog = h.dialog();
    let DialogKind::List(list) = &dialog.kind else {
        panic!("{dialog:?}");
    };
    assert_eq!(list.current, Some(1));
    // An answer for another dialog is dropped.
    h.reply(
        dialog.id + 100,
        DialogAnswer::Chose {
            value: "faux/faux-2".into(),
            save_default: false,
        },
    );
    h.reply(
        dialog.id,
        DialogAnswer::Chose {
            value: "faux/faux-1".into(),
            save_default: false,
        },
    );
    let updates = h.command_end(2);
    assert!(matches!(updates.last(), Some(RuntimeUpdate::Admitted(2))));
    let updates = h.until(|u| matches!(u, RuntimeUpdate::Status(_)));
    assert!(committed(&updates).contains(&Row::Info("Model: faux/faux-1".into())));
    // A cancelled list puts the draft back.
    h.submit_id(3, Submission::FollowUp("/thinking".into()));
    let dialog = h.dialog();
    h.reply(dialog.id, DialogAnswer::Cancel);
    let updates = h.command_end(3);
    assert_eq!(refusal(&updates).as_deref(), Some(commands::CANCELLED));
    h.stop();
}

// Pi `cycleThinkingLevel` (Shift+Tab): each press moves to the model's
// next level, from the highest back to the lowest, for this session only,
// and the status line follows; a model with one level stays where it is.
#[test]
fn shift_tab_cycles_the_thinking_level_and_the_status_follows() {
    let h = harness(Vec::new(), Vec::new());
    h.until(|u| matches!(u, RuntimeUpdate::Status(_)));
    let session = h.port().test_session.upgrade().expect("live session");
    // faux-1 does not reason: it has one level, which a press keeps.
    let before = session.thinking_level();
    h.port().request(Request::CycleThinking);
    h.until(|u| matches!(u, RuntimeUpdate::Status(_)));
    assert_eq!(session.thinking_level(), before);
    h.submit_id(1, Submission::FollowUp("/model faux/faux-2".into()));
    h.command_end(1);
    h.until(|u| matches!(u, RuntimeUpdate::Status(_)));
    let levels = session.available_thinking_levels();
    assert!(levels.len() > 2, "{levels:?}");
    let start = levels
        .iter()
        .position(|level| *level == session.thinking_level())
        .expect("a supported level");
    for step in 1..=levels.len() {
        h.port().request(Request::CycleThinking);
        let updates = h.until(|u| matches!(u, RuntimeUpdate::Status(_)));
        let expected = levels[(start + step) % levels.len()];
        assert_eq!(session.thinking_level(), expected, "step {step}");
        assert!(
            matches!(
                updates.last(),
                Some(RuntimeUpdate::Status(SessionStatus { thinking, .. }))
                    if thinking.as_deref().is_none_or(|shown| shown == expected.as_str())
            ),
            "{updates:?}"
        );
    }
    // Around once: back where it started, and nothing is a transcript row.
    assert_eq!(session.thinking_level(), levels[start]);
    h.stop();
}

// Bake: `/logout` names CLIProxyAPI alone; another provider is refused
// before any account work, so nothing in the home changes.
#[test]
fn logout_of_another_provider_writes_nothing() {
    let home = Home::new("logout-other");
    std::fs::write(
        home.0.join("auth.json"),
        r#"{"cliproxyapi":{"type":"api_key","key":"sk-fake-kept","account":"g"}}"#,
    )
    .expect("auth");
    std::fs::write(
        home.0.join("settings.json"),
        r#"{"cliproxyapi":{"account":"g"}}"#,
    )
    .expect("settings");
    let before = home.files();
    let h = home_harness(&home, None);
    h.submit_id(1, Submission::FollowUp("/logout openai".into()));
    let updates = h.command_end(1);
    assert!(
        refusal(&updates).is_some_and(|r| r
            == "/logout openai is not available in bake yet; providers other than CLIProxyAPI are planned for a later release, and your draft is kept."),
        "{updates:?}"
    );
    h.stop();
    assert_eq!(home.files(), before);
}

// cliproxyapi.test.ts sign-in, through the live dialogs: the address,
// then the key, checked against the proxy; the key goes to auth.json, the
// route to settings.json, and no update carries the key.
#[test]
fn login_saves_the_account_and_no_update_carries_the_key() {
    let home = Home::new("login");
    let h = home_harness(&home, None);
    let proxy = Proxy::start(&h.runtime, models(), None);
    h.submit_id(1, Submission::FollowUp("/login".into()));
    let url = h.dialog();
    let DialogKind::Field(field) = &url.kind else {
        panic!("{url:?}");
    };
    // The address field starts empty, the address an empty answer uses
    // shown as its placeholder, so typing another needs nothing deleted.
    assert!(!field.secret && field.initial.is_empty(), "{field:?}");
    assert!(
        field
            .placeholder
            .as_deref()
            .is_some_and(|placeholder| placeholder.contains("Enter to use")),
        "{field:?}"
    );
    h.reply(url.id, entered(&proxy.url));
    let key = h.dialog();
    let DialogKind::Field(field) = &key.kind else {
        panic!("{key:?}");
    };
    assert!(field.secret && field.initial.is_empty());
    h.reply(key.id, entered(KEY));
    let updates = h.command_end(1);
    assert!(
        matches!(updates.last(), Some(RuntimeUpdate::Admitted(1))),
        "{updates:?}"
    );
    let after = h.until(|u| matches!(u, RuntimeUpdate::Status(_)));
    let note = committed(&after);
    assert!(
        matches!(note.first(), Some(Row::Info(text)) if text.contains("2 models ready")),
        "{note:?}"
    );
    no_key_in(&updates);
    no_key_in(&after);
    assert_eq!(auth_key(&home.0).as_deref(), Some(KEY));
    let settings = std::fs::read_to_string(home.0.join("settings.json")).expect("settings");
    assert!(!settings.contains(KEY));
    assert!(
        proxy.heads.lock().expect("heads")[0]
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {KEY}"))
    );
    // The session's runtime has the route: /model lists its models.
    h.submit_id(
        2,
        Submission::FollowUp("/model cliproxyapi/gpt-live".into()),
    );
    let updates = h.command_end(2);
    assert!(
        matches!(updates.last(), Some(RuntimeUpdate::Admitted(2))),
        "{updates:?}"
    );
    // Sign out: the model is no longer usable.
    h.submit_id(3, Submission::FollowUp("/logout".into()));
    let updates = h.command_end(3);
    assert!(
        matches!(updates.last(), Some(RuntimeUpdate::Admitted(3))),
        "{updates:?}"
    );
    assert_eq!(auth_key(&home.0), None);
    h.submit_id(4, Submission::FollowUp("hello".into()));
    let updates = h.until(|u| matches!(u, RuntimeUpdate::Refused { id: 4, .. }));
    assert!(refusal(&updates).is_some());
    proxy.stop(&h.runtime);
    h.stop();
}

// Bake: Esc at the address, at the key, and during the held check of the
// proxy ends the sign-in with nothing written and the draft back; the held
// fetch is aborted, not waited out.
#[test]
fn login_cancelled_at_each_step_writes_nothing() {
    let home = Home::new("login-cancel");
    let h = home_harness(&home, None);
    let (reached_tx, reached) = std_mpsc::channel();
    let release = Arc::new(Notify::new());
    let proxy = Proxy::start(
        &h.runtime,
        models(),
        Some((reached_tx, Arc::clone(&release))),
    );
    // At the address.
    h.submit_id(1, Submission::FollowUp("/login".into()));
    let url = h.dialog();
    h.reply(url.id, DialogAnswer::Cancel);
    assert_eq!(
        refusal(&h.command_end(1)).as_deref(),
        Some(commands::CANCELLED)
    );
    // At the key.
    h.submit_id(2, Submission::FollowUp("/login cliproxyapi".into()));
    let url = h.dialog();
    h.reply(url.id, entered(&proxy.url));
    let key = h.dialog();
    h.reply(key.id, DialogAnswer::Cancel);
    assert_eq!(
        refusal(&h.command_end(2)).as_deref(),
        Some(commands::CANCELLED)
    );
    // During the check, held at the proxy: Esc on the shown field aborts.
    h.submit_id(3, Submission::FollowUp("/login".into()));
    let url = h.dialog();
    h.reply(url.id, entered(&proxy.url));
    let key = h.dialog();
    h.reply(key.id, entered(KEY));
    reached
        .recv_timeout(PATIENCE)
        .expect("the check reached the proxy");
    h.reply(key.id, DialogAnswer::Cancel);
    let updates = h.command_end(3);
    assert_eq!(refusal(&updates).as_deref(), Some(commands::CANCELLED));
    no_key_in(&updates);
    assert_eq!(auth_key(&home.0), None);
    assert!(!home.0.join("settings.json").exists());
    proxy.stop(&h.runtime);
    h.stop();
}

// Bake: quitting while the check is held at the proxy aborts it; the port
// stops, and nothing is written.
#[test]
fn quitting_during_a_held_check_writes_nothing() {
    let home = Home::new("login-quit");
    let h = home_harness(&home, None);
    let (reached_tx, reached) = std_mpsc::channel();
    let release = Arc::new(Notify::new());
    let proxy = Proxy::start(
        &h.runtime,
        models(),
        Some((reached_tx, Arc::clone(&release))),
    );
    h.submit_id(1, Submission::FollowUp("/login".into()));
    let url = h.dialog();
    h.reply(url.id, entered(&proxy.url));
    let key = h.dialog();
    h.reply(key.id, entered(KEY));
    reached
        .recv_timeout(PATIENCE)
        .expect("the check reached the proxy");
    let Harness {
        runtime,
        port,
        updates,
        ..
    } = h;
    port.expect("port").stop().expect("stop");
    let late: Vec<RuntimeUpdate> = updates.try_iter().collect();
    no_key_in(&late);
    assert_eq!(auth_key(&home.0), None);
    assert!(!home.0.join("settings.json").exists());
    proxy.stop(&runtime);
}

// Bake: once the check has passed, the sign-in is committed even when Esc
// or a quit comes while it is held at the commit; the command reports what
// it saved.
#[test]
fn a_cancel_at_the_commit_boundary_does_not_abandon_the_commit() {
    let home = Home::new("login-commit");
    let (reached_tx, reached) = std_mpsc::channel();
    let release = Arc::new(Notify::new());
    let gate = commands::CommitGate {
        reached: reached_tx,
        release: commands::Release::Notified(Arc::clone(&release)),
    };
    let h = home_harness(&home, Some(gate));
    let proxy = Proxy::start(&h.runtime, models(), None);
    h.submit_id(1, Submission::FollowUp("/login".into()));
    let url = h.dialog();
    h.reply(url.id, entered(&proxy.url));
    let key = h.dialog();
    h.reply(key.id, entered(KEY));
    reached.recv_timeout(PATIENCE).expect("held at the commit");
    h.reply(key.id, DialogAnswer::Cancel);
    release.notify_one();
    let updates = h.command_end(1);
    assert!(
        matches!(updates.last(), Some(RuntimeUpdate::Admitted(1))),
        "{updates:?}"
    );
    assert_eq!(auth_key(&home.0).as_deref(), Some(KEY));
    proxy.stop(&h.runtime);
    h.stop();
}

// Bake: a late Esc for the previous command's last dialog never aborts the
// next command, here a refresh held at the proxy before it opens any
// dialog of its own.
#[test]
fn a_stale_cancel_does_not_abort_the_next_command() {
    let home = Home::new("stale-cancel");
    let h = home_harness(&home, None);
    let (reached_tx, reached) = std_mpsc::channel();
    let release = Arc::new(Notify::new());
    let proxy = Proxy::start_holding_from(
        &h.runtime,
        models(),
        Some((reached_tx, Arc::clone(&release))),
        1,
    );
    h.submit_id(1, Submission::FollowUp("/login".into()));
    let url = h.dialog();
    h.reply(url.id, entered(&proxy.url));
    let key = h.dialog();
    h.reply(key.id, entered(KEY));
    assert!(matches!(
        h.command_end(1).last(),
        Some(RuntimeUpdate::Admitted(1))
    ));
    h.submit_id(2, Submission::FollowUp("/thinking".into()));
    let last = h.dialog();
    h.reply(last.id, DialogAnswer::Cancel);
    assert_eq!(
        refusal(&h.command_end(2)).as_deref(),
        Some(commands::CANCELLED)
    );
    h.submit_id(3, Submission::FollowUp("/refresh-models".into()));
    reached
        .recv_timeout(PATIENCE)
        .expect("the refresh reached the proxy");
    // The stale Esc arrives while the refresh, which shows no dialog, is
    // held. A second request on the same path is handled after it.
    h.reply(last.id, DialogAnswer::Cancel);
    h.submit_id(4, Submission::FollowUp("/model".into()));
    let refused = h.until(|u| matches!(u, RuntimeUpdate::Refused { id: 4, .. }));
    assert!(refusal(&refused).is_some_and(|r| r.contains("Another command")));
    release.notify_one();
    let updates = h.command_end(3);
    assert!(
        matches!(updates.last(), Some(RuntimeUpdate::Admitted(3))),
        "{updates:?}"
    );
    proxy.stop(&h.runtime);
    h.stop();
}

/// Stops `h`'s port, whose driver panicked, and checks the panic is
/// reported.
fn stop_after_panic(mut h: Harness) -> tokio::runtime::Runtime {
    let error = h
        .port
        .take()
        .expect("port")
        .stop()
        .expect_err("the panic is reported");
    assert!(error.to_string().contains("driver failed"), "{error}");
    h.runtime
}

// Bake: a driver that panics while a sign-in is held at its commit hands
// the command to the port, which aborts it as a shutdown does and awaits
// it, so the commit, which no abort stops, has finished when `stop`
// returns. The gate opens only on that abort.
#[test]
fn a_driver_panic_during_a_held_login_commit_still_finishes_it() {
    let home = Home::new("panic-commit");
    let (reached_tx, reached) = std_mpsc::channel();
    let gate = commands::CommitGate {
        reached: reached_tx,
        release: commands::Release::Aborted,
    };
    let (h, callback_alive) = panicking_harness(&home, Some(gate), Some(99));
    let proxy = Proxy::start(&h.runtime, models(), None);
    h.submit_id(1, Submission::FollowUp("/login".into()));
    let url = h.dialog();
    h.reply(url.id, entered(&proxy.url));
    let key = h.dialog();
    h.reply(key.id, entered(KEY));
    reached.recv_timeout(PATIENCE).expect("held at the commit");
    assert_eq!(auth_key(&home.0), None);
    h.submit_id(99, Submission::FollowUp("/copy".into()));
    let runtime = stop_after_panic(h);
    assert_eq!(auth_key(&home.0).as_deref(), Some(KEY));
    assert!(
        callback_alive.upgrade().is_none(),
        "a task outlived the port"
    );
    proxy.stop(&runtime);
}

// Pi's `offlineMode`: `--offline` runs no startup network operation. The
// home is signed in to a loopback proxy that reports every request it
// receives, so the refresh an ordinary launch makes is the test's control:
// the offline launch reaches the proxy not once.
#[test]
fn offline_starts_the_session_without_the_catalog_refresh() {
    let home = Home::new("offline");
    let (reached_tx, reached) = std_mpsc::channel();
    let release = Arc::new(Notify::new());
    let signed_in = home_harness(&home, None);
    // The sign-in's own check is request 0; the refreshes are held from 1.
    let proxy = Proxy::start_holding_from(
        &signed_in.runtime,
        models(),
        Some((reached_tx, Arc::clone(&release))),
        1,
    );
    signed_in.submit_id(1, Submission::FollowUp("/login".into()));
    let url = signed_in.dialog();
    signed_in.reply(url.id, entered(&proxy.url));
    let key = signed_in.dialog();
    signed_in.reply(key.id, entered(KEY));
    assert!(matches!(
        signed_in.command_end(1).last(),
        Some(RuntimeUpdate::Admitted(1))
    ));
    // The proxy's task runs on the first runtime, which stays up.
    let Harness {
        runtime: proxy_runtime,
        port,
        ..
    } = signed_in;
    port.expect("port").stop().expect("stop");

    let offline = offline_harness(&home);
    offline.submit_id(2, Submission::FollowUp("hello".into()));
    offline.until_turn_end();
    offline.stop();
    assert_eq!(
        reached.try_recv(),
        Err(std_mpsc::TryRecvError::Empty),
        "an offline launch reached the proxy"
    );

    // The control: the same home without `--offline` does refresh.
    let online = home_harness(&home, None);
    reached
        .recv_timeout(PATIENCE)
        .expect("the startup refresh reached the proxy");
    release.notify_waiters();
    online.stop();
    proxy.stop(&proxy_runtime);
}

// Bake: a driver that panics while the startup refresh's fetch is held
// hands the refresh to the port, which aborts it and awaits it: when
// `stop` returns, with the runtime still up, the refresh has ended (so
// nothing holds the update callback) and its fetch hangs up.
#[test]
fn a_driver_panic_during_a_held_startup_refresh_aborts_its_fetch() {
    let home = Home::new("panic-startup");
    let (reached_tx, reached) = std_mpsc::channel();
    let release = Arc::new(Notify::new());
    let signed_in = home_harness(&home, None);
    let proxy = Proxy::start_holding_from(
        &signed_in.runtime,
        models(),
        Some((reached_tx, Arc::clone(&release))),
        1,
    );
    signed_in.submit_id(1, Submission::FollowUp("/login".into()));
    let url = signed_in.dialog();
    signed_in.reply(url.id, entered(&proxy.url));
    let key = signed_in.dialog();
    signed_in.reply(key.id, entered(KEY));
    assert!(matches!(
        signed_in.command_end(1).last(),
        Some(RuntimeUpdate::Admitted(1))
    ));
    // The proxy's task runs on the first runtime, which stays up.
    let Harness {
        runtime: proxy_runtime,
        port,
        ..
    } = signed_in;
    port.expect("port").stop().expect("stop");
    let (h, callback_alive) = panicking_harness(&home, None, Some(99));
    reached
        .recv_timeout(PATIENCE)
        .expect("the startup refresh reached the proxy");
    h.submit_id(99, Submission::FollowUp("/copy".into()));
    let runtime = stop_after_panic(h);
    assert!(
        callback_alive.upgrade().is_none(),
        "the refresh outlived the port"
    );
    proxy
        .closed
        .recv_timeout(PATIENCE)
        .expect("the held fetch hung up");
    drop(runtime);
    proxy.stop(&proxy_runtime);
}
