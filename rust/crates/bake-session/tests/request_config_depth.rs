//! Config members survive restoration and replay, including deep members
//! discarded by request assembly's object spread overwrites.

use bake_session::{
    PathPlatform, dismantle, json_text, replay_requests, replay_restored_requests,
    restore_plain_log,
};

const DEPTH: usize = 10_000;
const STACK: usize = 256 * 1024;

fn log(deep: &str, tools: &str) -> Vec<u8> {
    format!(
        concat!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"config\",\"createdAt\":5,\"isSeeded\":false,\"delegationDepth\":0}}\n",
            "{{\"type\":\"turn/start\",\"seq\":0,\"time\":100,\"data\":{{\"turn\":1}}}}\n",
            "{{\"type\":\"step/start\",\"seq\":1,\"time\":101,\"data\":{{\"turn\":1,\"step\":1}}}}\n",
            "{{\"type\":\"request/header\",\"seq\":2,\"time\":102,\"data\":{{\"header\":{{\"config\":{{\"provider\":\"p\",\"model\":\"m\",\"extra\":{deep},\"messages\":{deep},\"toolHistory\":{deep},\"sessionId\":{deep},\"tools\":{deep},\"signal\":{deep}}}{tools}}},\"reason\":\"initial\"}}}}\n",
            "{{\"type\":\"assistant/message\",\"seq\":3,\"time\":103,\"data\":{{\"turn\":1,\"step\":1,\"message\":{{\"id\":\"a1\",\"role\":\"assistant\",\"source\":{{\"kind\":\"model\",\"provider\":\"p\",\"model\":\"m\"}},\"content\":[{{\"type\":\"text\",\"text\":\"ok\"}}]}},\"stream\":[]}},\"surfaceOp\":\"append\"}}\n",
            "{{\"type\":\"step/end\",\"seq\":4,\"time\":104,\"data\":{{\"turn\":1,\"step\":1}}}}\n",
            "{{\"type\":\"turn/end\",\"seq\":5,\"time\":105,\"data\":{{\"turn\":1,\"reason\":{{\"kind\":\"completed\"}}}}}}\n",
        ),
        deep = deep,
        tools = tools,
    )
    .into_bytes()
}

fn read_config(object: bool) {
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || {
            let deep = if object {
                format!("{}1{}", "{\"k\":".repeat(DEPTH), "}".repeat(DEPTH))
            } else {
                format!("{}1{}", "[".repeat(DEPTH), "]".repeat(DEPTH))
            };
            for (header_tools, expected_tools, history_tools) in [
                ("", deep.as_str(), "[]"),
                (
                    ",\"tools\":[{\"name\":\"t\",\"parameters\":{}}]",
                    "[{\"name\":\"t\",\"parameters\":{}}]",
                    "[{\"name\":\"t\",\"parameters\":{}}]",
                ),
            ] {
                let bytes = log(&deep, header_tools);
                let expected = format!(
                    r#"{{"provider":"p","model":"m","extra":{deep},"messages":[],"toolHistory":{{"tools":{history_tools},"updates":[]}},"sessionId":"config","tools":{expected_tools},"signal":{deep}}}"#
                );
                let requests = replay_requests(&bytes, PathPlatform::Posix, 64).expect("replay");
                assert_eq!(requests.len(), 1);
                let value = requests[0].to_json();
                let text = json_text(&value);
                dismantle(value);
                assert!(text == expected, "config spread differs");
                drop(requests);

                let restored = restore_plain_log(&bytes, PathPlatform::Posix, 64).expect("restore");
                let header = restored.request_header().expect("header");
                let header_text = json_text(&header);
                dismantle(header);
                assert!(header_text.contains(&format!(r#""messages":{deep}"#)));
                let requests = replay_restored_requests(&restored).expect("restored replay");
                assert_eq!(requests.len(), 1);
                let value = requests[0].to_json();
                let text = json_text(&value);
                dismantle(value);
                assert!(text == expected, "restored config spread differs");
                drop(requests);
                drop(restored);
            }
        })
        .expect("spawn")
        .join()
        .expect("config is copied and discarded without recursion");
}

#[test]
fn array_config_members_ten_thousand_deep() {
    read_config(false);
}

#[test]
fn object_config_members_ten_thousand_deep() {
    read_config(true);
}
