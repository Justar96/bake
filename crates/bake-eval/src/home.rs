//! A sample's Bake home: the provider account and nothing else.
//!
//! The user's Bake home is only read. From it a sample's home gets the
//! provider's record of `auth.json`, the `cliproxyapi` member of
//! `settings.json` when that is the provider (the signed-in route and its
//! model list), and `models.json` when there is one. No other setting comes
//! along, so the user's default tools, hooks, compaction, or team settings
//! cannot differ between hosts or change a result. An arm's own settings,
//! given to the run, are the only others a sample has.

use std::fs;
use std::io::Write;
use std::path::Path;

use serde_json::{Map, Value};

/// The provider whose route lives in `settings.json`.
const ROUTE_PROVIDER: &str = "cliproxyapi";

fn read_object(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(object)) => Ok(Some(object)),
        Ok(_) => Err(format!("{} is not a JSON object", path.display())),
        Err(error) => Err(format!("{} is not valid JSON: {error}", path.display())),
    }
}

/// Writes `text` to a new file only its owner can read.
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Reads an arm's settings: a JSON object of `settings.json` members. The
/// route's member is refused, since the account comes from the source home.
pub fn read_arm_settings(path: &Path) -> Result<Map<String, Value>, String> {
    let settings = read_object(path)?
        .ok_or_else(|| format!("cannot read {}: no such file", path.display()))?;
    if settings.contains_key(ROUTE_PROVIDER) {
        return Err(format!(
            "{}: an arm's settings cannot set {ROUTE_PROVIDER}",
            path.display()
        ));
    }
    Ok(settings)
}

/// Builds the Bake home `home` (which must not exist) for `provider` from
/// the account in `source`, with the arm's own `settings`. Fails when
/// `source` holds neither a key nor a `models.json` for the provider, since
/// the sample could reach no model.
pub fn prepare(
    source: &Path,
    provider: &str,
    home: &Path,
    arm_settings: &Map<String, Value>,
) -> Result<(), String> {
    let auth = read_object(&source.join("auth.json"))?.unwrap_or_default();
    let settings = read_object(&source.join("settings.json"))?.unwrap_or_default();
    let models = source.join("models.json");
    let record = auth.get(provider);
    if record.is_none() && !models.is_file() {
        return Err(format!(
            "{} has no key for the provider {provider} and no models.json",
            source.display()
        ));
    }
    fs::create_dir_all(home)
        .map_err(|error| format!("cannot create {}: {error}", home.display()))?;
    let mut kept_auth = Map::new();
    if let Some(record) = record {
        kept_auth.insert(provider.to_owned(), record.clone());
    }
    write_private(
        &home.join("auth.json"),
        &Value::Object(kept_auth).to_string(),
    )?;
    let mut kept_settings = arm_settings.clone();
    kept_settings.remove(ROUTE_PROVIDER);
    if provider == ROUTE_PROVIDER
        && let Some(route) = settings.get(ROUTE_PROVIDER)
    {
        kept_settings.insert(ROUTE_PROVIDER.to_owned(), route.clone());
    }
    write_private(
        &home.join("settings.json"),
        &Value::Object(kept_settings).to_string(),
    )?;
    if models.is_file() {
        let text = fs::read_to_string(&models)
            .map_err(|error| format!("cannot read {}: {error}", models.display()))?;
        write_private(&home.join("models.json"), &text)?;
    }
    Ok(())
}

/// Removes the account files [`prepare`] wrote, so a kept sample directory
/// holds no key.
pub fn remove_account(home: &Path) {
    for name in ["auth.json", "settings.json", "models.json"] {
        let _ = fs::remove_file(home.join(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bake-eval-home-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).expect("read")).expect("json")
    }

    #[test]
    fn only_the_account_is_copied() {
        let dir = scratch("account");
        let source = dir.join("source");
        fs::create_dir_all(&source).expect("source");
        fs::write(
            source.join("auth.json"),
            r#"{"cliproxyapi":{"type":"api_key","key":"k1","account":"t"},"other":{"type":"api_key","key":"k2"}}"#,
        )
        .expect("auth");
        fs::write(
            source.join("settings.json"),
            r#"{"cliproxyapi":{"baseURL":"http://127.0.0.1:1","account":"t"},"telecom":true,"defaultTools":["+grep"],"hooks":{"codex":"x"}}"#,
        )
        .expect("settings");
        let home = dir.join("home");
        prepare(&source, "cliproxyapi", &home, &Map::new()).expect("prepare");
        assert_eq!(
            json(&home.join("auth.json")),
            serde_json::json!({"cliproxyapi":{"type":"api_key","key":"k1","account":"t"}})
        );
        assert_eq!(
            json(&home.join("settings.json")),
            serde_json::json!({"cliproxyapi":{"baseURL":"http://127.0.0.1:1","account":"t"}})
        );
        assert!(!home.join("models.json").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(home.join("auth.json"))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // The source is untouched.
        assert!(json(&source.join("settings.json")).get("telecom").is_some());
        remove_account(&home);
        assert!(!home.join("auth.json").exists() && !home.join("settings.json").exists());
        fs::remove_dir_all(&dir).expect("clean");
    }

    #[test]
    fn an_arm_s_settings_join_the_route_and_cannot_replace_it() {
        let dir = scratch("arm");
        let source = dir.join("source");
        fs::create_dir_all(&source).expect("source");
        fs::write(source.join("auth.json"), r#"{"cliproxyapi":{"key":"k"}}"#).expect("auth");
        fs::write(
            source.join("settings.json"),
            r#"{"cliproxyapi":{"account":"t"},"telecom":true}"#,
        )
        .expect("settings");
        let arm = dir.join("arm.json");
        fs::write(
            &arm,
            r#"{"nestedContextFiles":true,"skills":{"listingTokens":900}}"#,
        )
        .expect("arm");
        let mut settings = read_arm_settings(&arm).expect("settings");
        // Even a route smuggled past the reader does not reach the home.
        settings.insert("cliproxyapi".into(), serde_json::json!({"account":"other"}));
        let home = dir.join("home");
        prepare(&source, "cliproxyapi", &home, &settings).expect("prepare");
        assert_eq!(
            json(&home.join("settings.json")),
            serde_json::json!({
                "nestedContextFiles": true,
                "skills": {"listingTokens": 900},
                "cliproxyapi": {"account": "t"}
            })
        );
        fs::write(&arm, r#"{"cliproxyapi":{"account":"other"}}"#).expect("arm");
        let error = read_arm_settings(&arm).unwrap_err();
        assert!(
            error.ends_with("an arm's settings cannot set cliproxyapi"),
            "{error}"
        );
        assert!(read_arm_settings(&dir.join("none.json")).is_err());
        fs::remove_dir_all(&dir).expect("clean");
    }

    #[test]
    fn another_provider_takes_models_json_and_no_route() {
        let dir = scratch("models");
        let source = dir.join("source");
        fs::create_dir_all(&source).expect("source");
        fs::write(
            source.join("auth.json"),
            r#"{"gw":{"type":"api_key","key":"k"}}"#,
        )
        .expect("auth");
        fs::write(
            source.join("settings.json"),
            r#"{"cliproxyapi":{"account":"t"}}"#,
        )
        .expect("settings");
        fs::write(
            source.join("models.json"),
            "{ \"providers\": {} } // kept as written\n",
        )
        .expect("models");
        let home = dir.join("home");
        prepare(&source, "gw", &home, &Map::new()).expect("prepare");
        assert_eq!(json(&home.join("settings.json")), serde_json::json!({}));
        assert_eq!(
            fs::read_to_string(home.join("models.json")).expect("models"),
            "{ \"providers\": {} } // kept as written\n"
        );
        fs::remove_dir_all(&dir).expect("clean");
    }

    #[test]
    fn a_missing_account_or_broken_file_is_an_error() {
        let dir = scratch("missing");
        let source = dir.join("source");
        fs::create_dir_all(&source).expect("source");
        let error = prepare(&source, "cliproxyapi", &dir.join("home"), &Map::new()).unwrap_err();
        assert!(
            error.ends_with("has no key for the provider cliproxyapi and no models.json"),
            "{error}"
        );
        assert!(!dir.join("home").exists());
        fs::write(source.join("auth.json"), "[]").expect("auth");
        let error = prepare(&source, "cliproxyapi", &dir.join("home"), &Map::new()).unwrap_err();
        assert!(error.ends_with("auth.json is not a JSON object"), "{error}");
        fs::remove_dir_all(&dir).expect("clean");
    }
}
