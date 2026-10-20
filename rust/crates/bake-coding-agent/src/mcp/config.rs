//! MCP server configuration: the `mcpServers` shape other MCP clients share.
//!
//! Adapted from Pi v1.3.0 `src/extensions/mcp/config.ts` (`readConfigFile`)
//! and `src/core/mcp-servers.ts` (`validateMcpServerConfig`,
//! `getMcpToolExposure`), with Pi's error messages. Bake reads one file the
//! caller names, such as `$BAKE_HOME/mcp.json`; there is no project file,
//! so a repository cannot add a server, change one, or run a command.
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "files": { "command": "mcp-files", "args": ["--root", "."], "env": { "TOKEN": "${FILES_TOKEN}" } }
//!   }
//! }
//! ```
//!
//! Differences from Pi, each refused with a message rather than ignored:
//!
//! - Server names follow Bake's tool-name contract,
//!   `[A-Za-z0-9_-]{1,32}` (`packages/mcp/mcp-client`).
//! - The only exposures are `direct` (the default here; Pi's is
//!   `codemode`) and `hidden`: Pi's `codemode`, `codemode-deferred`, and
//!   `deferred` are not supported yet.
//! - An environment or header value may reference variables (`$NAME`,
//!   `${NAME}`), as
//!   Pi's config values do, but never run a command (`!cmd`). A server's
//!   environment is Bake's scrubbed environment, which drops credential-like
//!   and Bake-managed names, with the entry's `env` over it, so naming a
//!   scrubbed variable here passes it: the file's owner authorizes it, as an
//!   explicit `env` overrides the scrub in Bake's TypeScript
//!   (`packages/mcp/mcp-client/src/transport.ts` `buildChildEnv`, and
//!   `scrubbedParentEnv` in `packages/subprocess/subprocess/src/index.ts`).
//!   Only an owner-controlled file may therefore be loaded. A sandbox
//!   runner's own variables still go over the entry's, so `env` cannot
//!   forge them.
//! - A `url` server's headers are values of the same kind, resolved when the
//!   server connects ([`super::egress`]); `!cmd` is refused there too.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::config_value::is_command_config_value;
use crate::tools::index::tool_name_matcher;

/// The file name in the Bake home.
pub const MCP_CONFIG_FILE: &str = "mcp.json";

/// The largest configuration file read.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// The longest server name, Bake's `serverName` limit.
pub const MAX_SERVER_NAME_LENGTH: usize = 32;

/// The longest per-request timeout accepted, in seconds.
const MAX_TIMEOUT_SECONDS: f64 = 86_400.0;

/// How a server's tools reach the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpExposure {
    /// Declared to the model like the built-in tools.
    Direct,
    /// Listed but neither declared nor callable.
    Hidden,
}

/// What every server entry may set.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerSettings {
    /// `enabled`; a disabled entry is kept but not started. Default: true.
    pub enabled: bool,
    /// `exposure`. Default: [`McpExposure::Direct`].
    pub exposure: McpExposure,
    /// `toolExposure`: exact tool names or `*` patterns, in file order.
    pub tool_exposure: Vec<(String, McpExposure)>,
    /// `timeout`: each request's deadline. Default: 60 s, as Pi.
    pub timeout: Duration,
    /// `description`.
    pub description: Option<String>,
}

impl Default for McpServerSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            exposure: McpExposure::Direct,
            tool_exposure: Vec::new(),
            timeout: Duration::from_secs(60),
            description: None,
        }
    }
}

impl McpServerSettings {
    /// Pi's `getMcpToolExposure`: an exact `toolExposure` entry, else the
    /// first pattern that matches, else the server's exposure.
    pub fn tool_exposure(&self, tool: &str) -> McpExposure {
        if let Some((_, exposure)) = self.tool_exposure.iter().find(|(name, _)| name == tool) {
            return *exposure;
        }
        self.tool_exposure
            .iter()
            .filter(|(pattern, _)| pattern.contains('*'))
            .find(|(pattern, _)| tool_name_matcher(std::slice::from_ref(pattern))(tool))
            .map_or(self.exposure, |(_, exposure)| *exposure)
    }
}

/// A server run as a child process over its standard input and output.
/// `Debug` shows the names of `env`, never its values.
#[derive(Clone, PartialEq)]
pub struct McpStdioConfig {
    /// `command`; `~` and `~/…` name the home directory.
    pub command: String,
    /// `args`; each may start with `~` as `command` may.
    pub args: Vec<String>,
    /// `env`, in file order, set over Bake's scrubbed environment. Values
    /// may reference variables, resolved when the server starts.
    pub env: Vec<(String, String)>,
    /// `cwd`, relative to the session's working directory.
    pub cwd: Option<String>,
}

impl std::fmt::Debug for McpStdioConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env: Vec<&str> = self.env.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("McpStdioConfig")
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env", &env)
            .field("cwd", &self.cwd)
            .finish()
    }
}

/// A server reached over Streamable HTTP. `Debug` shows the URL's origin
/// and the names of `headers`, never a path, a query, or a header value.
#[derive(Clone, PartialEq)]
pub struct McpHttpConfig {
    /// `url`: the endpoint, `http` or `https`.
    pub url: String,
    /// `headers`, in file order, sent with every request. Values may
    /// reference variables, resolved when the server connects.
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for McpHttpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let origin = reqwest::Url::parse(&self.url)
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_else(|_| "<invalid>".to_owned());
        let headers: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("McpHttpConfig")
            .field("origin", &origin)
            .field("headers", &headers)
            .finish()
    }
}

/// A server's transport.
#[derive(Debug, Clone, PartialEq)]
pub enum McpTransportConfig {
    /// `command`: a stdio server.
    Stdio(McpStdioConfig),
    /// `url`: a Streamable HTTP server.
    Http(McpHttpConfig),
}

/// One validated `mcpServers` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerEntry {
    /// The entry's name: the namespace of its tool names.
    pub name: String,
    /// How it is reached.
    pub transport: McpTransportConfig,
    /// What every entry may set.
    pub settings: McpServerSettings,
}

/// A configuration file's servers, in file order, and the problems found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McpConfig {
    /// The valid entries, disabled ones included.
    pub servers: Vec<McpServerEntry>,
    /// One line per refused entry or file problem, each starting with the
    /// file's path.
    pub errors: Vec<String>,
}

/// Reads `path`: a missing file is an empty configuration; a file that
/// cannot be read, is a symbolic link or not a regular file (a FIFO is
/// refused without waiting for a writer), is over [`MAX_CONFIG_BYTES`], or is
/// not the expected shape is an error and no server.
///
/// The file's `env` values may pass variables the scrub drops (see the
/// module documentation), so `path` must be a file only the user controls.
/// `bake-rs` reads `$BAKE_HOME/mcp.json` through
/// [`load_home_mcp_config`](super::home::load_home_mcp_config), which also
/// checks who can change it; this function checks only what it opens.
pub fn load_mcp_config(path: &Path) -> McpConfig {
    match open_regular_file(path) {
        Ok(None) => McpConfig::default(),
        Ok(Some(file)) => read_config(file, path),
        Err(error) => failed(format!("{}: {error}", path.display())),
    }
}

/// Opens `path` for reading, `None` when it does not exist. The open never
/// follows a link at the last component and never waits: on Unix with
/// `O_NOFOLLOW | O_NONBLOCK | O_NOCTTY`, so a FIFO opens at once and is then
/// refused, and on Windows with `FILE_FLAG_OPEN_REPARSE_POINT`. The opened
/// handle must be a regular file; every later check reads that handle.
pub(crate) fn open_regular_file(path: &Path) -> Result<Option<fs::File>, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        /// `FILE_FLAG_OPEN_REPARSE_POINT`: open a link itself.
        const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(OPEN_REPARSE_POINT);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(LINK_REFUSED.to_owned());
        }
        Err(error) => return Err(error.to_string()),
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        /// `FILE_ATTRIBUTE_REPARSE_POINT`.
        const REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & REPARSE_POINT != 0 {
            return Err(LINK_REFUSED.to_owned());
        }
    }
    if !metadata.is_file() {
        return Err("is not a regular file; bake-rs reads only a regular file".to_owned());
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(format!("larger than {MAX_CONFIG_BYTES} bytes"));
    }
    Ok(Some(file))
}

pub(crate) const LINK_REFUSED: &str =
    "is a symbolic link; bake-rs reads only a regular file, so replace the link with the file";

/// Reads and parses an opened configuration file, at most
/// [`MAX_CONFIG_BYTES`].
pub(crate) fn read_config(file: fs::File, path: &Path) -> McpConfig {
    let shown = path.display();
    let mut text = String::new();
    match file.take(MAX_CONFIG_BYTES + 1).read_to_string(&mut text) {
        Ok(read) if read as u64 > MAX_CONFIG_BYTES => {
            failed(format!("{shown}: larger than {MAX_CONFIG_BYTES} bytes"))
        }
        Ok(_) => parse_mcp_config(&text, path),
        Err(error) => failed(format!("{shown}: {error}")),
    }
}

pub(crate) fn failed(error: String) -> McpConfig {
    McpConfig {
        servers: Vec::new(),
        errors: vec![error],
    }
}

/// Parses one configuration file's text; `source` names it in errors.
pub fn parse_mcp_config(text: &str, source: &Path) -> McpConfig {
    let shown = source.display();
    let parsed: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => return failed(format!("{shown}: {error}")),
    };
    let servers = match &parsed {
        Value::Object(object) => match object.get("mcpServers") {
            None => return McpConfig::default(),
            Some(Value::Object(servers)) => servers,
            Some(_) => return failed(expected_shape(&shown)),
        },
        _ => return failed(expected_shape(&shown)),
    };
    let mut config = McpConfig::default();
    for (name, value) in servers {
        match validate_server(name, value) {
            Ok(entry) => config.servers.push(entry),
            Err(error) => config.errors.push(format!("{shown}: {error}")),
        }
    }
    config
}

fn expected_shape(shown: &std::path::Display<'_>) -> String {
    format!("{shown}: expected an object with an \"mcpServers\" object")
}

fn is_server_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SERVER_NAME_LENGTH
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn exposure(name: &str, field: &str, value: &Value) -> Result<McpExposure, String> {
    match value.as_str() {
        Some("direct") => Ok(McpExposure::Direct),
        Some("hidden") => Ok(McpExposure::Hidden),
        Some(other @ ("codemode" | "codemode-deferred" | "deferred")) => Err(format!(
            "server \"{name}\": {field} \"{other}\" is not supported yet; use \"direct\" or \"hidden\""
        )),
        _ => Err(format!(
            "server \"{name}\": {field} must be one of \"direct\", \"hidden\""
        )),
    }
}

fn string_map(value: &Value) -> Option<Vec<(String, String)>> {
    let object = value.as_object()?;
    object
        .iter()
        .map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
        .collect()
}

/// Pi's `validateMcpServerConfig`, with Bake's differences (see the module
/// documentation).
fn validate_server(name: &str, value: &Value) -> Result<McpServerEntry, String> {
    if !is_server_name(name) {
        return Err(format!(
            "invalid server name \"{name}\" (use letters, digits, \"_\" and \"-\", at most {MAX_SERVER_NAME_LENGTH})"
        ));
    }
    let Value::Object(object) = value else {
        return Err(format!("server \"{name}\" must be an object"));
    };
    let settings = validate_settings(name, object)?;
    let kind = object.get("type");
    if kind.and_then(Value::as_str) == Some("sse") {
        return Err(format!(
            "server \"{name}\": legacy SSE transport is not supported; use the streamable HTTP URL"
        ));
    }
    let kind = kind.map(|kind| kind.as_str().unwrap_or_default());
    if let Some(Value::String(url)) = object.get("url")
        && matches!(kind, None | Some("http" | "streamable-http"))
    {
        let parsed = reqwest::Url::parse(url).ok();
        if !parsed.is_some_and(|url| matches!(url.scheme(), "http" | "https")) {
            return Err(format!(
                "server \"{name}\": url must be an http or https URL"
            ));
        }
        let headers = match object.get("headers") {
            None => Vec::new(),
            Some(headers) => string_map(headers)
                .ok_or_else(|| format!("server \"{name}\": headers must map names to strings"))?,
        };
        if let Some((key, _)) = headers
            .iter()
            .find(|(_, value)| is_command_config_value(value))
        {
            return Err(format!(
                "server \"{name}\": headers \"{key}\" runs a command, which Bake does not run; set the value or reference a variable with ${{NAME}}"
            ));
        }
        return Ok(McpServerEntry {
            name: name.to_owned(),
            transport: McpTransportConfig::Http(McpHttpConfig {
                url: url.clone(),
                headers,
            }),
            settings,
        });
    }
    if let Some(Value::String(command)) = object.get("command")
        && matches!(kind, None | Some("stdio"))
    {
        let args = match object.get("args") {
            None => Vec::new(),
            Some(Value::Array(args)) => args
                .iter()
                .map(|arg| arg.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| format!("server \"{name}\": args must be an array of strings"))?,
            Some(_) => {
                return Err(format!(
                    "server \"{name}\": args must be an array of strings"
                ));
            }
        };
        let env = match object.get("env") {
            None => Vec::new(),
            Some(env) => string_map(env)
                .ok_or_else(|| format!("server \"{name}\": env must map names to strings"))?,
        };
        if let Some((key, _)) = env.iter().find(|(_, value)| is_command_config_value(value)) {
            return Err(format!(
                "server \"{name}\": env \"{key}\" runs a command, which Bake does not run; set the value or reference a variable with ${{NAME}}"
            ));
        }
        let cwd = match object.get("cwd") {
            None => None,
            Some(Value::String(cwd)) => Some(cwd.clone()),
            Some(_) => return Err(format!("server \"{name}\": cwd must be a string")),
        };
        return Ok(McpServerEntry {
            name: name.to_owned(),
            transport: McpTransportConfig::Stdio(McpStdioConfig {
                command: command.clone(),
                args,
                env,
                cwd,
            }),
            settings,
        });
    }
    Err(format!(
        "server \"{name}\" needs either \"command\" (stdio) or \"url\" (streamable HTTP)"
    ))
}

fn validate_settings(name: &str, object: &Map<String, Value>) -> Result<McpServerSettings, String> {
    let mut settings = McpServerSettings::default();
    if let Some(value) = object.get("exposure") {
        settings.exposure = exposure(name, "exposure", value)?;
    }
    if let Some(value) = object.get("toolExposure") {
        let Value::Object(entries) = value else {
            return Err(format!(
                "server \"{name}\": toolExposure must map tool names to exposures"
            ));
        };
        for (tool, value) in entries {
            let field = format!("toolExposure \"{tool}\"");
            settings
                .tool_exposure
                .push((tool.clone(), exposure(name, &field, value)?));
        }
    }
    match object.get("enabled") {
        None => {}
        Some(Value::Bool(enabled)) => settings.enabled = *enabled,
        Some(_) => return Err(format!("server \"{name}\": enabled must be a boolean")),
    }
    match object.get("description") {
        None => {}
        Some(Value::String(description)) => settings.description = Some(description.clone()),
        Some(_) => return Err(format!("server \"{name}\": description must be a string")),
    }
    if let Some(value) = object.get("timeout") {
        let seconds = value.as_f64().filter(|seconds| *seconds > 0.0);
        let Some(seconds) = seconds.filter(|seconds| *seconds <= MAX_TIMEOUT_SECONDS) else {
            return Err(format!(
                "server \"{name}\": timeout must be a positive number of seconds, at most {MAX_TIMEOUT_SECONDS}"
            ));
        };
        settings.timeout = Duration::from_secs_f64(seconds);
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    //! Pi v1.3.0 `test/mcp-extension.test.ts`, "MCP config", for the
    //! global file Bake reads, and Bake's refusals.

    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn parse(value: Value) -> McpConfig {
        parse_mcp_config(&value.to_string(), &PathBuf::from("/home/u/.bake/mcp.json"))
    }

    // Pi: "merges global and trusted project servers and validates
    // entries", for the global file.
    #[test]
    fn validates_entries_and_keeps_disabled_ones() {
        let config = parse(json!({
            "mcpServers": {
                "shared": { "command": "global-cmd" },
                "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer ${TOKEN}" } },
                "off": { "command": "x", "enabled": false },
                "bad": { "args": ["no command"] },
                "legacy": { "type": "sse", "url": "https://example.com/sse" },
                "badUrl": { "url": "example.com/mcp" },
                "bad name": { "command": "x" },
            }
        }));
        let names: Vec<_> = config
            .servers
            .iter()
            .map(|server| server.name.as_str())
            .collect();
        assert_eq!(names, ["shared", "remote", "off"]);
        assert!(!config.servers[2].settings.enabled);
        assert_eq!(
            config.servers[1].transport,
            McpTransportConfig::Http(McpHttpConfig {
                url: "https://example.com/mcp".into(),
                headers: vec![("Authorization".into(), "Bearer ${TOKEN}".into())],
            })
        );
        assert_eq!(config.errors.len(), 4);
        assert!(config.errors[0].contains("server \"bad\" needs either \"command\""));
        assert!(config.errors[1].contains("legacy SSE transport is not supported"));
        assert!(config.errors[2].contains("server \"badUrl\": url must be an http or https URL"));
        assert!(config.errors[3].contains("invalid server name \"bad name\""));
        assert!(
            config
                .errors
                .iter()
                .all(|error| error.starts_with("/home/u/.bake/mcp.json: "))
        );
    }

    #[test]
    fn reads_stdio_fields_in_order() {
        let config = parse(json!({
            "mcpServers": {
                "files": {
                    "command": "~/bin/files",
                    "args": ["--root", "."],
                    "env": { "B": "2", "A": "${HOME}" },
                    "cwd": "sub",
                    "timeout": 1.5,
                    "description": "Files"
                }
            }
        }));
        assert!(config.errors.is_empty(), "{:?}", config.errors);
        let server = &config.servers[0];
        assert_eq!(
            server.transport,
            McpTransportConfig::Stdio(McpStdioConfig {
                command: "~/bin/files".into(),
                args: vec!["--root".into(), ".".into()],
                env: vec![("B".into(), "2".into()), ("A".into(), "${HOME}".into())],
                cwd: Some("sub".into()),
            })
        );
        assert_eq!(server.settings.timeout, Duration::from_millis(1500));
        assert_eq!(server.settings.description.as_deref(), Some("Files"));
    }

    #[test]
    fn debug_output_names_env_and_header_values_never() {
        let config = parse(json!({
            "mcpServers": {
                "s": { "command": "x", "env": { "TOKEN": "sk-secret-value" } },
                "r": {
                    "url": "https://example.com/mcp?token=query-secret",
                    "headers": { "Authorization": "Bearer header-secret" },
                },
            }
        }));
        let shown = format!("{config:?}");
        assert!(
            shown.contains("TOKEN") && shown.contains("Authorization"),
            "{shown}"
        );
        assert!(shown.contains("https://example.com"), "{shown}");
        assert!(!shown.contains("secret"), "{shown}");
    }

    #[test]
    fn refuses_what_bake_does_not_run() {
        let config = parse(json!({
            "mcpServers": {
                "cmd": { "command": "x", "env": { "TOKEN": "!op read secret" } },
                "header-cmd": { "url": "https://example.com/mcp", "headers": { "Authorization": "!op read secret" } },
                "code": { "command": "x", "exposure": "codemode" },
                "deferred": { "command": "x", "toolExposure": { "a": "deferred" } },
                "long-name-that-is-over-thirty-two-chars": { "command": "x" },
                "slow": { "command": "x", "timeout": 0 },
                "never": { "command": "x", "timeout": 1.0e9 },
            }
        }));
        assert!(config.servers.is_empty());
        assert!(config.errors[0].contains("env \"TOKEN\" runs a command"));
        assert!(
            config.errors[1].contains("headers \"Authorization\" runs a command"),
            "{:?}",
            config.errors
        );
        assert!(config.errors[2].ends_with(
            "server \"code\": exposure \"codemode\" is not supported yet; use \"direct\" or \"hidden\""
        ));
        assert!(config.errors[3].contains(
            "toolExposure \"a\" \"deferred\" is not supported yet; use \"direct\" or \"hidden\""
        ));
        assert!(config.errors[4].contains("at most 32"));
        assert!(config.errors[5].contains("timeout must be a positive number"));
        assert!(config.errors[6].contains("timeout must be a positive number"));
    }

    // `timeout` is untrusted JSON: a negative, zero, oversized, or
    // non-numeric value is refused, never converted (`Duration::from_secs_f64`
    // panics on negative, non-finite, and overflowing input), and a number
    // JSON cannot hold refuses the file.
    #[test]
    fn timeouts_outside_their_bounds_are_refused() {
        let text = r#"{ "mcpServers": {
            "negative": { "command": "x", "timeout": -1 },
            "tiny-negative": { "command": "x", "timeout": -1e-300 },
            "max": { "command": "x", "timeout": 18446744073709551615 },
            "just-over": { "command": "x", "timeout": 86400.001 },
            "text": { "command": "x", "timeout": "60" },
            "null": { "command": "x", "timeout": null },
            "at-max": { "command": "x", "timeout": 86400 },
            "tiny": { "command": "x", "timeout": 1e-300 }
        } }"#;
        let config = parse_mcp_config(text, Path::new("mcp.json"));
        let names: Vec<&str> = config
            .servers
            .iter()
            .map(|server| server.name.as_str())
            .collect();
        assert_eq!(names, ["at-max", "tiny"]);
        assert_eq!(config.errors.len(), 6, "{:?}", config.errors);
        assert!(
            config
                .errors
                .iter()
                .all(|error| error.contains("timeout must be a positive number"))
        );
        assert_eq!(
            config.servers[0].settings.timeout,
            Duration::from_secs(86_400)
        );
        let overflow = parse_mcp_config(
            r#"{ "mcpServers": { "inf": { "command": "x", "timeout": 1e400 } } }"#,
            Path::new("mcp.json"),
        );
        assert!(overflow.servers.is_empty());
        assert_eq!(overflow.errors.len(), 1, "{:?}", overflow.errors);
    }

    #[test]
    fn refuses_malformed_files_whole() {
        let path = PathBuf::from("mcp.json");
        assert_eq!(parse_mcp_config("{", &path).errors.len(), 1);
        assert_eq!(
            parse_mcp_config("[]", &path).errors,
            ["mcp.json: expected an object with an \"mcpServers\" object"]
        );
        assert_eq!(
            parse_mcp_config("{\"mcpServers\": []}", &path).errors,
            ["mcp.json: expected an object with an \"mcpServers\" object"]
        );
        assert_eq!(parse_mcp_config("{}", &path), McpConfig::default());
    }

    #[test]
    fn loads_a_missing_file_as_empty_and_bounds_its_size() {
        let dir = std::env::temp_dir().join(format!("bake-mcp-config-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(MCP_CONFIG_FILE);
        let _ = fs::remove_file(&path);
        assert_eq!(load_mcp_config(&path), McpConfig::default());
        fs::write(&path, vec![b' '; (MAX_CONFIG_BYTES + 1) as usize]).unwrap();
        assert!(load_mcp_config(&path).errors[0].contains("larger than"));
        fs::write(&path, r#"{"mcpServers":{"a":{"command":"x"}}}"#).unwrap();
        assert_eq!(load_mcp_config(&path).servers.len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    // Pi: "resolves per-tool exposure from exact names, then patterns in
    // order".
    #[test]
    fn resolves_tool_exposure_from_exact_names_then_patterns() {
        let config = parse(json!({
            "mcpServers": {
                "s": {
                    "command": "x",
                    "exposure": "hidden",
                    "toolExposure": { "read_*": "direct", "read_secret": "hidden", "*": "hidden", "write": "direct" }
                }
            }
        }));
        let settings = &config.servers[0].settings;
        assert_eq!(settings.tool_exposure("read_file"), McpExposure::Direct);
        assert_eq!(settings.tool_exposure("read_secret"), McpExposure::Hidden);
        assert_eq!(settings.tool_exposure("write"), McpExposure::Direct);
        assert_eq!(settings.tool_exposure("other"), McpExposure::Hidden);
        let plain = McpServerSettings::default();
        assert_eq!(plain.tool_exposure("anything"), McpExposure::Direct);
    }
}
