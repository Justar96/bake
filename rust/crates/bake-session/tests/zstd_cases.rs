//! Shared source-authored compressed logs, including physical recovery offsets.
//! Expectations were fixed before either arm ran; no TypeScript output is read.
use bake_session::{PathPlatform, RestoreRefusal, ScanRefusal, restore_zstd_log};
use serde_json::{Value, json};

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
