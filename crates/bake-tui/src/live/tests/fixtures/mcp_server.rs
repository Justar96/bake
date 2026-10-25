//! A std-only native MCP fixture. A private loopback connection triggers
//! state changes independently of prompts, without polling or sleeps.

use std::io::{BufRead, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn send(message: &str) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{message}").unwrap();
    out.flush().unwrap();
}

fn main() {
    let mut args = std::env::args().skip(1);
    let control = std::net::TcpStream::connect(args.next().unwrap()).unwrap();
    let fail_start = args.next().as_deref() == Some("fail-start");
    let broken = Arc::new(AtomicBool::new(false));
    let controller = Arc::clone(&broken);
    std::thread::spawn(move || {
        let mut control = std::io::BufReader::new(control);
        loop {
            let mut request = String::new();
            if control.read_line(&mut request).unwrap() == 0 {
                return;
            }
            let name = request.trim_end();
            if name != "disconnect" {
                controller.store(name == "refresh_fail", Ordering::SeqCst);
                send(r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#);
            }
            writeln!(control.get_mut(), "{name}").unwrap();
            control.get_mut().flush().unwrap();
            if name == "disconnect" {
                eprintln!("SYNTHETIC_STDERR_SECRET\u{202e}");
                std::process::exit(0);
            }
        }
    });
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        // Bake's client sends numeric request ids. Notifications have none.
        let Some(id) = line.split_once("\"id\":").map(|(_, rest)| {
            rest.trim_start()
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap()
        }) else {
            continue;
        };
        if line.contains("\"initialize\"") {
            if fail_start {
                eprintln!("SYNTHETIC_STDERR_SECRET\u{1b}[2J");
                send(&format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32000,"message":"SYNTHETIC_SERVER_SECRET"}}}}"#
                ));
                return;
            }
            send(&format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"2025-11-25","capabilities":{{"tools":{{"listChanged":true}}}},"serverInfo":{{"name":"fixture","version":"1"}}}}}}"#
            ));
        } else if line.contains("\"tools/list\"") {
            if broken.load(Ordering::SeqCst) {
                send(&format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32000,"message":"SYNTHETIC_SERVER_SECRET"}}}}"#
                ));
            } else {
                send(&format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[{{"name":"fixture_tool","inputSchema":{{"type":"object","properties":{{}}}}}}]}}}}"#
                ));
            }
        }
    }
}
