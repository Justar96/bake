//! The D25 import from temporary homes built from fake values only.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::cliproxyapi::yaml::YamlErrorKind;

const FAKE_KEY: &str = "sk-fake-0123456789";

/// A temporary Bake home, removed on drop.
struct TempHome(PathBuf);

impl TempHome {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "bake-cliproxy-{name}-{}-{:x}",
            std::process::id(),
            bake_ai::now_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary home");
        Self(dir)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let file = self.0.join(name);
        std::fs::write(&file, text).expect("a fixture file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
                .expect("owner-only permissions");
        }
        file
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn no_env(_: &str) -> Option<OsString> {
    None
}

/// The settings shape of a 0.3 home after `/login cliproxyapi`, with fake
/// values.
const SETTINGS: &str = "\
llm-pi-ai:
  providers:
    cliproxyapi:
      displayName: CLIProxyAPI
      apiKeyEnv: CLIPROXYAPI_API_KEY
      api: openai-responses
      baseURL: https://proxy.example.test/v1
      models:
        - id: gpt-fake-6
          name: GPT Fake 6
          contextWindow: 272000
          maxTokens: 128000
          input: [text, image]
          reasoningEfforts: {low: low, medium: medium, high: high, xhigh: xhigh, max: max}
        - id: claude-fake-5
          api: anthropic-messages
          baseURL: https://proxy.example.test
          name: Claude Fake 5
          reasoningEfforts: {low: low, high: high, max: max}
          compat: {forceAdaptiveThinking: true}
      retryPolicy: {mode: normal, backoff: {maxDelayMs: 60000}}
      compat: {sendSessionAffinityHeaders: true}
agent-default-model: {provider: cliproxyapi, model: claude-fake-5, reasoningEffort: high}
";

fn credentials(key: &str) -> String {
    format!("version: 1\nrefs:\n  CLIPROXYAPI_API_KEY: {key}\n")
}

fn modified(file: &Path) -> (Vec<u8>, std::time::SystemTime) {
    let bytes = std::fs::read(file).expect("the file");
    let time = std::fs::metadata(file)
        .and_then(|metadata| metadata.modified())
        .expect("its modification time");
    (bytes, time)
}

// D25 and pi-first-plan "The ~/.bake home": the route, the key from
// `.credentials.yaml`, and the default model, read without writing either
// file.
#[test]
fn imports_the_route_key_and_default_model_read_only() {
    let home = TempHome::new("import");
    let settings = home.write(SETTINGS_FILENAME, SETTINGS);
    let creds = home.write(CREDENTIALS_FILENAME, &credentials(FAKE_KEY));
    let before = (modified(&settings), modified(&creds));
    let import = import_cliproxyapi(&home.0, &no_env).expect("an import");
    assert_eq!((modified(&settings), modified(&creds)), before);

    assert_eq!(
        import.default_model,
        Some(DefaultModel {
            provider: "cliproxyapi".into(),
            model: "claude-fake-5".into(),
            reasoning_effort: Some("high".into()),
            thinking_level: Some(ModelThinkingLevel::High),
        })
    );
    let route = import.route.expect("a route");
    assert_eq!(route.upgraded, []);
    assert_eq!(
        route.key.as_ref().map(|key| key.source),
        Some(KeySource::CredentialsFile)
    );
    assert_eq!(
        route.key.as_ref().map(|key| key.key.expose()),
        Some(FAKE_KEY)
    );
    assert_eq!(route.provider.skipped, []);
    assert_eq!(
        route.provider.config,
        json!({
            "name": "CLIProxyAPI", "baseUrl": "https://proxy.example.test/v1", "api": "openai-responses",
            "apiKey": "$CLIPROXYAPI_API_KEY", "compat": { "sendSessionAffinityHeaders": true },
            "models": [
                { "id": "gpt-fake-6", "name": "GPT Fake 6", "reasoning": true,
                  "thinkingLevelMap": { "off": null, "minimal": null, "low": "low", "medium": "medium",
                                        "high": "high", "xhigh": "xhigh", "max": "max" },
                  "input": ["text", "image"], "contextWindow": 272_000, "maxTokens": 128_000 },
                { "id": "claude-fake-5", "name": "Claude Fake 5", "api": "anthropic-messages",
                  "baseUrl": "https://proxy.example.test", "reasoning": true,
                  "thinkingLevelMap": { "off": null, "minimal": null, "low": "low", "medium": null,
                                        "high": "high", "xhigh": null, "max": "max" },
                  "input": ["text"], "contextWindow": 262_144, "maxTokens": 32_768,
                  "compat": { "forceAdaptiveThinking": true } },
            ],
        })
    );
}

// LocalCredentialProvider.resolve: a non-empty inherited variable wins over
// the file, and an empty one does not count.
#[test]
fn takes_the_environment_key_before_the_file() {
    let home = TempHome::new("env");
    home.write(SETTINGS_FILENAME, SETTINGS);
    home.write(CREDENTIALS_FILENAME, &credentials(FAKE_KEY));
    let env: HashMap<&str, &str> = HashMap::from([("CLIPROXYAPI_API_KEY", "  sk-env-key  ")]);
    let lookup = |name: &str| env.get(name).map(OsString::from);
    let key = import_cliproxyapi(&home.0, &lookup)
        .expect("an import")
        .route
        .and_then(|route| route.key);
    assert_eq!(
        key.as_ref().map(|key| key.source),
        Some(KeySource::Environment)
    );
    // `normalizeApiKey` trims.
    assert_eq!(key.as_ref().map(|key| key.key.expose()), Some("sk-env-key"));

    let empty = |name: &str| (name == "CLIPROXYAPI_API_KEY").then(OsString::new);
    let key = import_cliproxyapi(&home.0, &empty)
        .expect("an import")
        .route
        .and_then(|route| route.key);
    assert_eq!(key.map(|key| key.source), Some(KeySource::CredentialsFile));
}

#[test]
fn imports_what_exists_and_nothing_else() {
    let home = TempHome::new("partial");
    assert_eq!(
        import_cliproxyapi(&home.0, &no_env),
        Ok(CliProxyImport::default())
    );
    home.write(SETTINGS_FILENAME, "# nothing yet\n");
    assert_eq!(
        import_cliproxyapi(&home.0, &no_env),
        Ok(CliProxyImport::default())
    );
    // A default model alone; `selection` needs both halves.
    home.write(
        SETTINGS_FILENAME,
        "agent-default-model: {provider: cliproxyapi, model: m, reasoningEffort: turbo}\n",
    );
    let import = import_cliproxyapi(&home.0, &no_env).expect("an import");
    assert_eq!(import.route, None);
    assert_eq!(
        import
            .default_model
            .map(|model| (model.reasoning_effort, model.thinking_level)),
        Some((Some("turbo".into()), None))
    );
    home.write(
        SETTINGS_FILENAME,
        "agent-default-model: {provider: cliproxyapi, model: ''}\n",
    );
    assert_eq!(
        import_cliproxyapi(&home.0, &no_env).map(|import| import.default_model),
        Ok(None)
    );
    // A route with no key anywhere imports without one.
    home.write(SETTINGS_FILENAME, SETTINGS);
    let route = import_cliproxyapi(&home.0, &no_env)
        .expect("an import")
        .route;
    assert_eq!(route.map(|route| route.key), Some(None));
}

// cliproxyapi.test.ts: "upgrading a route an earlier login wrote", as
// startup's `upgradeCliProxyRoute` would leave the route, applied in memory
// with the file left as it was.
#[test]
fn upgrades_an_earlier_logins_route_in_memory() {
    let home = TempHome::new("legacy");
    let settings = home.write(
        SETTINGS_FILENAME,
        "llm-pi-ai:\n  providers:\n    cliproxyapi:\n      apiKeyEnv: CLIPROXYAPI_API_KEY\n      api: openai-responses\n      baseURL: https://proxy.example.test/v1\n      models:\n        - {id: gpt-test, name: GPT}\n        - {id: claude-test, name: Claude, reasoningEfforts: {high: high, max: max}}\n        - {id: glm-test, name: GLM}\n",
    );
    let before = modified(&settings);
    let route = import_cliproxyapi(&home.0, &no_env)
        .expect("an import")
        .route
        .expect("a route");
    assert_eq!(modified(&settings), before);
    assert_eq!(
        route.upgraded,
        [
            CliProxyRouteChange::Protocols,
            CliProxyRouteChange::AdaptiveThinking,
            CliProxyRouteChange::Retry,
            CliProxyRouteChange::Affinity,
        ]
    );
    let models = &route.provider.config["models"];
    assert_eq!(models[1]["api"], json!("anthropic-messages"));
    assert_eq!(models[1]["baseUrl"], json!("https://proxy.example.test"));
    assert_eq!(
        models[1]["compat"],
        json!({ "forceAdaptiveThinking": true })
    );
    assert_eq!(models[2]["api"], json!("openai-completions"));
    assert_eq!(
        route.provider.config["compat"],
        json!({ "sendSessionAffinityHeaders": true })
    );
}

/// The rendered error and its `Debug` form, which must never hold a key.
fn shown(error: &ImportError) -> String {
    format!("{error} | {error:?}")
}

// Hostile and malformed files: typed errors, bounded cost, no panic.
#[test]
fn refuses_hostile_settings_with_a_typed_error() {
    let home = TempHome::new("hostile");
    let mut laughs = String::from("a: &a [lol, lol, lol, lol, lol, lol, lol, lol, lol, lol]\n");
    for (previous, name) in ["a", "b", "c", "d", "e", "f", "g", "h"]
        .iter()
        .zip(["b", "c", "d", "e", "f", "g", "h", "i"])
    {
        laughs.push_str(&format!(
            "{name}: &{name} [{}]\n",
            vec![format!("*{previous}"); 10].join(", ")
        ));
    }
    laughs.push_str("llm-pi-ai: {providers: {cliproxyapi: *i}}\n");
    home.write(SETTINGS_FILENAME, &laughs);
    let started = std::time::Instant::now();
    let error = import_cliproxyapi(&home.0, &no_env).expect_err("refused");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(
        matches!(&error, ImportError::Yaml { error, .. } if error.kind == YamlErrorKind::Limit),
        "{error:?}"
    );

    home.write(SETTINGS_FILENAME, &"#".repeat(MAX_FILE_BYTES + 1));
    assert!(matches!(
        import_cliproxyapi(&home.0, &no_env),
        Err(ImportError::TooLarge { .. })
    ));

    let nested = format!("llm-pi-ai: {}{}\n", "{a: ".repeat(500), "}".repeat(500));
    home.write(SETTINGS_FILENAME, &nested);
    assert!(matches!(
        import_cliproxyapi(&home.0, &no_env),
        Err(ImportError::Yaml { .. })
    ));

    home.write(SETTINGS_FILENAME, "- a list\n");
    assert_eq!(
        import_cliproxyapi(&home.0, &no_env).map_err(|error| match error {
            ImportError::Invalid { path, .. } => path,
            other => format!("{other:?}"),
        }),
        Err("(root)".to_owned())
    );

    home.write(SETTINGS_FILENAME, "llm-pi-ai:\n  providers:\n    cliproxyapi:\n      baseURL: http://h/v1\n      models: [{id: a, contextWindow: -1}]\n");
    let error = import_cliproxyapi(&home.0, &no_env).expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{}: llm-pi-ai.providers.cliproxyapi.models[0].contextWindow must be a positive integer",
            home.0.join(SETTINGS_FILENAME).display()
        )
    );
}

// credentials-local's `parseCredentialsDocument`, `parseRefs`, and the flat
// layout; `normalizeApiKey`'s refusals; and no key in any error.
#[test]
fn reads_the_credentials_file_as_credentials_local_does() {
    let home = TempHome::new("credentials");
    home.write(SETTINGS_FILENAME, SETTINGS);
    let key = |text: &str| {
        home.write(CREDENTIALS_FILENAME, text);
        import_cliproxyapi(&home.0, &no_env).map(|import| {
            import
                .route
                .and_then(|route| route.key)
                .map(|key| key.key.expose().to_owned())
        })
    };
    // The pre-release flat layout, read as its migration would leave it.
    assert_eq!(
        key(&format!("CLIPROXYAPI_API_KEY: {FAKE_KEY}\n")),
        Ok(Some(FAKE_KEY.into()))
    );
    assert_eq!(key(""), Ok(None));
    assert_eq!(
        key("version: 1.0\nrefs:\n  OTHER_KEY: x\nrecords: {}\n"),
        Ok(None)
    );
    for (text, path) in [
        ("refs:\n  CLIPROXYAPI_API_KEY: [1]\n".to_owned(), "version"),
        (
            format!("version: 2\nrefs: {{CLIPROXYAPI_API_KEY: {FAKE_KEY}}}\n"),
            "version",
        ),
        (format!("version: 1\nsecrets: {FAKE_KEY}\n"), "secrets"),
        ("version: 1\nrefs: [a]\n".to_owned(), "refs"),
        ("version: 1\nrecords: [a]\n".to_owned(), "records"),
        (
            "version: 1\nrefs: {CLIPROXYAPI_API_KEY: ''}\n".to_owned(),
            "refs.CLIPROXYAPI_API_KEY",
        ),
        (
            "version: 1\nrefs: {CLIPROXYAPI_API_KEY: 42}\n".to_owned(),
            "refs.CLIPROXYAPI_API_KEY",
        ),
        (
            format!("version: 1\nrefs: {{'not-a-name': {FAKE_KEY}}}\n"),
            "refs",
        ),
    ] {
        match key(&text) {
            Err(ImportError::Invalid { path: found, .. }) => assert_eq!(found, path, "{text}"),
            other => panic!("{text}: expected an invalid document, got {other:?}"),
        }
    }
    for (value, reason) in [
        ("'   '", KeyRejection::Empty),
        ("'sk-has space'", KeyRejection::IllegalCharacters),
        ("\"sk-\\u00e9\"", KeyRejection::IllegalCharacters),
    ] {
        assert_eq!(
            key(&format!(
                "version: 1\nrefs: {{CLIPROXYAPI_API_KEY: {value}}}\n"
            )),
            Err(ImportError::UnusableKey {
                name: CLIPROXYAPI_KEY_NAME.into(),
                reason
            })
        );
    }
    // Malformed YAML around a key: the error names a position, not the key.
    for text in [
        format!("version: 1\nrefs: {{CLIPROXYAPI_API_KEY: {FAKE_KEY}\n"),
        format!(
            "version: 1\nrefs:\n  CLIPROXYAPI_API_KEY: {FAKE_KEY}\n  CLIPROXYAPI_API_KEY: {FAKE_KEY}\n"
        ),
        format!("version: 1\nrefs:\n  CLIPROXYAPI_API_KEY: \"{FAKE_KEY}\n"),
        format!("version: 2\nrefs: {{CLIPROXYAPI_API_KEY: {FAKE_KEY}}}\n"),
        format!("version: 1\nrefs: {{CLIPROXYAPI_API_KEY: '  {FAKE_KEY} x'}}\n"),
    ] {
        let error = key(&text).expect_err("refused");
        assert!(!shown(&error).contains(FAKE_KEY), "{}", shown(&error));
    }
}

const CLIPROXYAPI_KEY_NAME: &str = crate::cliproxyapi::CLIPROXYAPI_KEY;

// credentials-local's `assertOwnerOnly`: a file other users can read is
// refused before it is read.
#[cfg(unix)]
#[test]
fn refuses_a_credentials_file_readable_beyond_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let home = TempHome::new("mode");
    home.write(SETTINGS_FILENAME, SETTINGS);
    let file = home.write(CREDENTIALS_FILENAME, &credentials(FAKE_KEY));
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let error = import_cliproxyapi(&home.0, &no_env).expect_err("refused");
    assert_eq!(
        error,
        ImportError::CredentialsMode {
            file: file.clone(),
            mode: 0o644
        }
    );
    assert!(!shown(&error).contains(FAKE_KEY));
}

// No `Debug` of an import, a key, or an error shows the key.
#[test]
fn never_shows_the_key() {
    let home = TempHome::new("redact");
    home.write(SETTINGS_FILENAME, SETTINGS);
    home.write(CREDENTIALS_FILENAME, &credentials(FAKE_KEY));
    let import = import_cliproxyapi(&home.0, &no_env).expect("an import");
    let key = import
        .route
        .as_ref()
        .and_then(|route| route.key.as_ref())
        .expect("a key");
    assert_eq!(
        format!("{:?} {}", key.key, key.key),
        "ApiKey(<redacted>) <redacted>"
    );
    assert!(!format!("{import:?}").contains(FAKE_KEY));
    assert!(!format!("{import:#?}").contains(FAKE_KEY));
    assert!(
        !import
            .route
            .map(|route| route.provider.config.to_string())
            .unwrap_or_default()
            .contains(FAKE_KEY)
    );
}

// The live smoke test: the real Bake home, read-only, and its proxy's
// catalog. Prints the model count only, never the key or the URL. Run with
// `cargo test -p bake-coding-agent -- --ignored live_cliproxyapi`.
#[test]
#[ignore = "reads the real ~/.bake and needs the network"]
fn live_cliproxyapi_catalog_from_the_real_home() {
    let import = import_from_bake_home().expect("the real home imports");
    let route = import.route.expect("the home has a CLIProxyAPI route");
    let key = route.key.expect("the home has a CLIProxyAPI key");
    let endpoints = crate::cliproxyapi::cli_proxy_endpoints(&route.route.base_url)
        .expect("the saved base URL is a proxy URL");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let models = runtime
        .block_on(crate::cliproxyapi::fetch_cli_proxy_models(
            &endpoints.models,
            key.key.expose(),
            None,
            Some(&endpoints.root),
        ))
        // The failure's kind only: its message names the proxy's host.
        .map_err(|error| match error {
            crate::cliproxyapi::FetchCliProxyModelsError::Check(error) => {
                format!("{:?}", error.field())
            }
            other => other.to_string(),
        })
        .expect("the catalog");
    println!(
        "live CLIProxyAPI catalog: {} models; imported route lists {} ({} skipped); default model set: {}",
        models.len(),
        route.route.models.len(),
        route.provider.skipped.len(),
        import.default_model.is_some()
    );
    assert!(!models.is_empty());
}
