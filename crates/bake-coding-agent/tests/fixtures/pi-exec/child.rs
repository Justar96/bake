// A dependency-free subprocess fixture shared by the Pi recorder and Rust tests.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args[0].as_str() {
        "capture" => {
            std::io::stdout().write_all(args[1].as_bytes()).unwrap();
            std::io::stderr().write_all(args[2].as_bytes()).unwrap();
            std::process::exit(args[3].parse().unwrap());
        }
        "stdin" => {
            let mut data = Vec::new();
            std::io::stdin().read_to_end(&mut data).unwrap();
            print!("{}", data.len());
        }
        "large" => {
            for _ in 0..64 {
                std::io::stdout().write_all(&[b'o'; 8192]).unwrap();
                std::io::stderr().write_all(&[b'e'; 8192]).unwrap();
            }
        }
        "cwd" => print!("{}", std::fs::read_to_string("marker").unwrap()),
        "create" => std::fs::write(&args[1], "ran").unwrap(),
        "env" => print!("{}", std::env::var(&args[1]).unwrap_or_default()),
        "tree" | "orphan" => {
            let _child = Command::new(std::env::current_exe().unwrap())
                .args(["leaf", &args[1]])
                .stdin(Stdio::null())
                .spawn()
                .unwrap();
            if args[0] == "tree" {
                wait(&args[1]);
            } else {
                println!("HEAD");
            }
        }
        "leaf" => wait(&args[1]),
        _ => panic!("unknown fixture mode"),
    }
}

fn wait(address: &str) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.write_all(b"ready").unwrap();
    let mut command = [0];
    loop {
        stream.read_exact(&mut command).unwrap();
        match command[0] {
            b'x' => break,
            b'o' => {
                print!("OUT");
                std::io::stdout().flush().unwrap();
            }
            b'e' => {
                eprint!("ERR");
                std::io::stderr().flush().unwrap();
            }
            _ => panic!("unknown command"),
        }
    }
}
