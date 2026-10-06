use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

fn conformance_dir(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../conformance")
        .join(kind)
}

fn json_files(kind: &str) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(conformance_dir(kind))
        .expect("shared conformance directory exists")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read(&path).expect("fixture is readable"))
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no {kind} fixtures found");
    files
}

/// Creates the directory atomically with mode 0700 on Unix; Windows keeps
/// its default ACL.
fn private_dir_builder() -> fs::DirBuilder {
    #[allow(unused_mut)]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
}

/// A private directory removed on drop; exclusive creation retries on a stale name.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bake-conformance-unit-{}-{name}-{n}",
                std::process::id()
            ));
            match private_dir_builder().create(&path) {
                Ok(()) => return TempRoot(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create temp root: {error}"),
            }
        }
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn input(permissions: &str, writes: &str) -> String {
    format!(
        r#"{{"schema":"{INPUT_SCHEMA}","version":1,"prompts":[],"events":[],"permissions":{permissions},"writes":{writes}}}"#
    )
}

fn with_events(events: &str) -> String {
    format!(
        r#"{{"schema":"{INPUT_SCHEMA}","version":1,"prompts":[],"events":{events},"permissions":[],"writes":[]}}"#
    )
}

fn rejects(text: &str) -> String {
    match parse(text.as_bytes()) {
        Err(Failure::Invalid(message)) => message,
        other => panic!("expected invalid input for {text}, got {other:?}"),
    }
}

#[test]
fn fixture_inputs_relay_their_expected_vectors() {
    for (name, bytes) in json_files("fixtures") {
        let fixture: Value = serde_json::from_slice(&bytes).unwrap();
        let input_bytes = serde_json::to_vec(&fixture["input"]).unwrap();
        let input = parse(&input_bytes).unwrap_or_else(|f| panic!("{name}: {f}"));
        let observed: Value = serde_json::from_str(&observe(&input)).unwrap();
        let expected = &fixture["expected"];
        assert_eq!(observed["schema"], OBSERVATION_SCHEMA, "{name}");
        assert_eq!(observed["version"], 1, "{name}");
        for key in ["prompts", "events", "permissions"] {
            assert_eq!(observed[key], expected[key], "{name}: {key}");
        }
        assert_eq!(observed.as_object().unwrap().len(), 5, "{name}");
    }
}

#[test]
fn shared_invalid_inputs_are_rejected() {
    for (name, bytes) in json_files("invalid") {
        assert!(
            matches!(parse(&bytes), Err(Failure::Invalid(_))),
            "{name} was accepted"
        );
    }
}

#[test]
fn portable_paths_are_accepted() {
    for path in [
        "result.txt",
        "src/nested/result.txt",
        "a",
        ".hidden",
        "con-notes.txt",
        "COM10",
        "lpt",
        "กา/👩‍💻.txt",
    ] {
        assert_eq!(path_problem(path), None, "{path}");
    }
}

#[test]
fn non_portable_paths_are_rejected() {
    let long = "a".repeat(MAX_PATH_BYTES + 1);
    for path in [
        "",
        long.as_str(),
        "/abs",
        "C:/x",
        "c:x",
        "//server/share",
        "\\\\server\\share",
        "a\\b",
        "a//b",
        "a/",
        "./a",
        "a/./b",
        "../a",
        "a/..",
        "a\0b",
        "a\u{1f}b",
        "a\u{7f}b",
        "a?b",
        "a*b",
        "a<b",
        "a|b",
        "a\"b",
        "trail.",
        "trail ",
        "dir./x",
        "CON",
        "con.txt",
        "nul .txt",
        "Aux",
        "PRN.tar.gz",
        "com1",
        "LPT9.log",
        "com0",
        "COM¹",
        "conin$",
        "x/CONOUT$.y",
    ] {
        assert!(path_problem(path).is_some(), "{path:?} was accepted");
    }
    assert_eq!(path_problem(&"a".repeat(MAX_PATH_BYTES)), None);
}

#[test]
fn event_numbers_must_be_safe_integers() {
    parse(with_events(r#"[{"n":9007199254740991,"m":-9007199254740991}]"#).as_bytes()).unwrap();
    for number in [
        "9007199254740992",
        "-9007199254740992",
        "1.0",
        "1e2",
        "-0",
        "18446744073709551616",
    ] {
        rejects(&with_events(&format!(r#"[{{"data":{{"n":[{number}]}}}}]"#)));
    }
}

#[test]
fn events_keep_every_field_and_reject_duplicates() {
    let text = with_events(r#"[{"z":null,"a":[true,"x",{"k":1}],"type":"t"}]"#);
    let observed: Value = serde_json::from_str(&observe(&parse(text.as_bytes()).unwrap())).unwrap();
    assert_eq!(
        observed["events"],
        serde_json::json!([{"type":"t","a":[true,"x",{"k":1}],"z":null}])
    );
    assert!(rejects(&with_events(r#"[{"a":1,"a":1}]"#)).contains("duplicate event key"));
    assert!(rejects(&with_events(r#"[{"d":{"a":1,"a":2}}]"#)).contains("duplicate event key"));
    rejects(&with_events(r#"["not an object"]"#));
}

#[test]
fn structured_records_reject_unknown_missing_and_duplicate_fields() {
    rejects(&input(
        r#"[{"id":"p","path":"a","decision":"allow","extra":1}]"#,
        "[]",
    ));
    rejects(&input(r#"[{"id":"p","path":"a"}]"#, "[]"));
    rejects(&input(r#"[{"id":"p","path":"a","decision":"ask"}]"#, "[]"));
    rejects(&input(
        "[]",
        r#"[{"path":"a","text":"","permission":"p","mode":1}]"#,
    ));
    rejects(&format!(
        r#"{{"schema":"{INPUT_SCHEMA}","schema":"{INPUT_SCHEMA}","version":1,"prompts":[],"events":[],"permissions":[],"writes":[]}}"#
    ));
    rejects(
        r#"{"schema":"other","version":1,"prompts":[],"events":[],"permissions":[],"writes":[]}"#,
    );
    rejects(&format!(
        r#"{{"schema":"{INPUT_SCHEMA}","version":1.0,"prompts":[],"events":[],"permissions":[],"writes":[]}}"#
    ));
}

#[test]
fn malformed_json_is_invalid() {
    for text in [
        "",
        " ",
        "{",
        "[]",
        "null",
        "\u{feff}{}",
        &format!("{} {{}}", input("[]", "[]")),
    ] {
        rejects(text);
    }
    let mut raw = input("[]", "[]").into_bytes();
    let at = raw.iter().position(|&b| b == b'[').unwrap();
    raw.splice(at..at + 2, *br#"["\xff"]"#);
    assert!(matches!(parse(&raw), Err(Failure::Invalid(_))));
}

#[test]
fn lone_surrogates_are_rejected_everywhere() {
    for events in [
        r#"[{"t":"\udc00"}]"#,
        r#"[{"\ud800":1}]"#,
        r#"[{"t":"\ud800x"}]"#,
    ] {
        rejects(&with_events(events));
    }
    rejects(&input(
        r#"[{"id":"\udfff","path":"a","decision":"allow"}]"#,
        "[]",
    ));
    let pair = with_events(r#"[{"t":"\ud83d\udc69"}]"#);
    let observed = observe(&parse(pair.as_bytes()).unwrap());
    assert!(observed.contains("\"t\":\"\u{1f469}\""), "{observed}");
}

#[test]
fn bounds_are_enforced() {
    let prompts = |n: usize| {
        let list = vec!["\"\""; n].join(",");
        format!(
            r#"{{"schema":"{INPUT_SCHEMA}","version":1,"prompts":[{list}],"events":[],"permissions":[],"writes":[]}}"#
        )
    };
    parse(prompts(MAX_ITEMS).as_bytes()).unwrap();
    assert!(rejects(&prompts(MAX_ITEMS + 1)).contains("more than 64"));

    let text = |n: usize| {
        input(
            r#"[{"id":"p","path":"a","decision":"allow"}]"#,
            &format!(
                r#"[{{"path":"a","text":"{}","permission":"p"}}]"#,
                "é".repeat(n / 2)
            ),
        )
    };
    parse(text(MAX_STRING_BYTES).as_bytes()).unwrap();
    assert!(rejects(&text(MAX_STRING_BYTES + 2)).contains("exceeds 65536 bytes"));
    let key = "k".repeat(MAX_STRING_BYTES + 1);
    rejects(&with_events(&format!(r#"[{{"{key}":1}}]"#)));

    let oversized = vec![b' '; MAX_INPUT_BYTES + 1];
    assert_eq!(
        rejects(std::str::from_utf8(&oversized).unwrap()),
        format!("input exceeds {MAX_INPUT_BYTES} bytes")
    );
    let root = TempRoot::new("oversized");
    let error = run(oversized.as_slice(), Vec::new(), &root.0).unwrap_err();
    assert_eq!(
        error,
        Failure::Invalid(format!("input exceeds {MAX_INPUT_BYTES} bytes"))
    );
}

#[test]
fn permissions_and_writes_must_agree() {
    let allow = r#"[{"id":"p","path":"a.txt","decision":"allow"}]"#;
    let write = |path: &str, permission: &str| {
        format!(r#"{{"path":"{path}","text":"","permission":"{permission}"}}"#)
    };
    rejects(&input(r#"[{"id":"","path":"a","decision":"allow"}]"#, "[]"));
    rejects(&input("[]", &format!("[{}]", write("a.txt", "missing"))));
    assert!(rejects(&input(allow, &format!("[{}]", write("b.txt", "p")))).contains("differs"));
}

#[test]
fn repeated_writes_to_one_path_apply_in_order() {
    let root = TempRoot::new("repeated");
    let text = input(
        r#"[{"id":"p","path":"a/same.txt","decision":"allow"},{"id":"d","path":"a/same.txt","decision":"deny"}]"#,
        r#"[{"path":"a/same.txt","text":"first","permission":"p"},{"path":"a/same.txt","text":"second","permission":"p"},{"path":"a/same.txt","text":"denied","permission":"d"}]"#,
    );
    run(text.as_bytes(), Vec::new(), &root.0).unwrap();
    assert_eq!(fs::read(root.0.join("a/same.txt")).unwrap(), b"second");
}

#[test]
fn a_later_write_failure_keeps_earlier_writes() {
    let root = TempRoot::new("partial");
    let text = input(
        r#"[{"id":"f","path":"a","decision":"allow"},{"id":"c","path":"a/b","decision":"allow"}]"#,
        r#"[{"path":"a","text":"file","permission":"f"},{"path":"a/b","text":"x","permission":"c"}]"#,
    );
    let mut out = Vec::new();
    let error = run(text.as_bytes(), &mut out, &root.0).unwrap_err();
    assert!(
        matches!(error, Failure::Io(ref m) if m.contains("parent is not a directory")),
        "{error}"
    );
    assert!(out.is_empty());
    assert_eq!(fs::read(root.0.join("a")).unwrap(), b"file");
}

/// An event whose innermost container sits at `depth`, alternating objects and arrays.
fn nested_event(depth: usize) -> String {
    let mut value = String::from("1");
    for level in (2..=depth).rev() {
        value = if level % 2 == 0 {
            format!("[{value}]")
        } else {
            format!(r#"{{"k":{value}}}"#)
        };
    }
    with_events(&format!(r#"[{{"d":{value}}}]"#))
}

#[test]
fn event_nesting_is_bounded() {
    let deepest = nested_event(MAX_EVENT_DEPTH);
    // The document object, the events array, and the empty prompts,
    // permissions, and writes arrays add five brackets outside the event.
    assert_eq!(deepest.matches(['[', '{']).count(), MAX_EVENT_DEPTH + 5);
    parse(deepest.as_bytes()).unwrap();
    parse(nested_event(MAX_EVENT_DEPTH + 1).as_bytes()).unwrap_err();
    assert!(rejects(&nested_event(MAX_EVENT_DEPTH + 1)).contains("nests more than 32 containers"));
    rejects(&nested_event(200));

    let root = TempRoot::new("deep");
    let deep = nested_event(MAX_EVENT_DEPTH + 1).replace(
        r#""permissions":[],"writes":[]"#,
        r#""permissions":[{"id":"p","path":"a.txt","decision":"allow"}],"writes":[{"path":"a.txt","text":"x","permission":"p"}]"#,
    );
    assert!(deep.contains("a.txt"));
    let mut out = Vec::new();
    assert!(matches!(
        run(deep.as_bytes(), &mut out, &root.0),
        Err(Failure::Invalid(_))
    ));
    assert!(out.is_empty());
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
}

#[test]
fn invalid_input_writes_nothing_even_after_a_valid_allowed_write() {
    let root = TempRoot::new("validate-first");
    let text = input(
        r#"[{"id":"p","path":"first.txt","decision":"allow"}]"#,
        r#"[{"path":"first.txt","text":"x","permission":"p"},{"path":"first.txt","text":"y","permission":"missing"}]"#,
    );
    let mut out = Vec::new();
    assert!(matches!(
        run(text.as_bytes(), &mut out, &root.0),
        Err(Failure::Invalid(_))
    ));
    assert!(out.is_empty());
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
}

#[test]
fn allowed_writes_create_parents_and_denied_writes_are_skipped() {
    let root = TempRoot::new("execute");
    let text = input(
        r#"[{"id":"p","path":"a/b/c.txt","decision":"allow"},{"id":"d","path":"no.txt","decision":"deny"}]"#,
        r#"[{"path":"a/b/c.txt","text":"ok\r\n","permission":"p"},{"path":"no.txt","text":"x","permission":"d"}]"#,
    );
    let mut out = Vec::new();
    run(text.as_bytes(), &mut out, &root.0).unwrap();
    assert_eq!(fs::read(root.0.join("a/b/c.txt")).unwrap(), b"ok\r\n");
    assert!(!root.0.join("no.txt").exists());
    let line = String::from_utf8(out).unwrap();
    assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
}

#[test]
fn write_failure_is_an_io_failure_without_output() {
    let root = TempRoot::new("io-failure");
    fs::create_dir(root.0.join("taken")).unwrap();
    let text = input(
        r#"[{"id":"p","path":"taken","decision":"allow"}]"#,
        r#"[{"path":"taken","text":"x","permission":"p"}]"#,
    );
    let mut out = Vec::new();
    let error = run(text.as_bytes(), &mut out, &root.0).unwrap_err();
    assert_eq!(error.exit_code(), 1);
    assert!(out.is_empty());
}

#[cfg(unix)]
#[test]
fn writes_refuse_to_cross_symlinks() {
    let root = TempRoot::new("symlink");
    let outside = TempRoot::new("symlink-target");
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&root.0).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o700);
    std::os::unix::fs::symlink(&outside.0, root.0.join("link")).unwrap();
    let text = input(
        r#"[{"id":"p","path":"link/escape.txt","decision":"allow"}]"#,
        r#"[{"path":"link/escape.txt","text":"x","permission":"p"}]"#,
    );
    let error = run(text.as_bytes(), Vec::new(), &root.0).unwrap_err();
    assert!(
        matches!(error, Failure::Io(ref m) if m.contains("symbolic link")),
        "{error}"
    );
    assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 0);
}
