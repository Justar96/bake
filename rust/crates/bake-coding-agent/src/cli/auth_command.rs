//! `bake-rs auth check`, `auth print-api-key`, and `auth print-bearer-token`:
//! whether a provider is ready, and the credential an external client needs.
//!
//! Adapted from Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`)
//! `packages/coding-agent/src/cli/{auth-command,auth-check,credential-print}.ts`
//! and the `runAuthCommand` branch of `src/main.ts`, for the providers
//! `bake-rs` has: whatever `models.json` composes, the CLIProxyAPI account
//! and the route imported from Bake 0.3 (D8 and D25), and the credentials
//! `auth.json` holds ([`crate::auth_storage`]).
//!
//! The commands read files only: the Bake home's `models.json`,
//! `auth.json`, and `settings.json`. No network operation runs, as in Pi
//! (`allowModelNetwork: false`, `refreshOnCreate: false`), and nothing is
//! written, so `--no-refresh` is accepted and changes nothing: Pi's refresh
//! renews an expired OAuth credential, and `bake-rs` has no OAuth provider
//! to renew. For the same reason `print-bearer-token` has no bearer token
//! to print and fails with Pi's message, and `--min-expiry` is checked and
//! then unused.
//!
//! Deviation from Pi: with only `--model` given, Pi searches every provider
//! that has a stored credential with `resolveModelReference`, which is not
//! a ported function, so this search matches the reference exactly
//! ([`find_exact_model_reference_match`], Pi's
//! `findExactModelReferenceMatch`) instead of fuzzily. `--provider` with
//! `--model` resolves as a run does
//! ([`resolve_cli_model`](crate::model_registry::resolver::resolve_cli_model),
//! Pi's `resolveCliModel`), which is what Pi uses there too.
//!
//! A key reaches standard output only from `print-api-key`, and from
//! `auth check --credentials`, whose purpose is to print it. No error
//! message, status line, or `Debug` output carries one: [`AuthCommand`] has
//! no `Debug` (its arguments are the command line's own) and the resolved
//! credential is returned to the caller that prints it, never formatted
//! into a message.

use std::path::Path;

use crate::auth_storage::{AuthStorage, Credential};
use crate::model_registry::resolver::{find_exact_model_reference_match, resolve_cli_model};
use crate::model_registry::{ModelRegistry, auth_path};
use crate::print_mode::{SharedWriter, write_out};

use super::args::{Args, DiagnosticKind, parse_args, unknown_flags_message};

/// Pi's default `--min-expiry` for a bearer token: 30 minutes.
pub const DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS: u64 = 30 * 60_000;

/// Pi's `AuthCommandKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthCommandKind {
    /// `auth check`.
    Check,
    /// `auth print-api-key`.
    ApiKey,
    /// `auth print-bearer-token`.
    BearerToken,
}

/// Pi's `AuthCommand`. It has no `Debug`: its `args` are the command line's
/// own, which may name a key.
pub struct AuthCommand {
    /// Which command.
    pub kind: AuthCommandKind,
    /// The arguments after the command, for [`parse_args`].
    pub args: Vec<String>,
    /// `--json`, `auth check` only.
    pub json: bool,
    /// `--credentials`, `auth check` only.
    pub credentials: bool,
    /// `--no-refresh`, `auth check` only.
    pub no_refresh: bool,
    /// `--min-expiry`, `print-bearer-token` only, in milliseconds.
    pub min_expiry_ms: Option<u64>,
}

impl AuthCommandKind {
    /// Pi's `getAuthCommandName`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Check => "auth check",
            Self::ApiKey => "auth print-api-key",
            Self::BearerToken => "auth print-bearer-token",
        }
    }

    /// Pi's `getAuthCommandUsage`, with Bake's program name.
    pub fn usage(self) -> &'static str {
        match self {
            Self::Check => {
                "bake-rs auth check --provider <provider> [--json] [--credentials] [--no-refresh]"
            }
            Self::ApiKey => "bake-rs auth print-api-key --provider <provider> [--model <model>]",
            Self::BearerToken => {
                "bake-rs auth print-bearer-token --provider <provider> [--model <model>] [--min-expiry <duration>]"
            }
        }
    }
}

/// Pi's `isAuthCommandHelp`.
pub fn is_auth_command_help(args: &[String]) -> bool {
    args.first().is_some_and(|first| first == "auth")
        && (args.len() == 1
            || args[1] == "help"
            || args.iter().any(|arg| arg == "--help" || arg == "-h"))
}

/// Pi's `printAuthCommandHelp`, with Bake's program name and the providers
/// `bake-rs` has.
pub const AUTH_COMMAND_HELP: &str = "\
Usage:
  bake-rs auth print-api-key [--provider <provider>] [--model <model>]
  bake-rs auth print-bearer-token [--provider <provider>] [--model <model>] [--min-expiry <duration>]
  bake-rs auth check [--provider <provider>] [--model <model>] [--json] [--credentials] [--no-refresh]

Auth commands need at least one of --provider or --model. They read the Bake
home's models.json, auth.json, and settings.json, reach no network, and write
nothing, so --no-refresh changes nothing: bake-rs has no OAuth provider to
renew, which is also why print-bearer-token has no token to print.
--credentials prints the API key, or includes it in the JSON output.
";

/// Pi's `parseAuthCommand`: `None` when `args` is not an auth command, and
/// Pi's message when it is one that does not exist or is given a bad value.
pub fn parse_auth_command(args: &[String]) -> Result<Option<AuthCommand>, String> {
    if args.first().is_none_or(|first| first != "auth") {
        return Ok(None);
    }
    let kind = match args.get(1).map(String::as_str) {
        Some("check") => AuthCommandKind::Check,
        Some("print-api-key") => AuthCommandKind::ApiKey,
        Some("print-bearer-token") => AuthCommandKind::BearerToken,
        other => {
            return Err(format!(
                "Unknown auth command \"{}\". Use \"bake-rs auth print-api-key\", \"bake-rs auth print-bearer-token\", or \"bake-rs auth check\".",
                other.unwrap_or_default()
            ));
        }
    };
    let mut command = AuthCommand {
        kind,
        args: Vec::new(),
        json: false,
        credentials: false,
        no_refresh: false,
        min_expiry_ms: None,
    };
    let mut index = 2;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--min-expiry" {
            if kind != AuthCommandKind::BearerToken {
                return Err("--min-expiry is only supported by print-bearer-token".to_owned());
            }
            index += 1;
            command.min_expiry_ms = Some(
                args.get(index)
                    .and_then(|value| parse_duration_ms(value))
                    .ok_or("--min-expiry must use a duration such as 30m or 1h")?,
            );
            index += 1;
            continue;
        }
        if matches!(arg, "--json" | "--credentials" | "--no-refresh") {
            if kind != AuthCommandKind::Check {
                return Err(format!("{arg} is only supported by auth check"));
            }
            match arg {
                "--json" => command.json = true,
                "--credentials" => command.credentials = true,
                _ => command.no_refresh = true,
            }
            index += 1;
            continue;
        }
        command.args.push(args[index].clone());
        index += 1;
    }
    Ok(Some(command))
}

/// Pi's `/^(\d+)(ms|s|m|h)$/iu` duration, in milliseconds. `None` when the
/// value is not one, or its milliseconds do not fit.
fn parse_duration_ms(value: &str) -> Option<u64> {
    let digits = value
        .find(|ch: char| !ch.is_ascii_digit())
        .filter(|end| *end > 0)?;
    let amount: u64 = value[..digits].parse().ok()?;
    let factor = match value[digits..].to_ascii_lowercase().as_str() {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => return None,
    };
    amount.checked_mul(factor)
}

/// Pi's `validateAuthCommandArgs`: the trimmed `--provider` and `--model`,
/// and nothing else.
pub fn validate_auth_command_args(
    args: &Args,
    kind: AuthCommandKind,
) -> Result<(Option<String>, Option<String>), String> {
    let trimmed = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let provider = trimmed(&args.provider);
    let model = trimmed(&args.model);
    if let Some((name, _)) = args.unknown_flags.first() {
        return Err(format!("Unknown option --{name} for \"{}\".", kind.name()));
    }
    if args.api_key.is_some() || !args.messages.is_empty() || !args.file_args.is_empty() {
        return Err("Auth commands only accept --provider and --model".to_owned());
    }
    if provider.is_none() && model.is_none() {
        return Err(if kind == AuthCommandKind::Check {
            "Auth checks require --provider <provider> or --model <model>".to_owned()
        } else {
            "Credential printing requires --provider <provider> or --model <model>".to_owned()
        });
    }
    Ok((provider, model))
}

/// Pi's `AuthCheckStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthCheckStatus {
    /// `ready`.
    Ready,
    /// `not_ready`.
    NotReady,
    /// `invalid`.
    Invalid,
}

impl AuthCheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NotReady => "not_ready",
            Self::Invalid => "invalid",
        }
    }

    /// Pi's exit codes: ready 0, not ready 1, invalid 2.
    fn code(self) -> i32 {
        match self {
            Self::Ready => 0,
            Self::NotReady => 1,
            Self::Invalid => 2,
        }
    }
}

/// Pi's `AuthCheckResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCheckResult {
    /// The status.
    pub status: AuthCheckStatus,
    /// The provider checked.
    pub provider: String,
    /// Pi's `AuthCheckReason`, when the status is not `ready`.
    pub reason: Option<&'static str>,
    /// `api_key` when ready. `bake-rs` has no OAuth provider, so the
    /// credential of a ready provider is always a key.
    pub auth_type: Option<&'static str>,
}

/// Pi's `checkProviderAuth`, reading files only.
pub fn check_provider_auth(
    args: &Args,
    registry: &ModelRegistry,
) -> Result<AuthCheckResult, String> {
    let (cli_provider, cli_model) = validate_auth_command_args(args, AuthCommandKind::Check)?;
    let mut provider = cli_provider.clone();
    if let Some(cli_model) = &cli_model {
        let resolved = resolve_cli_model(cli_provider.as_deref(), Some(cli_model), None, registry);
        match (resolved.error, resolved.model) {
            (None, Some(model)) => provider = Some(model.provider),
            (error, _) => {
                return Err(
                    error.unwrap_or_else(|| format!("Unable to resolve model \"{cli_model}\""))
                );
            }
        }
    }
    let provider = provider.ok_or("Unable to resolve an auth provider")?;
    let result = |status: AuthCheckStatus, reason: Option<&'static str>| AuthCheckResult {
        status,
        provider: provider.clone(),
        reason,
        auth_type: None,
    };
    if registry.error().is_some() {
        return Ok(result(AuthCheckStatus::Invalid, Some("invalid_state")));
    }
    if !registry.has_provider(&provider) {
        return Ok(result(
            AuthCheckStatus::NotReady,
            Some("provider_not_found"),
        ));
    }
    if registry.check_auth(&provider).is_none() {
        return Ok(result(
            AuthCheckStatus::NotReady,
            Some("credentials_not_configured"),
        ));
    }
    Ok(AuthCheckResult {
        status: AuthCheckStatus::Ready,
        provider,
        reason: None,
        auth_type: Some("api_key"),
    })
}

/// The check's result as Pi's JSON, with the credential when it was asked
/// for and found.
fn check_json(result: &AuthCheckResult, credential: Option<&str>) -> String {
    let mut object = serde_json::Map::new();
    object.insert("status".into(), result.status.as_str().into());
    object.insert("provider".into(), result.provider.clone().into());
    if let Some(reason) = result.reason {
        object.insert("reason".into(), reason.into());
    }
    if let Some(auth_type) = result.auth_type {
        object.insert("authType".into(), auth_type.into());
    }
    if let Some(credential) = credential {
        object.insert("credentials".into(), credential.into());
    }
    serde_json::Value::Object(object).to_string()
}

/// What `auth.json` holds for `provider`, as Pi's `listCredentials` reports
/// a credential's type. A provider whose key comes from `models.json` or
/// the environment has no record, as in Pi.
fn stored_credential_type(credentials: &AuthStorage, provider: &str) -> Option<&'static str> {
    match credentials.read(provider) {
        Some(Credential::ApiKey { .. }) => Some("api_key"),
        Some(Credential::OAuth) => Some("oauth"),
        None => None,
    }
}

/// Pi's `resolveCredentialForPrint`: the one credential of the kind asked
/// for, or Pi's reason why there is none. The value is returned, never
/// logged.
pub fn resolve_credential_for_print(
    args: &Args,
    registry: &ModelRegistry,
    credentials: &AuthStorage,
    kind: AuthCommandKind,
) -> Result<String, String> {
    let (cli_provider, cli_model) = validate_auth_command_args(args, kind)?;
    // Pi's `providers` list: the one `--provider` names, or every provider
    // with a stored credential whose models `--model` matches.
    let mut providers: Vec<String> = Vec::new();
    match &cli_provider {
        Some(cli_provider) => {
            let provider = registry
                .models()
                .into_iter()
                .map(|model| model.provider)
                .find(|id| id == cli_provider)
                .filter(|id| registry.has_provider(id))
                .or_else(|| registry.has_provider(cli_provider).then(|| cli_provider.clone()))
                .ok_or_else(|| {
                    format!(
                        "Unknown provider \"{cli_provider}\". Use --list-models to see available providers."
                    )
                })?;
            if let Some(cli_model) = &cli_model {
                let resolved = resolve_cli_model(Some(&provider), Some(cli_model), None, registry);
                if resolved.model.is_none() {
                    return Err(resolved.error.unwrap_or_else(|| {
                        "Unable to resolve the requested provider/model".to_owned()
                    }));
                }
            }
            providers.push(provider);
        }
        None => {
            let cli_model = cli_model.as_deref().unwrap_or_default();
            let models = registry.models();
            for model in &models {
                if providers.contains(&model.provider)
                    || stored_credential_type(credentials, &model.provider).is_none()
                {
                    continue;
                }
                let of_provider: Vec<_> = models
                    .iter()
                    .filter(|other| other.provider == model.provider)
                    .cloned()
                    .collect();
                if find_exact_model_reference_match(cli_model, &of_provider).is_some() {
                    providers.push(model.provider.clone());
                }
            }
            if providers.is_empty() {
                return Err(format!(
                    "Model \"{cli_model}\" not found. Use --list-models to see available models."
                ));
            }
        }
    }
    let mut found: Vec<(String, String)> = Vec::new();
    for provider in &providers {
        let stored = stored_credential_type(credentials, provider);
        // Pi skips a provider whose stored credential is the other kind;
        // `bake-rs` has no OAuth provider, so a bearer token is never found.
        if kind == AuthCommandKind::ApiKey && stored == Some("oauth") {
            continue;
        }
        if kind == AuthCommandKind::BearerToken {
            continue;
        }
        if let Some(key) = registry.provider_api_key(provider)? {
            found.push((provider.clone(), key));
        }
    }
    match <[_; 1]>::try_from(found) {
        Ok([(_, value)]) => Ok(value),
        Err(found) if found.is_empty() => {
            let first = providers.first().map(String::as_str).unwrap_or_default();
            let stored = stored_credential_type(credentials, first);
            if cli_provider.is_some() && kind == AuthCommandKind::ApiKey && stored == Some("oauth")
            {
                return Err(format!(
                    "Provider \"{first}\" is configured with OAuth, not an API key"
                ));
            }
            if cli_provider.is_some() && kind == AuthCommandKind::BearerToken {
                return Err(format!(
                    "Provider \"{first}\" is not configured with an OAuth bearer token"
                ));
            }
            Err(format!(
                "No usable {} is configured",
                if kind == AuthCommandKind::ApiKey {
                    "API key"
                } else {
                    "OAuth bearer token"
                }
            ))
        }
        Err(found) => Err(format!(
            "Multiple configured providers matched ({}). Specify --provider.",
            found
                .iter()
                .map(|(provider, _)| provider.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Pi's `runAuthCommand` over one Bake home: the command's output on
/// `stdout`, its failures as `Error: ` lines on `stderr`, and Pi's exit
/// code. `None` when `args` is not an auth command. Blocking: it reads the
/// home's files.
pub fn run_auth_command(
    args: &[String],
    home: Option<&Path>,
    extra_providers: Vec<(String, serde_json::Value, Option<String>)>,
    stdout: &SharedWriter,
    stderr: &SharedWriter,
) -> Option<i32> {
    if is_auth_command_help(args) {
        write_out(stdout, AUTH_COMMAND_HELP);
        return Some(0);
    }
    let command = match parse_auth_command(args) {
        Ok(None) => return None,
        Ok(Some(command)) => command,
        Err(message) => return Some(fail(stderr, &message, 1)),
    };
    let parsed = parse_args(&command.args);
    if let Some(message) = unknown_flags_message(&parsed.unknown_flags) {
        // Pi names the command and its usage, not the general message.
        let _ = message;
        let (name, _) = parsed.unknown_flags.first()?;
        write_out(
            stderr,
            &format!(
                "Unknown option --{name} for \"{}\".\nUse \"bake-rs --help\" or \"{}\".\n",
                command.kind.name(),
                command.kind.usage()
            ),
        );
        return Some(1);
    }
    // Pi's `check` reports a bad argument as an invalid state (2); a print
    // command fails (1).
    let bad_argument = if command.kind == AuthCommandKind::Check {
        2
    } else {
        1
    };
    if parsed
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == DiagnosticKind::Error)
    {
        let message = parsed
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        return Some(fail(stderr, &message, bad_argument));
    }
    let Some(home) = home else {
        return Some(fail(
            stderr,
            "Cannot find the Bake home: set BAKE_HOME or make the user's home directory resolvable",
            bad_argument,
        ));
    };
    let mut registry = ModelRegistry::load(home);
    for (id, config, key) in extra_providers {
        // A refused import leaves the provider out, as it does for a run;
        // the check then reports it as not found.
        let _ = registry.add_provider_config(&id, config, key);
    }
    let _ = super::apply_account(home, &mut registry);
    let credentials = AuthStorage::load(&auth_path(home));
    if command.kind != AuthCommandKind::Check {
        return Some(
            match resolve_credential_for_print(&parsed, &registry, &credentials, command.kind) {
                Ok(credential) => {
                    write_out(stdout, &format!("{credential}\n"));
                    0
                }
                Err(message) => fail(stderr, &message, 1),
            },
        );
    }
    let result = match check_provider_auth(&parsed, &registry) {
        Ok(result) => result,
        Err(message) => return Some(fail(stderr, &message, 2)),
    };
    let credential = match (command.credentials, result.status) {
        (true, AuthCheckStatus::Ready) => {
            registry.provider_api_key(&result.provider).ok().flatten()
        }
        _ => None,
    };
    // Pi: a ready provider whose credential cannot be produced is not ready.
    let result =
        if command.credentials && result.status == AuthCheckStatus::Ready && credential.is_none() {
            AuthCheckResult {
                status: AuthCheckStatus::NotReady,
                provider: result.provider,
                reason: Some("credential_not_available"),
                auth_type: None,
            }
        } else {
            result
        };
    let output = if command.json {
        check_json(&result, credential.as_deref())
    } else {
        credential.unwrap_or_else(|| result.status.as_str().to_owned())
    };
    write_out(stdout, &format!("{output}\n"));
    Some(result.status.code())
}

fn fail(stderr: &SharedWriter, message: &str, code: i32) -> i32 {
    write_out(stderr, &format!("Error: {message}\n"));
    code
}

#[cfg(test)]
mod tests {
    //! Cases from Pi `test/auth-command.test.ts` and
    //! `test/credential-print.test.ts` for the commands `bake-rs` has.

    use super::*;
    use crate::model_registry::ModelConfig;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    /// Why `args` is not an auth command this binary runs.
    fn refusal(args: &[&str]) -> String {
        match parse_auth_command(&strings(args)) {
            Err(message) => message,
            Ok(_) => panic!("{args:?} parsed"),
        }
    }

    #[test]
    fn help_is_the_bare_command_help_and_any_help_flag() {
        for args in [
            vec!["auth"],
            vec!["auth", "help"],
            vec!["auth", "check", "--help"],
            vec!["auth", "print-api-key", "-h"],
        ] {
            assert!(is_auth_command_help(&strings(&args)), "{args:?}");
        }
        for args in [
            vec!["auth", "check"],
            vec!["--help"],
            vec!["-p", "auth", "help"],
        ] {
            assert!(!is_auth_command_help(&strings(&args)), "{args:?}");
        }
    }

    #[test]
    fn commands_and_their_flags_parse() {
        assert!(
            matches!(parse_auth_command(&strings(&["-p", "hi"])), Ok(None)),
            "not an auth command"
        );
        let command = parse_auth_command(&strings(&[
            "auth",
            "check",
            "--provider",
            "cliproxyapi",
            "--json",
            "--credentials",
            "--no-refresh",
        ]))
        .expect("parse")
        .expect("command");
        assert_eq!(command.kind, AuthCommandKind::Check);
        assert!(command.json && command.credentials && command.no_refresh);
        assert_eq!(command.args, ["--provider", "cliproxyapi"]);
        let command = parse_auth_command(&strings(&[
            "auth",
            "print-bearer-token",
            "--min-expiry",
            "2h",
            "--provider",
            "p",
        ]))
        .expect("parse")
        .expect("command");
        assert_eq!(command.min_expiry_ms, Some(2 * 3_600_000));
        assert_eq!(command.args, ["--provider", "p"]);
        assert_eq!(
            refusal(&["auth", "login"]),
            "Unknown auth command \"login\". Use \"bake-rs auth print-api-key\", \"bake-rs auth print-bearer-token\", or \"bake-rs auth check\"."
        );
        assert_eq!(
            refusal(&["auth", "check", "--min-expiry", "1h"]),
            "--min-expiry is only supported by print-bearer-token"
        );
        assert_eq!(
            refusal(&["auth", "print-api-key", "--json"]),
            "--json is only supported by auth check"
        );
        for value in ["", "soon", "1d", "h", "-1m", "99999999999999999999h"] {
            assert_eq!(
                refusal(&["auth", "print-bearer-token", "--min-expiry", value]),
                "--min-expiry must use a duration such as 30m or 1h",
                "{value:?}"
            );
        }
        assert_eq!(parse_duration_ms("250ms"), Some(250));
        assert_eq!(parse_duration_ms("30M"), Some(1_800_000));
        assert_eq!(parse_duration_ms("45s"), Some(45_000));
    }

    #[test]
    fn only_provider_and_model_are_accepted() {
        let args = parse_args(&strings(&["--provider", "  faux  "]));
        assert_eq!(
            validate_auth_command_args(&args, AuthCommandKind::Check),
            Ok((Some("faux".to_owned()), None))
        );
        assert_eq!(
            validate_auth_command_args(&parse_args(&[]), AuthCommandKind::Check),
            Err("Auth checks require --provider <provider> or --model <model>".to_owned())
        );
        assert_eq!(
            validate_auth_command_args(&parse_args(&[]), AuthCommandKind::ApiKey),
            Err("Credential printing requires --provider <provider> or --model <model>".to_owned())
        );
        // A key is never taken from an auth command, and never echoed.
        let refused = validate_auth_command_args(
            &parse_args(&strings(&["--provider", "faux", "--api-key", "sk-secret"])),
            AuthCommandKind::ApiKey,
        )
        .expect_err("refused");
        assert_eq!(refused, "Auth commands only accept --provider and --model");
        assert!(!refused.contains("sk-secret"));
        assert_eq!(
            validate_auth_command_args(
                &parse_args(&strings(&["--provider", "faux", "extra"])),
                AuthCommandKind::ApiKey
            ),
            Err("Auth commands only accept --provider and --model".to_owned())
        );
        assert_eq!(
            validate_auth_command_args(
                &parse_args(&strings(&["--provider", "faux", "--theme", "dark"])),
                AuthCommandKind::Check
            ),
            Err("Unknown option --theme for \"auth check\".".to_owned())
        );
    }

    /// A registry with `faux` (a stored key), `keyless` (no key), and
    /// `oauth` (an OAuth record, which owns its provider).
    fn registry() -> (ModelRegistry, AuthStorage) {
        let auth = AuthStorage::from_json(
            r#"{ "faux": { "type": "api_key", "key": "sk-faux-stored" },
                 "oauth": { "type": "oauth" } }"#,
        );
        let config = ModelConfig::from_value(
            serde_json::json!({
                "providers": {
                    "faux": { "baseUrl": "https://example.invalid", "api": "openai-completions",
                        "models": [{ "id": "faux-1", "contextWindow": 1000 }] },
                    "keyless": { "baseUrl": "https://example.invalid", "api": "openai-completions",
                        "models": [{ "id": "keyless-1", "contextWindow": 1000 }] },
                    "oauth": { "baseUrl": "https://example.invalid", "api": "openai-completions",
                        "models": [{ "id": "oauth-1", "contextWindow": 1000 }] },
                },
            }),
            "models.json",
        );
        (
            ModelRegistry::new(config, auth.clone()),
            AuthStorage::from_json(
                r#"{ "faux": { "type": "api_key", "key": "sk-faux-stored" },
                     "oauth": { "type": "oauth" } }"#,
            ),
        )
    }

    // Pi `auth-check.test.ts`: ready, not ready with its reason, and the
    // statuses' exit codes.
    #[test]
    fn a_check_reports_each_status() {
        let (registry, _) = registry();
        let check = |args: &[&str]| {
            check_provider_auth(&parse_args(&strings(args)), &registry).expect("checked")
        };
        let ready = check(&["--provider", "faux"]);
        assert_eq!(ready.status, AuthCheckStatus::Ready);
        assert_eq!(ready.auth_type, Some("api_key"));
        assert_eq!(ready.status.code(), 0);
        let not_configured = check(&["--provider", "keyless"]);
        assert_eq!(not_configured.status, AuthCheckStatus::NotReady);
        assert_eq!(not_configured.reason, Some("credentials_not_configured"));
        assert_eq!(not_configured.status.code(), 1);
        let missing = check(&["--provider", "nope"]);
        assert_eq!(missing.reason, Some("provider_not_found"));
        // An OAuth record owns its provider, and no OAuth method is ported.
        assert_eq!(
            check(&["--provider", "oauth"]).reason,
            Some("credentials_not_configured")
        );
        // `--model` names the provider to check.
        let by_model = check(&["--model", "faux/faux-1"]);
        assert_eq!(by_model.provider, "faux");
        assert_eq!(by_model.status, AuthCheckStatus::Ready);
        assert!(
            check_provider_auth(&parse_args(&strings(&["--model", "nope"])), &registry).is_err()
        );
        assert_eq!(AuthCheckStatus::Invalid.code(), 2);
        assert_eq!(
            check_json(&ready, Some("sk-faux-stored")),
            r#"{"status":"ready","provider":"faux","authType":"api_key","credentials":"sk-faux-stored"}"#
        );
        assert_eq!(
            check_json(&not_configured, None),
            r#"{"status":"not_ready","provider":"keyless","reason":"credentials_not_configured"}"#
        );
    }

    // Pi `credential-print.test.ts`: one API key, the reason when there is
    // none, and no bearer token, since no OAuth provider is ported.
    #[test]
    fn printing_resolves_one_credential_or_says_why_not() {
        let (registry, credentials) = registry();
        let resolve = |args: &[&str], kind| {
            resolve_credential_for_print(&parse_args(&strings(args)), &registry, &credentials, kind)
        };
        assert_eq!(
            resolve(&["--provider", "faux"], AuthCommandKind::ApiKey),
            Ok("sk-faux-stored".to_owned())
        );
        assert_eq!(
            resolve(
                &["--provider", "faux", "--model", "faux-1"],
                AuthCommandKind::ApiKey
            ),
            Ok("sk-faux-stored".to_owned())
        );
        assert_eq!(
            resolve(&["--provider", "nope"], AuthCommandKind::ApiKey),
            Err(
                "Unknown provider \"nope\". Use --list-models to see available providers."
                    .to_owned()
            )
        );
        assert_eq!(
            resolve(&["--provider", "keyless"], AuthCommandKind::ApiKey),
            Err("No usable API key is configured".to_owned())
        );
        assert_eq!(
            resolve(&["--provider", "oauth"], AuthCommandKind::ApiKey),
            Err("Provider \"oauth\" is configured with OAuth, not an API key".to_owned())
        );
        assert_eq!(
            resolve(&["--provider", "faux"], AuthCommandKind::BearerToken),
            Err("Provider \"faux\" is not configured with an OAuth bearer token".to_owned())
        );
        assert_eq!(
            resolve(&["--model", "faux-1"], AuthCommandKind::BearerToken),
            Err("No usable OAuth bearer token is configured".to_owned())
        );
        // `--model` alone searches the providers with a stored credential.
        assert_eq!(
            resolve(&["--model", "faux-1"], AuthCommandKind::ApiKey),
            Ok("sk-faux-stored".to_owned())
        );
        assert_eq!(
            resolve(&["--model", "keyless-1"], AuthCommandKind::ApiKey),
            Err(
                "Model \"keyless-1\" not found. Use --list-models to see available models."
                    .to_owned()
            )
        );
    }
}
