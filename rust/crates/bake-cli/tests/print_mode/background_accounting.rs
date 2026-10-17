//! The real binary keeps a dispatched summary's terminal event through exit.

use super::*;
use std::net::Shutdown;
use std::sync::Condvar;

struct HeldSummaryServer {
    port: u16,
    stop: Arc<AtomicBool>,
    ready: Arc<(Mutex<bool>, Condvar)>,
    sockets: Arc<Mutex<Vec<TcpStream>>>,
    requests: Arc<AtomicUsize>,
    summary_closed: Arc<(Mutex<Option<bool>>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

fn request(socket: &mut TcpStream) -> Value {
    socket.set_read_timeout(Some(RUN_TIMEOUT)).unwrap();
    let mut bytes = Vec::new();
    let mut part = [0; 8192];
    let head = loop {
        let read = socket.read(&mut part).expect("request bytes");
        assert_ne!(read, 0, "request ended before headers");
        bytes.extend_from_slice(&part[..read]);
        if let Some(head) = find(&bytes, b"\r\n\r\n") {
            break head;
        }
        assert!(bytes.len() < 1 << 20, "bounded request headers");
    };
    let headers = String::from_utf8_lossy(&bytes[..head]);
    let length: usize = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .unwrap()
        .1
        .trim()
        .parse()
        .unwrap();
    assert!(length < 1 << 20, "bounded fixture request");
    while bytes.len() < head + 4 + length {
        let read = socket.read(&mut part).expect("request body");
        assert_ne!(read, 0, "request ended before its body");
        bytes.extend_from_slice(&part[..read]);
    }
    serde_json::from_slice(&bytes[head + 4..head + 4 + length]).unwrap()
}

fn respond(socket: &mut TcpStream, reply: &Reply) {
    socket.set_write_timeout(Some(RUN_TIMEOUT)).unwrap();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        reply.body.len(),
        reply.body
    );
    socket.write_all(response.as_bytes()).unwrap();
    socket.flush().unwrap();
}

impl HeldSummaryServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new((Mutex::new(false), Condvar::new()));
        let sockets = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(AtomicUsize::new(0));
        let summary_closed = Arc::new((Mutex::new(None), Condvar::new()));
        let thread = {
            let stop = Arc::clone(&stop);
            let ready = Arc::clone(&ready);
            let sockets = Arc::clone(&sockets);
            let requests = Arc::clone(&requests);
            let summary_closed = Arc::clone(&summary_closed);
            std::thread::spawn(move || {
                let foreground = Arc::new(AtomicUsize::new(0));
                let mut handlers = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            // Winsock accepts inherit the listener's nonblocking mode.
                            socket.set_nonblocking(false).unwrap();
                            sockets.lock().unwrap().push(socket.try_clone().unwrap());
                            let stop = Arc::clone(&stop);
                            let ready = Arc::clone(&ready);
                            let requests = Arc::clone(&requests);
                            let summary_closed = Arc::clone(&summary_closed);
                            let foreground = Arc::clone(&foreground);
                            handlers.push(std::thread::spawn(move || {
                                let body = request(&mut socket);
                                requests.fetch_add(1, Ordering::SeqCst);
                                if body["tools"].as_array().is_none_or(Vec::is_empty) {
                                    *ready.0.lock().unwrap() = true;
                                    ready.1.notify_all();
                                    // No provider usage has arrived. Cancellation must
                                    // retain this paid dispatch with usage missing.
                                    let mut byte = [0];
                                    let closed = match socket.read(&mut byte) {
                                        Ok(0) => true,
                                        Err(error) => matches!(
                                            error.kind(),
                                            std::io::ErrorKind::ConnectionReset
                                                | std::io::ErrorKind::ConnectionAborted
                                                | std::io::ErrorKind::BrokenPipe
                                        ),
                                        Ok(_) => false,
                                    };
                                    *summary_closed.0.lock().unwrap() = Some(closed);
                                    summary_closed.1.notify_all();
                                } else if foreground.fetch_add(1, Ordering::SeqCst) == 0 {
                                    let mut reply = Reply::read_call();
                                    reply.body = reply
                                        .body
                                        .replace("\"input_tokens\":40", "\"input_tokens\":6500")
                                        .replace("\"total_tokens\":48", "\"total_tokens\":6508");
                                    respond(&mut socket, &reply);
                                } else {
                                    let (started, timeout) = ready
                                        .1
                                        .wait_timeout_while(
                                            ready.0.lock().unwrap(),
                                            RUN_TIMEOUT,
                                            |started| !*started && !stop.load(Ordering::SeqCst),
                                        )
                                        .unwrap();
                                    assert!(
                                        !timeout.timed_out(),
                                        "summary request reached the server"
                                    );
                                    if *started && !stop.load(Ordering::SeqCst) {
                                        respond(&mut socket, &Reply::sse());
                                    }
                                }
                            }));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
                for handler in handlers {
                    handler.join().expect("HTTP fixture worker");
                }
            })
        };
        Self {
            port,
            stop,
            ready,
            sockets,
            requests,
            summary_closed,
            thread: Some(thread),
        }
    }
}

impl Drop for HeldSummaryServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.ready.1.notify_all();
        for socket in self.sockets.lock().unwrap().iter() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn final_background_dispatch_is_accounted_before_json_print_exit() {
    let server = HeldSummaryServer::start();
    let workspace = Workspace::new("background-accounting", server.port);
    let path = workspace.home().join("models.json");
    let mut models: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    models["providers"]["loop"]["models"][0]["contextWindow"] = json!(8192);
    models["providers"]["loop"]["models"][0]["maxTokens"] = json!(512);
    std::fs::write(path, models.to_string()).unwrap();
    std::fs::write(
        workspace.home().join("settings.json"),
        json!({
            "backgroundCompaction":true,
            "compaction":{"reserveTokens":1024,"keepRecentTokens":1},
            "retry":{"enabled":false}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(workspace.project().join("notes.txt"), "fixture note").unwrap();
    let output = workspace.run(
        &[
            "-p",
            "--mode",
            "json",
            "--no-session",
            "--model",
            "loop/loop-model",
            "Read notes.txt, then finish.",
        ],
        None,
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    let events: Vec<Value> = text(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let lifecycle: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "background_compaction")
        .collect();
    assert_eq!(
        lifecycle.len(),
        2,
        "each admitted candidate has a terminal event"
    );
    assert_eq!(lifecycle[0]["status"], "started");
    assert_eq!(lifecycle[1]["status"], "discarded");
    let attempts = lifecycle[1]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["dispatched"], true);
    assert!(
        attempts[0]["usage"].is_null(),
        "unreported usage stays missing"
    );
    let foreground = events
        .iter()
        .filter(|event| event["type"] == "message_end" && event["message"]["role"] == "assistant")
        .count();
    assert_eq!(foreground, 2);
    assert_eq!(
        server.requests.load(Ordering::SeqCst),
        foreground + attempts.len()
    );
    let (closed, timeout) = server
        .summary_closed
        .1
        .wait_timeout_while(
            server.summary_closed.0.lock().unwrap(),
            RUN_TIMEOUT,
            |closed| closed.is_none(),
        )
        .unwrap();
    assert!(!timeout.timed_out(), "server observed the connection close");
    assert_eq!(
        *closed,
        Some(true),
        "print cleans up its provider connection"
    );
}
