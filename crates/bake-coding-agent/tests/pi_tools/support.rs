//! Reading `tests/fixtures/pi-tools/golden.json`, recorded from Pi v1.1.0:
//! building a case's files and encoding a Bake result in the fixture format.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine as _;
use serde_json::{Map, Value, json};

/// The recorded fixture.
pub fn golden() -> &'static Value {
    static GOLDEN: OnceLock<Value> = OnceLock::new();
    GOLDEN.get_or_init(|| {
        let text = include_str!("../fixtures/pi-tools/golden.json");
        serde_json::from_str(text).expect("golden.json parses")
    })
}

pub const DIR: &str = "<DIR>";

/// A directory the test owns; removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "bake-pi-tools-{}-{}-{label}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("create temp dir");
        // Canonical, as the generator's `realpathSync`.
        Self(std::fs::canonicalize(&base).expect("canonical temp dir"))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn text(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Read-only files do not stop removal of their directory on Unix.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn materialize_str(text: &str, i: usize) -> String {
    text.replace("{i}", &i.to_string())
}

/// The generator's `materialize`.
pub fn materialize(parts: &Value, i: usize) -> String {
    match parts {
        Value::String(text) => materialize_str(text, i),
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part {
                Value::String(text) => materialize_str(text, i),
                Value::Array(items) => match items[0].as_str() {
                    Some("repeat") => items[1]
                        .as_str()
                        .expect("repeat text")
                        .repeat(items[2].as_u64().expect("count") as usize),
                    Some("pad") => {
                        let width = items[1].as_u64().expect("width") as usize;
                        format!("{i:0>width$}")
                    }
                    Some("lines") => {
                        let count = items[1].as_u64().expect("count") as usize;
                        let separator = items.get(3).and_then(Value::as_str).unwrap_or("\n");
                        (1..=count)
                            .map(|k| materialize(&items[2], k))
                            .collect::<Vec<_>>()
                            .join(separator)
                    }
                    other => panic!("unknown part {other:?}"),
                },
                other => panic!("unknown part {other}"),
            })
            .collect(),
        other => panic!("unknown text {other}"),
    }
}

/// The generator's `setup`: writes `files` under a fresh directory.
pub fn setup(files: &Value, label: &str) -> TempDir {
    let dir = TempDir::new(label);
    for file in files.as_array().expect("files") {
        let path = dir.path().join(file["path"].as_str().expect("path"));
        if file.get("dir").and_then(Value::as_bool) == Some(true) {
            std::fs::create_dir_all(&path).expect("mkdir");
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parent");
        }
        if let Some(series) = file.get("series") {
            let suffix = materialize(&series["suffix"], 0);
            let width = series["pad"].as_u64().expect("pad") as usize;
            for i in 0..series["count"].as_u64().expect("count") as usize {
                std::fs::write(path.join(format!("{i:0>width$}{suffix}")), "").expect("write");
            }
            continue;
        }
        if let Some(target) = file.get("symlink").and_then(Value::as_str) {
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &path).expect("symlink");
            #[cfg(not(unix))]
            let _ = target;
            continue;
        }
        let bytes = match file.get("base64").and_then(Value::as_str) {
            Some(data) => base64::engine::general_purpose::STANDARD
                .decode(data)
                .expect("base64"),
            None => materialize(file.get("text").unwrap_or(&json!("")), 0).into_bytes(),
        };
        std::fs::write(&path, bytes).expect("write");
        #[cfg(unix)]
        if let Some(mode) = file.get("mode").and_then(Value::as_u64) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode as u32))
                .expect("chmod");
        }
    }
    dir
}

/// `<DIR>` replaced by `dir` in every string of `value`.
pub fn substitute(value: &Value, dir: &str) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace(DIR, dir)),
        Value::Array(items) => Value::Array(items.iter().map(|v| substitute(v, dir)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), substitute(v, dir)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The generator's `fnv1a64`.
pub fn fnv1a64(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The generator's `compact`: strings over 4096 UTF-16 units become their
/// hash, UTF-8 length, and ends. Integral numbers are written as integers,
/// as `JSON.stringify` writes them.
pub fn compact(value: &Value) -> Value {
    match value {
        Value::String(text) => {
            let units: Vec<u16> = text.encode_utf16().collect();
            if units.len() <= 4096 {
                return value.clone();
            }
            json!({
                "$hash": fnv1a64(text),
                "bytes": text.len(),
                "head": String::from_utf16_lossy(&units[..120]),
                "tail": String::from_utf16_lossy(&units[units.len() - 120..]),
            })
        }
        Value::Array(items) => Value::Array(items.iter().map(compact).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), compact(v)))
                .collect::<Map<_, _>>(),
        ),
        Value::Number(number) => match number.as_f64() {
            Some(float) if number.is_f64() && float.fract() == 0.0 && float.abs() < 9e15 => {
                json!(float as i64)
            }
            _ => value.clone(),
        },
        other => other.clone(),
    }
}

/// The generator's `scrub`: `dir` replaced by `<DIR>`, then compacted.
pub fn scrub(value: &Value, dir: &str) -> Value {
    fn replace(value: &Value, dir: &str) -> Value {
        match value {
            Value::String(text) => Value::String(text.replace(dir, DIR)),
            Value::Array(items) => Value::Array(items.iter().map(|v| replace(v, dir)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), replace(v, dir)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    compact(&replace(value, dir))
}

/// Whether the current user can write a file its mode makes read-only, as
/// root can.
#[cfg(unix)]
pub fn ignores_file_modes() -> bool {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("mode-probe");
    let file = dir.path().join("probe");
    std::fs::write(&file, "x").expect("write probe");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).expect("chmod");
    std::fs::OpenOptions::new().write(true).open(&file).is_ok()
}
