//! Pi v1.1.0's output handling. Golden results execute its production helpers;
//! explicit regressions below port the output cases of `test/tools.test.ts`
//! and the multibyte case of `test/mcp-extension.test.ts`.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use bake_coding_agent::tools::output_accumulator::{OutputAccumulator, OutputAccumulatorOptions};
use bake_coding_agent::tools::truncate::{self, TruncatedBy, TruncationOptions};
use serde_json::{Value, json};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::getrandom(&mut random).unwrap();
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("bake-output-test-{id}"));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn accumulator(&self, max_lines: usize, max_bytes: usize) -> OutputAccumulator {
        OutputAccumulator::new(OutputAccumulatorOptions {
            limits: TruncationOptions {
                max_lines,
                max_bytes,
            },
            temp_directory: self.0.clone(),
            ..Default::default()
        })
        .unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/pi-output/golden.json")).unwrap()
}

fn snapshot(accumulator: &OutputAccumulator) -> Value {
    let snapshot = accumulator.snapshot().unwrap();
    json!({
        "content": snapshot.content,
        "truncation": snapshot.truncation,
        "spilled": snapshot.full_output_path.is_some(),
        "lastLineBytes": accumulator.last_line_bytes(),
    })
}

#[test]
fn truncation_matches_pi_golden_results() {
    let oracle = golden();
    for case in oracle["truncations"].as_array().unwrap() {
        let content = case["content"].as_str().unwrap();
        let options = serde_json::from_value(case["options"].clone()).unwrap();
        assert_eq!(
            json!(truncate::truncate_head(content, options)),
            case["head"],
            "{case}"
        );
        assert_eq!(
            json!(truncate::truncate_tail(content, options)),
            case["tail"],
            "{case}"
        );
    }
    for case in oracle["middle"].as_array().unwrap() {
        assert_eq!(
            json!(truncate::truncate_middle(
                case["content"].as_str().unwrap(),
                case["maxBytes"].as_u64().unwrap() as usize
            )),
            case["result"],
            "{case}"
        );
    }
    for case in oracle["line"].as_array().unwrap() {
        assert_eq!(
            json!(truncate::truncate_line(
                case["content"].as_str().unwrap(),
                case["maxChars"].as_u64().unwrap() as usize
            )),
            case["result"],
            "{case}"
        );
    }
    for case in oracle["sizes"].as_array().unwrap() {
        assert_eq!(
            truncate::format_size(case["bytes"].as_u64().unwrap() as usize),
            case["result"].as_str().unwrap(),
            "{case}"
        );
    }
}

#[test]
fn streaming_snapshots_raw_files_and_capped_reads_match_pi() {
    let root = TempDir::new();
    for case in golden()["streams"].as_array().unwrap() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions {
            limits: serde_json::from_value(case["options"].clone()).unwrap(),
            temp_directory: root.0.clone(),
            ..Default::default()
        })
        .unwrap();
        let mut snapshots = Vec::new();
        for chunk in case["chunks"].as_array().unwrap() {
            let bytes: Vec<u8> = serde_json::from_value(chunk.clone()).unwrap();
            accumulator.append(&bytes).unwrap();
            snapshots.push(snapshot(&accumulator));
        }
        accumulator.finish().unwrap();
        snapshots.push(snapshot(&accumulator));
        assert_eq!(json!(snapshots), case["snapshots"], "{}", case["name"]);
        if let Some(path) = accumulator.snapshot().unwrap().full_output_path {
            assert_eq!(
                json!(fs::read(path).unwrap()),
                case["raw"],
                "{}",
                case["name"]
            );
        } else {
            assert!(case["raw"].is_null());
        }
        for read in case["reads"].as_array().unwrap() {
            let actual = accumulator
                .read_full_output(read["maxBytes"].as_u64().unwrap() as usize)
                .unwrap();
            assert_eq!(
                json!({"content": actual.content, "truncated": actual.truncated}),
                read["result"],
                "{}: {read}",
                case["name"]
            );
        }
    }
}

#[test]
fn trailing_newline_is_not_an_extra_line_and_line_only_truncation_spills() {
    // Pi tools.test.ts: 4000 lines ending in LF retain lines 2001–4000.
    let root = TempDir::new();
    let mut accumulator = root.accumulator(2000, 50 * 1024);
    let mut original = String::new();
    for i in 1..=4000 {
        let line = format!("line-{i:04}\n");
        accumulator.append(line.as_bytes()).unwrap();
        original.push_str(&line);
    }
    accumulator.finish().unwrap();
    let result = accumulator.snapshot().unwrap();
    assert_eq!(result.truncation.total_lines, 4000);
    assert_eq!(result.truncation.output_lines, 2000);
    assert_eq!(result.truncation.truncated_by, Some(TruncatedBy::Lines));
    assert!(result.content.starts_with("line-2001\n"));
    assert!(result.content.ends_with("line-4000"));
    assert_eq!(
        fs::read_to_string(result.full_output_path.unwrap()).unwrap(),
        original
    );
}

#[test]
fn split_utf8_euro_sign_is_preserved() {
    // Pi tools.test.ts: the Euro sign's leading byte arrives alone.
    let root = TempDir::new();
    let mut accumulator = root.accumulator(2000, 50 * 1024);
    let bytes = "€\n".as_bytes();
    accumulator.append(&bytes[..1]).unwrap();
    assert_eq!(accumulator.snapshot().unwrap().content, "");
    accumulator.append(&bytes[1..]).unwrap();
    accumulator.finish().unwrap();
    assert_eq!(accumulator.snapshot().unwrap().content, "€\n");
    assert_eq!(accumulator.last_line_bytes(), 0);
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
}

#[test]
fn middle_truncation_cuts_multibyte_text_only_at_character_boundaries() {
    // Pi mcp-extension.test.ts: odd byte allowance splits two-byte characters.
    let text = format!("{}end", "é".repeat(20_000));
    let result = truncate::truncate_middle(&text, 1001);
    assert!(result.truncated);
    assert!(!result.content.contains('\u{FFFD}'));
    assert!(result.content.ends_with("end"));
    let marker = format!("…{} chars truncated…", result.removed_chars);
    let (head, tail) = result.content.split_once(&marker).unwrap();
    assert!(head.len() <= 500);
    assert!(tail.len() <= 501);
    assert_eq!(
        head.chars().count() + tail.chars().count() + result.removed_chars,
        text.chars().count()
    );
}

#[test]
fn finish_is_idempotent_closes_the_file_and_rejects_further_appends() {
    let root = TempDir::new();
    let mut accumulator = root.accumulator(10, 2);
    assert_eq!(
        accumulator.read_full_output(10).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    accumulator.append(b"x\xe2").unwrap();
    assert!(accumulator.snapshot().unwrap().full_output_path.is_none());
    accumulator.finish().unwrap();
    let first = accumulator.snapshot().unwrap();
    assert_eq!(first.content, "");
    assert_eq!(first.truncation.total_bytes, 4);
    accumulator.finish().unwrap();
    assert_eq!(accumulator.snapshot().unwrap(), first);
    assert_eq!(
        accumulator.append(b"late").unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        accumulator.read_full_output(100).unwrap().content,
        "x\u{FFFD}"
    );
    let path = first.full_output_path.unwrap();
    let moved = path.with_extension("moved");
    fs::rename(&path, &moved).unwrap();
    assert_eq!(fs::read(&moved).unwrap(), b"x\xe2");
    assert_eq!(
        accumulator.read_full_output(100).unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn failed_spill_cannot_report_complete_output_or_accept_more_data() {
    let root = TempDir::new();
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions {
        limits: TruncationOptions {
            max_lines: 10,
            max_bytes: 2,
        },
        temp_directory: root.0.join("does-not-exist"),
        ..Default::default()
    })
    .unwrap();
    accumulator.append(b"ok").unwrap();
    assert_eq!(
        accumulator.append(b"overflow").unwrap_err().kind(),
        ErrorKind::NotFound
    );
    assert!(accumulator.snapshot().is_err());
    assert!(accumulator.finish().is_err());
    assert!(accumulator.read_full_output(100).is_err());
    assert_eq!(
        accumulator.append(b"late").unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
}

#[test]
fn prefixes_cannot_escape_the_private_directory() {
    for prefix in ["", "../elsewhere", "/absolute", "a/b", "a\\b", "a\0b", "💣"] {
        assert_eq!(
            OutputAccumulator::new(OutputAccumulatorOptions {
                temp_file_prefix: prefix.to_owned(),
                ..Default::default()
            })
            .unwrap_err()
            .kind(),
            ErrorKind::InvalidInput
        );
    }
}

#[test]
fn spilled_output_survives_owner_drop() {
    let root = TempDir::new();
    let path = {
        let mut accumulator = root.accumulator(1, 1);
        accumulator.append(b"raw\xffbytes\n").unwrap();
        accumulator.finish().unwrap();
        accumulator.snapshot().unwrap().full_output_path.unwrap()
    };
    assert_eq!(fs::read(path).unwrap(), b"raw\xffbytes\n");
}

#[cfg(unix)]
#[test]
fn spills_are_private_files_inside_private_directories() {
    use std::os::unix::fs::PermissionsExt;
    let root = TempDir::new();
    let mut accumulator = root.accumulator(1, 1);
    accumulator.append(b"secret").unwrap();
    accumulator.finish().unwrap();
    let path = accumulator.snapshot().unwrap().full_output_path.unwrap();
    let parent = path.parent().unwrap();
    assert_eq!(parent.parent(), Some(root.0.as_path()));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(parent).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn arbitrary_byte_sequences_decode_without_panicking_at_any_two_chunk_split() {
    let root = TempDir::new();
    let mut seed = 0xdeadbeef_u32;
    for _ in 0..128 {
        let bytes: Vec<u8> = (0..32)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let expected = String::from_utf8_lossy(&bytes);
        for split in 0..=bytes.len() {
            let mut accumulator = root.accumulator(2000, 50 * 1024);
            accumulator.append(&bytes[..split]).unwrap();
            accumulator.append(&bytes[split..]).unwrap();
            accumulator.finish().unwrap();
            assert_eq!(
                accumulator.snapshot().unwrap().content,
                expected,
                "{bytes:?}, split {split}"
            );
        }
    }
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
}

#[test]
fn extreme_limits_do_not_overflow() {
    let root = TempDir::new();
    for options in [
        TruncationOptions {
            max_lines: usize::MAX,
            max_bytes: usize::MAX,
        },
        TruncationOptions {
            max_lines: 0,
            max_bytes: 0,
        },
    ] {
        for text in ["", "🍞\n", "abc\n\nlast"] {
            let head = truncate::truncate_head(text, options);
            let tail = truncate::truncate_tail(text, options);
            assert!(head.output_bytes <= options.max_bytes);
            assert!(tail.output_bytes <= options.max_bytes);
            let mut accumulator = root.accumulator(options.max_lines, options.max_bytes);
            accumulator.append(text.as_bytes()).unwrap();
            accumulator.finish().unwrap();
            assert!(accumulator.snapshot().unwrap().content.len() <= options.max_bytes);
        }
    }
}
