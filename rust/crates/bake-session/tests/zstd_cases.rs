//! Shared source-authored compressed logs, including physical recovery offsets.
//! Expectations were fixed before either arm ran; no TypeScript output is read.
//!
//! `shared_zstd_frame_cases` compresses each input of
//! `conformance/session/zstd-frame-cases.json` with `compress_zstd_frame`
//! and compares the frame byte for byte, or by length and SHA-256, with the
//! frame TypeScript's `compressZstdFrame` wrote for it, after checking that
//! the vendored libzstd is the version the table names.
use bake_session::{
    PathPlatform, RestoreRefusal, ScanRefusal, compress_zstd_frame, restore_zstd_log,
};
use std::collections::BTreeSet;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const FRAME_CASE_COUNT: usize = 13;
const FRAME_ORACLE: &str = "compress the input's bytes with the JSONL backend's compressZstdFrame, Node's asynchronous zstdCompress with ZSTD_c_checksumFlag set; list the frame's hex when it is at most 512 bytes, otherwise its length and SHA-256";

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// An object's keys, sorted, so a match does not rely on serde_json's
/// member order.
fn sorted_keys<'a>(value: &'a Value, id: &str) -> Vec<&'a str> {
    let keys: BTreeSet<&str> = value
        .as_object()
        .unwrap_or_else(|| panic!("{id}: expected an object"))
        .keys()
        .map(String::as_str)
        .collect();
    keys.into_iter().collect()
}

/// The bytes a frame case's `input` stands for.
fn frame_input(input: &Value, id: &str) -> Vec<u8> {
    let count = |key: &str| usize::try_from(input[key].as_u64().expect(key)).expect(key);
    match sorted_keys(input, id).as_slice() {
        ["text"] => input["text"].as_str().expect("text").as_bytes().to_vec(),
        ["repeat", "times"] => input["repeat"]
            .as_str()
            .expect("repeat")
            .repeat(count("times"))
            .into_bytes(),
        ["rows"] => (0..count("rows"))
            .map(|k| {
                format!(
                    "{{\"type\":\"turn/start\",\"seq\":{k},\"time\":{},\"data\":{{\"turn\":{k}}}}}\n",
                    k + 1
                )
            })
            .collect::<String>()
            .into_bytes(),
        ["bytes", "xorshift"] => {
            let mut x = u32::try_from(input["xorshift"].as_u64().expect("seed")).expect("seed");
            (0..count("bytes"))
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x.to_le_bytes()[0]
                })
                .collect()
        }
        other => panic!("{id}: unknown input {other:?}"),
    }
}

#[test]
fn shared_zstd_frame_cases() {
    let table: Value = serde_json::from_str(include_str!(
        "../../../../conformance/session/zstd-frame-cases.json"
    ))
    .expect("table");
    assert_eq!(
        sorted_keys(&table, "table"),
        ["cases", "history", "libzstd", "oracle", "schema", "version"]
    );
    assert_eq!(table["schema"], "bake/session-conformance/zstd-frame-cases");
    assert_eq!(table["version"], 1);
    assert_eq!(table["oracle"], FRAME_ORACLE);
    assert!(
        table["history"]
            .as_array()
            .expect("history")
            .iter()
            .all(Value::is_string)
    );
    // The frames are libzstd's bytes for that version; another fails here.
    assert_eq!(
        table["libzstd"],
        zstd_safe::version_string(),
        "the vendored libzstd is not the version the frames were recorded with"
    );
    let cases = table["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), FRAME_CASE_COUNT);
    let mut ids = BTreeSet::new();
    for case in cases {
        let id = case["id"].as_str().expect("id");
        assert!(ids.insert(id), "duplicate {id}");
        let keys = sorted_keys(case, id);
        assert!(
            keys == ["frameHex", "id", "input", "inputBytes", "note"]
                || keys
                    == [
                        "frameBytes",
                        "frameSha256",
                        "id",
                        "input",
                        "inputBytes",
                        "note"
                    ],
            "{id}: keys {keys:?}"
        );
        assert!(case["note"].is_string(), "{id}: note");
        let input = frame_input(&case["input"], id);
        assert_eq!(
            Some(input.len() as u64),
            case["inputBytes"].as_u64(),
            "{id}"
        );
        let frame = compress_zstd_frame(&input).expect("compress");
        if let Some(hex) = case.get("frameHex") {
            assert_eq!(to_hex(&frame), hex.as_str().expect("hex"), "{id}");
        } else {
            assert_eq!(
                Some(frame.len() as u64),
                case["frameBytes"].as_u64(),
                "{id}"
            );
            assert_eq!(
                to_hex(&Sha256::digest(&frame)),
                case["frameSha256"].as_str().expect("sha"),
                "{id}"
            );
        }
    }
}

#[test]
fn shared_zstd_cases() {
    let table: Value = serde_json::from_str(include_str!(
        "../../../../conformance/session/zstd-cases.json"
    ))
    .unwrap();
    assert_eq!(table["schema"], "bake/session-conformance/zstd-cases");
    assert_eq!(table["version"], 1);
    let cases = table["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 49);
    let mut ids = std::collections::BTreeSet::new();
    for case in cases {
        let id = case["id"].as_str().unwrap();
        assert!(ids.insert(id), "duplicate {id}");
        let hex = case["hex"].as_str().unwrap();
        assert_eq!(hex.len() % 2, 0);
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let max = usize::try_from(case["maxPlaintextBytes"].as_u64().unwrap()).unwrap();
        let actual = match restore_zstd_log(&bytes, PathPlatform::Posix, 64, max) {
            Ok(restored) => {
                let stored = restored.stored();
                let header = stored.header();
                json!({"outcome":"restored", "header":{
                    "version":3,"id":header.id,"createdAt":header.created_at,
                    "isSeeded":header.is_seeded,"delegationDepth":header.delegation_depth},
                    "rows":stored.rows(), "inheritedEventCount":stored.inherited_event_count(),
                    "committedBytes":stored.committed_bytes(),
                    "torn":restored.torn().map(|tail| json!({"truncateTo":tail.truncate_to,"recoveredFrom":tail.recovered_from})),
                    "closers":restored.closers(),"endSeedAppended":restored.end_seed_appended(),
                    "messages":restored.messages(),"requestHeader":restored.request_header(),
                    "toolHistory":restored.tool_history(),"requestContext":restored.request_context()})
            }
            Err(RestoreRefusal::Zstd(error)) => {
                json!({"outcome":"refused","message":error.message()})
            }
            Err(RestoreRefusal::Scan(ScanRefusal::Unsupported { .. })) => {
                json!({"outcome":"unsupported"})
            }
            Err(RestoreRefusal::Scan(error)) => {
                json!({"outcome":"refused","message":error.message().expect("message in qualified corpus")})
            }
            Err(RestoreRefusal::NativePlaintextBudget {
                max_plaintext_bytes,
            }) => {
                assert_eq!(max_plaintext_bytes, max);
                json!({"outcome":"plaintext-budget"})
            }
            Err(other) => panic!("{id}: unexpected refusal {other:?}"),
        };
        assert_eq!(
            &actual,
            case.get("rust").unwrap_or(&case["expected"]),
            "{id}"
        );
    }
}

#[test]
fn plain_tail_offsets_are_physical_and_recover_no_rows() {
    let original = include_bytes!(
        "../../../../conformance/runtime/request-reconstruction/tool-call-turn/session.jsonl"
    );
    let complete = bake_session::restore_plain_log(original, PathPlatform::Posix, 64).unwrap();
    assert_eq!(complete.torn(), None);
    for tail in [b"partial".as_slice(), b"bad\n".as_slice()] {
        let input = [original.as_slice(), tail].concat();
        let restored = bake_session::restore_plain_log(&input, PathPlatform::Posix, 64).unwrap();
        assert_eq!(restored.stored(), complete.stored());
        assert_eq!(
            restored.torn(),
            Some(bake_session::TornTail {
                truncate_to: original.len(),
                recovered_from: complete.stored().rows().len(),
            })
        );
    }
}
