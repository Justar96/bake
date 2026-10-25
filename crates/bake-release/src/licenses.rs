//! The third-party license bundle a native release archive carries: one
//! text file per release platform and a machine-readable inventory, for
//! `bake`, the binary of [`ROOT_PACKAGE`].
//!
//! Selection, from Cargo's own resolver: a platform's `runtime` crates are
//! those `cargo tree -p bake-cli --target <triple> -e normal,no-proc-macro`
//! lists, the crates linked into the binary. Build scripts, proc macros,
//! and their dependencies run on the build host, and `cargo tree` evaluates
//! their `cfg()` for the host it runs on, so each platform also lists, as
//! `build`, every crate that `cargo tree --target all -e normal,build
//! --no-dedupe` shows below a build-dependency or proc-macro edge of its
//! linked crates, with no host `cfg()` applied: a superset of what its
//! runner builds. Dev-dependencies are excluded. Bake's own crates are
//! listed with their NOTICE files, and with the license files of the
//! third-party artifacts they embed: each `LICENSE-*` file in an immediate
//! subdirectory of the crate.
//!
//! Collection: cargo-about ([`CARGO_ABOUT_VERSION`] exactly, config
//! `about.toml`) owns license policy. This module checks provenance: the
//! crate set must equal the union of the per-target trees, every text must
//! be read from a file of the crate's own source package and appear in it
//! verbatim, and each clarification must name a compiled crate, keep its
//! declared license, and hash its files. NOTICE, COPYRIGHT, and COPYING
//! files at a crate's root are reproduced verbatim. For crates published
//! without the license file their repository keeps, `licenses/upstream.toml`
//! names a reviewed copy vendored under `licenses/`, used only where
//! cargo-about found none and every pin matches.
//!
//! Ported from Bake's `scripts/release/native-licenses.ts` (revision
//! 97ce6abba0), which generated the 0.4.0 bundle; the rules and the output
//! format are its own.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::util::{Result, Scratch, read_text, run, sha256};
use crate::workspace::ROOT_PACKAGE;

/// The cargo-about release the bundle is generated with.
pub const CARGO_ABOUT_VERSION: &str = "0.9.2";
/// The inventory's schema id.
pub const SCHEMA: &str = "bake-native-licenses/1";
/// Each platform's license file, at `<platform>/`.
pub const BUNDLE_FILE: &str = "THIRD-PARTY-LICENSES.txt";
/// The machine-readable inventory.
pub const INVENTORY_FILE: &str = "inventory.json";

/// Release platform ids and the Rust targets their runners build.
pub const PLATFORMS: [(&str, &str); 5] = [
    ("darwin-arm64", "aarch64-apple-darwin"),
    ("darwin-x64", "x86_64-apple-darwin"),
    ("linux-arm64", "aarch64-unknown-linux-gnu"),
    ("linux-x64", "x86_64-unknown-linux-gnu"),
    ("win32-x64", "x86_64-pc-windows-msvc"),
];

const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
const CONFIG_KEYS: [&str; 5] = [
    "accepted",
    "targets",
    "ignore-dev-dependencies",
    "ignore-build-dependencies",
    "private",
];
const SEARCH_LIMIT: u64 = 4 << 20;

fn rule() -> String {
    "=".repeat(80)
}

fn thin_rule() -> String {
    "-".repeat(80)
}

// ---------------------------------------------------------------- ordering

/// The primary weight of a character under the CLDR root collation that
/// `localeCompare(…, 'en')` applies: punctuation, then digits, then
/// letters, case aside.
fn primary(c: char) -> (u32, u32) {
    const PUNCTUATION: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";
    if c.is_whitespace() {
        return (0, c as u32);
    }
    if let Some(at) = PUNCTUATION.find(c) {
        return (1, u32::try_from(at).unwrap_or(u32::MAX));
    }
    if let Some(digit) = c.to_digit(10) {
        return (2, digit);
    }
    if c.is_ascii_alphabetic() {
        return (3, c.to_ascii_lowercase() as u32);
    }
    (4, c as u32)
}

/// `left.localeCompare(right, 'en')` for the ASCII names a crate graph
/// holds: primary weights first, then lowercase before uppercase, then
/// code points.
pub fn locale_cmp(left: &str, right: &str) -> Ordering {
    let weights = |text: &str| text.chars().map(primary).collect::<Vec<_>>();
    weights(left)
        .cmp(&weights(right))
        .then_with(|| {
            let case = |text: &str| {
                text.chars()
                    .map(|c| u8::from(c.is_ascii_uppercase()))
                    .collect::<Vec<_>>()
            };
            case(left).cmp(&case(right))
        })
        .then_with(|| left.cmp(right))
}

/// Orders versions numerically where they are numeric, so 0.10 follows 0.9.
fn compare_versions(left: &str, right: &str) -> Ordering {
    let split = |text: &str| -> Vec<String> {
        text.split(['.', '+', '-']).map(ToOwned::to_owned).collect()
    };
    let (a, b) = (split(left), split(right));
    for index in 0..a.len().max(b.len()) {
        let x = a.get(index).map_or("", String::as_str);
        let y = b.get(index).map_or("", String::as_str);
        let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let order = if numeric(x) && numeric(y) {
            x.parse::<u128>()
                .unwrap_or(0)
                .cmp(&y.parse::<u128>().unwrap_or(0))
        } else {
            locale_cmp(x, y)
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

/// Orders `name@version` labels by name, then version.
fn by_label(left: &str, right: &str) -> Ordering {
    let (a, av) = left.split_once('@').unwrap_or((left, ""));
    let (b, bv) = right.split_once('@').unwrap_or((right, ""));
    locale_cmp(a, b).then_with(|| compare_versions(av, bv))
}

fn label(key: &str) -> String {
    key.replacen('@', " ", 1)
}

// ---------------------------------------------------------------- trees

/// A crate as `cargo tree` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    pub version: String,
}

impl TreeEntry {
    fn key(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

/// `name vVERSION (…) (…)`: a package as `-f {p}` prints it, and the text
/// after it, the parenthesized annotations.
fn package_spec(text: &str) -> Option<(TreeEntry, &str)> {
    let (name, rest) = text.split_once(' ')?;
    if name.is_empty() || name.chars().any(char::is_whitespace) {
        return None;
    }
    let rest = rest.strip_prefix('v')?;
    let (version, mut annotations) = rest.split_once(' ').map_or((rest, ""), |(v, a)| (v, a));
    if version.is_empty() {
        return None;
    }
    let all = annotations;
    // Each annotation is ` (…)` with no parenthesis inside.
    let mut first = true;
    while !annotations.is_empty() {
        let body = if first {
            annotations.strip_prefix('(')
        } else {
            annotations.strip_prefix(" (")
        }?;
        let close = body.find(')')?;
        if body[..close].contains('(') {
            return None;
        }
        annotations = &body[close + 1..];
        first = false;
    }
    Some((
        TreeEntry {
            name: name.to_owned(),
            version: version.to_owned(),
        },
        all,
    ))
}

/// Parses `cargo tree --prefix none -f {p}` output; duplicates dropped.
pub fn parse_tree(text: &str) -> Result<Vec<TreeEntry>> {
    let mut seen: Vec<TreeEntry> = Vec::new();
    let mut keys = HashSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (entry, _) = package_spec(trimmed)
            .ok_or_else(|| format!("cargo tree printed an unexpected line: {line:?}"))?;
        if keys.insert(entry.key()) {
            seen.push(entry);
        } else if let Some(at) = seen.iter().position(|seen| seen.key() == entry.key()) {
            // A JavaScript Map keeps the first position of a re-set key.
            seen[at] = entry;
        }
    }
    Ok(seen)
}

/// The indentation of a tree line: how many four-character units (`│   `,
/// four spaces, or a final `├── ` / `└── `) it starts with, the rest of the
/// line, and whether a branch ended it.
fn indentation(line: &str) -> (usize, &str, bool) {
    let mut depth = 0;
    let mut rest = line;
    loop {
        if let Some(next) = rest
            .strip_prefix("│   ")
            .or_else(|| rest.strip_prefix("    "))
        {
            depth += 1;
            rest = next;
            continue;
        }
        if let Some(next) = rest
            .strip_prefix("├── ")
            .or_else(|| rest.strip_prefix("└── "))
        {
            return (depth + 1, next, true);
        }
        return (depth, rest, false);
    }
}

/// The dependency edges no build compiles, `parent@version>child@version`:
/// those declared only under `cfg(any())`.
pub fn never_edges(metadata: &Value) -> HashSet<String> {
    let mut keys = HashMap::new();
    for package in metadata
        .get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(id), Some(name), Some(version)) = (
            package.get("id").and_then(Value::as_str),
            package.get("name").and_then(Value::as_str),
            package.get("version").and_then(Value::as_str),
        ) {
            keys.insert(id.to_owned(), format!("{name}@{version}"));
        }
    }
    let mut edges = HashSet::new();
    let nodes = metadata
        .get("resolve")
        .and_then(|resolve| resolve.get("nodes"))
        .and_then(Value::as_array);
    for node in nodes.into_iter().flatten() {
        let Some(parent) = node
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| keys.get(id))
        else {
            continue;
        };
        for dep in node
            .get("deps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let kinds = dep
                .get("dep_kinds")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if kinds.is_empty()
                || !kinds
                    .iter()
                    .all(|kind| kind.get("target").and_then(Value::as_str) == Some("cfg(any())"))
            {
                continue;
            }
            if let Some(child) = dep
                .get("pkg")
                .and_then(Value::as_str)
                .and_then(|id| keys.get(id))
            {
                edges.insert(format!("{parent}>{child}"));
            }
        }
    }
    edges
}

/// The crates one platform's build runs on the build host: those under a
/// `[build-dependencies]` heading or a proc macro, below a chain of crates
/// all in `linked`, less the `never` edges.
pub fn parse_host_side(
    text: &str,
    linked: &HashSet<String>,
    never: &HashSet<String>,
) -> Result<Vec<TreeEntry>> {
    let mut seen: Vec<TreeEntry> = Vec::new();
    let mut seen_keys = HashSet::new();
    let mut host: Vec<bool> = Vec::new();
    let mut active: Vec<bool> = Vec::new();
    let mut build: Vec<bool> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (depth, rest, branched) = indentation(line);
        if !branched
            && let Some(kind) = rest
                .strip_prefix('[')
                .and_then(|inner| inner.strip_suffix(']'))
            && matches!(
                kind,
                "dependencies" | "build-dependencies" | "dev-dependencies"
            )
        {
            if depth >= host.len() {
                return Err(format!(
                    "cargo tree printed a heading with no package: {line:?}"
                ));
            }
            build[depth] = kind == "build-dependencies";
            continue;
        }
        let (entry, annotations) = package_spec(rest)
            .ok_or_else(|| format!("cargo tree printed an unexpected line: {line:?}"))?;
        if depth > host.len() || (depth == 0 && !host.is_empty()) {
            return Err(format!(
                "cargo tree printed an unexpected nesting: {line:?}"
            ));
        }
        let key = entry.key();
        let is_host = depth > 0
            && (host[depth - 1] || build[depth - 1] || annotations.contains("(proc-macro)"));
        let is_active = if depth == 0 {
            linked.contains(&key)
        } else {
            active[depth - 1]
                && (is_host || linked.contains(&key))
                && !never.contains(&format!("{}>{key}", keys[depth - 1]))
        };
        host.truncate(depth);
        active.truncate(depth);
        build.truncate(depth);
        keys.truncate(depth);
        keys.push(key.clone());
        host.push(is_host);
        active.push(is_active);
        build.push(false);
        if is_host && is_active {
            if seen_keys.insert(key.clone()) {
                seen.push(entry);
            } else if let Some(at) = seen.iter().position(|seen| seen.key() == key) {
                seen[at] = entry;
            }
        }
    }
    Ok(seen)
}

// ---------------------------------------------------------------- inputs

/// Everything generation reads, gathered by [`collect`].
#[derive(Debug, Clone)]
pub struct Inputs {
    pub root: PathBuf,
    pub cargo_lock: Vec<u8>,
    pub config: String,
    pub upstream: Option<String>,
    pub channel: String,
    pub cargo: String,
    /// `cargo metadata --format-version 1`, with the resolve graph.
    pub metadata: Value,
    /// Each platform's linked tree, by platform id.
    pub trees: Vec<(String, String)>,
    pub host_tree: String,
    /// cargo-about's `--format json` output.
    pub about: String,
}

/// Gathers [`Inputs`]: the pinned toolchain and cargo-about, `cargo
/// metadata`, both trees for each platform, and cargo-about's JSON. Every
/// call is locked and offline, so run `cargo fetch --locked` first.
///
/// # Errors
///
/// When a tool is missing or not the pinned version, or a call fails.
pub fn collect(root: &Path, cargo_about: &str) -> Result<Inputs> {
    let channel = crate::workspace::toolchain(root)?;
    let cargo = run("cargo", &["--version"], root)?.trim().to_owned();
    if !cargo.starts_with(&format!("cargo {channel} ")) {
        return Err(format!(
            "the checkout selects {cargo:?}, not the pinned Rust {channel}; install it with rustup"
        ));
    }
    let about_version = run(cargo_about, &["--version"], root).map_err(|error| {
        format!(
            "cargo-about {CARGO_ABOUT_VERSION} is required ({error}). Install it with `cargo install cargo-about --version ={CARGO_ABOUT_VERSION} --locked --features cli`, or name it with BAKE_CARGO_ABOUT"
        )
    })?;
    if about_version.trim() != format!("cargo-about {CARGO_ABOUT_VERSION}") {
        return Err(format!(
            "{cargo_about} is {:?}; the bundle is generated with cargo-about {CARGO_ABOUT_VERSION}",
            about_version.trim()
        ));
    }
    let metadata: Value = serde_json::from_str(&run(
        "cargo",
        &["metadata", "--format-version", "1", "--locked", "--offline"],
        root,
    )?)
    .map_err(|error| format!("cargo metadata: {error}"))?;
    let tree = [
        "tree",
        "--locked",
        "--offline",
        "--charset",
        "utf8",
        "-p",
        ROOT_PACKAGE,
    ];
    let mut trees = Vec::new();
    for (id, triple) in PLATFORMS {
        let mut args: Vec<&str> = tree.to_vec();
        args.extend([
            "--target",
            triple,
            "-e",
            "normal,no-proc-macro",
            "--prefix",
            "none",
            "-f",
            "{p}",
        ]);
        trees.push((id.to_owned(), run("cargo", &args, root)?));
    }
    let mut args: Vec<&str> = tree.to_vec();
    args.extend([
        "--target",
        "all",
        "-e",
        "normal,build",
        "--no-dedupe",
        "-f",
        "{p}",
    ]);
    let host_tree = run("cargo", &args, root)?;
    let scratch = Scratch::new("bake-license-collect")?;
    let output = scratch.path().join("licenses.json");
    let manifest = format!("crates/{ROOT_PACKAGE}/Cargo.toml");
    let output_arg = output.display().to_string();
    run(
        cargo_about,
        &[
            "about",
            "generate",
            "--frozen",
            "--fail",
            "-c",
            "about.toml",
            "-m",
            &manifest,
            "--format",
            "json",
            "--output-file",
            &output_arg,
        ],
        root,
    )?;
    let about = read_text(&output)?;
    let upstream_path = root.join("licenses/upstream.toml");
    Ok(Inputs {
        root: root.to_path_buf(),
        cargo_lock: std::fs::read(root.join("Cargo.lock"))
            .map_err(|error| format!("Cargo.lock: {error}"))?,
        config: read_text(&root.join("about.toml"))?,
        upstream: if upstream_path.exists() {
            Some(read_text(&upstream_path)?)
        } else {
            None
        },
        channel,
        cargo,
        metadata,
        trees,
        host_tree,
        about,
    })
}

// ---------------------------------------------------------------- generation

#[derive(Debug, Clone)]
struct Package {
    key: String,
    name: String,
    version: String,
    source: Option<String>,
    license: Option<String>,
    dir: PathBuf,
    workspace: bool,
}

#[derive(Debug, Clone)]
struct Upstream {
    repository: String,
    commit: String,
    path: String,
    url: String,
    file: String,
}

#[derive(Debug, Clone)]
struct Text {
    id: String,
    license: String,
    sha256: String,
    body: String,
    source: String,
    users: BTreeSet<String>,
    upstream: Option<Upstream>,
}

#[derive(Debug, Clone)]
struct Notice {
    file: String,
    sha256: String,
    body: String,
}

#[derive(Debug, Clone)]
struct ThirdParty {
    pkg: Package,
    roles: BTreeMap<String, &'static str>,
    /// License text ids, each with the file of this crate that holds it,
    /// in insertion order.
    texts: Vec<(String, String)>,
    notices: Vec<Notice>,
    clarified: bool,
}

impl ThirdParty {
    fn set_text(&mut self, id: &str, file: String) {
        if let Some(at) = self.texts.iter().position(|(seen, _)| seen == id) {
            self.texts[at].1 = file;
        } else {
            self.texts.push((id.to_owned(), file));
        }
    }
}

/// The generated files, in order, by path relative to the bundle.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub files: Vec<(String, String)>,
    pub crates: usize,
    pub texts: usize,
    pub per_platform: BTreeMap<String, usize>,
    pub excluded: Vec<String>,
}

impl Bundle {
    pub fn file(&self, path: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|(name, _)| name == path)
            .map(|(_, text)| text.as_str())
    }
}

fn table<'a>(value: &'a toml::Value, what: &str) -> Result<&'a toml::Table> {
    value
        .as_table()
        .ok_or_else(|| format!("{what} is not a table"))
}

struct UpstreamText {
    file: String,
    sha256: String,
    license: String,
    repository: String,
    commit: String,
    path: String,
    url: String,
    crates: Vec<(String, String, String)>,
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn read_upstream(text: Option<&str>) -> Result<HashMap<String, std::rc::Rc<UpstreamText>>> {
    let mut by_crate = HashMap::new();
    let Some(text) = text else {
        return Ok(by_crate);
    };
    let parsed: toml::Table = text
        .parse()
        .map_err(|error| format!("upstream.toml does not parse: {error}"))?;
    let entries = match (
        parsed.len(),
        parsed.get("text").and_then(toml::Value::as_array),
    ) {
        (1, Some(entries)) => entries,
        _ => return Err("upstream.toml holds only a [[text]] list".to_owned()),
    };
    const KEYS: [&str; 8] = [
        "commit",
        "crates",
        "file",
        "license",
        "path",
        "repository",
        "sha256",
        "url",
    ];
    let mut problems = Vec::new();
    for (index, value) in entries.iter().enumerate() {
        let entry = table(value, &format!("upstream.toml text {index}"))?;
        let string = |key: &str| entry.get(key).and_then(toml::Value::as_str);
        let strings = KEYS
            .iter()
            .filter(|key| **key != "crates")
            .all(|key| string(key).is_some());
        let extra = entry.keys().any(|key| !KEYS.contains(&key.as_str()));
        let crates = entry.get("crates").and_then(toml::Value::as_array);
        let file = string("file").unwrap_or("");
        let repository = string("repository").unwrap_or("");
        let commit = string("commit").unwrap_or("");
        let path = string("path").unwrap_or("");
        let repo_ok = repository
            .strip_prefix("https://github.com/")
            .is_some_and(|rest| {
                let parts: Vec<&str> = rest.split('/').collect();
                parts.len() == 2
                    && parts.iter().all(|part| {
                        !part.is_empty()
                            && part
                                .chars()
                                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'))
                    })
            });
        let url = format!(
            "{}/{commit}/{path}",
            repository.replace("https://github.com/", "https://raw.githubusercontent.com/")
        );
        if !strings
            || extra
            || crates.is_none_or(Vec::is_empty)
            || !is_hex(string("sha256").unwrap_or(""), 64)
            || !is_hex(commit, 40)
            || !repo_ok
            || string("url") != Some(url.as_str())
            || Path::new(file).is_absolute()
            || file.split(['/', '\\']).any(|part| part == "..")
        {
            problems.push(format!(
                "upstream.toml text {index} needs file, sha256, license, a github.com repository, a full commit, path, the raw URL of that path at that commit, and crates, and nothing else"
            ));
            continue;
        }
        let mut pinned = Vec::new();
        for item in crates.into_iter().flatten() {
            let krate = table(item, &format!("upstream.toml text {index} crate"))?;
            let get = |key: &str| krate.get(key).and_then(toml::Value::as_str);
            let name_ok = get("crate").is_some_and(|value| {
                value.split_once('@').is_some_and(|(n, v)| {
                    !n.is_empty() && !v.is_empty() && !v.contains('@') && !value.contains(' ')
                })
            });
            if krate
                .keys()
                .any(|key| !["checksum", "crate", "path-in-vcs"].contains(&key.as_str()))
                || !name_ok
                || !get("checksum").is_some_and(|c| is_hex(c, 64))
                || get("path-in-vcs").is_none()
            {
                problems.push(format!(
                    "upstream.toml text {index} crates need crate (name@version), its crates.io checksum, and path-in-vcs"
                ));
                continue;
            }
            pinned.push((
                get("crate").unwrap_or("").to_owned(),
                get("checksum").unwrap_or("").to_owned(),
                get("path-in-vcs").unwrap_or("").to_owned(),
            ));
        }
        let upstream = std::rc::Rc::new(UpstreamText {
            file: file.to_owned(),
            sha256: string("sha256").unwrap_or("").to_owned(),
            license: string("license").unwrap_or("").to_owned(),
            repository: repository.to_owned(),
            commit: commit.to_owned(),
            path: path.to_owned(),
            url,
            crates: pinned.clone(),
        });
        for (krate, _, _) in pinned {
            if by_crate.contains_key(&krate) {
                problems.push(format!("upstream.toml names {krate} twice"));
            }
            by_crate.insert(krate, std::rc::Rc::clone(&upstream));
        }
    }
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    Ok(by_crate)
}

struct Clarification {
    license: String,
    files: Vec<(String, String)>,
}

fn read_config(text: &str) -> Result<Vec<(String, Clarification)>> {
    let parsed: toml::Table = text
        .parse()
        .map_err(|error| format!("about.toml does not parse: {error}"))?;
    let mut problems = Vec::new();
    let accepted = parsed.get("accepted").and_then(toml::Value::as_array);
    if !accepted.is_some_and(|list| !list.is_empty() && list.iter().all(toml::Value::is_str)) {
        problems.push("about.toml `accepted` must be a non-empty list of license ids".to_owned());
    }
    let triples: Vec<&str> = PLATFORMS.iter().map(|(_, triple)| *triple).collect();
    let targets: Option<Vec<&str>> = parsed
        .get("targets")
        .and_then(toml::Value::as_array)
        .map(|list| list.iter().filter_map(toml::Value::as_str).collect());
    if targets.as_deref() != Some(triples.as_slice()) {
        problems.push(format!(
            "about.toml `targets` must be exactly {}",
            serde_json::to_string(&triples).unwrap_or_default()
        ));
    }
    if parsed
        .get("ignore-dev-dependencies")
        .and_then(toml::Value::as_bool)
        != Some(true)
    {
        problems.push("about.toml must set `ignore-dev-dependencies = true`".to_owned());
    }
    if parsed
        .get("ignore-build-dependencies")
        .and_then(toml::Value::as_bool)
        != Some(false)
    {
        problems.push("about.toml must set `ignore-build-dependencies = false`".to_owned());
    }
    if parsed
        .get("private")
        .and_then(|private| private.get("ignore"))
        .and_then(toml::Value::as_bool)
        != Some(true)
    {
        problems.push("about.toml must set `private = { ignore = true }`".to_owned());
    }
    let mut clarify = Vec::new();
    for (key, value) in &parsed {
        if CONFIG_KEYS.contains(&key.as_str()) {
            continue;
        }
        let Some(entry) = value
            .as_table()
            .filter(|table| table.len() == 1)
            .and_then(|table| table.get("clarify"))
            .and_then(toml::Value::as_table)
        else {
            problems.push(format!(
                "about.toml `{key}` is not a setting this bundle allows; only per-crate `clarify` tables are (no workarounds or per-crate acceptance)"
            ));
            continue;
        };
        let license = entry.get("license").and_then(toml::Value::as_str);
        let files = entry.get("files").and_then(toml::Value::as_array);
        if license.is_none()
            || files.is_none_or(Vec::is_empty)
            || entry
                .keys()
                .any(|field| field != "license" && field != "files")
        {
            problems.push(format!(
                "about.toml `{key}.clarify` must have a license and hashed files, and nothing else (no git sources)"
            ));
            continue;
        }
        let mut checked = Vec::new();
        for file in files.into_iter().flatten() {
            let item = file.as_table();
            let path = item
                .and_then(|item| item.get("path"))
                .and_then(toml::Value::as_str);
            let checksum = item
                .and_then(|item| item.get("checksum"))
                .and_then(toml::Value::as_str);
            let whole =
                item.is_some_and(|item| !item.contains_key("start") && !item.contains_key("end"));
            match (path, checksum) {
                (Some(path), Some(checksum)) if is_hex(checksum, 64) && whole => {
                    checked.push((path.to_owned(), checksum.to_owned()));
                }
                _ => problems.push(format!(
                    "about.toml `{key}.clarify.files` entries need a path and a SHA-256 checksum, and take the whole file"
                )),
            }
        }
        clarify.push((
            key.clone(),
            Clarification {
                license: license.unwrap_or("").to_owned(),
                files: checked,
            },
        ));
    }
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    Ok(clarify)
}

struct AboutLicense {
    id: String,
    text: String,
    source_path: Option<String>,
    users: Vec<String>,
}

fn key_of(value: Option<&Value>, what: &str) -> Result<String> {
    let name = value.and_then(|v| v.get("name")).and_then(Value::as_str);
    let version = value.and_then(|v| v.get("version")).and_then(Value::as_str);
    match (name, version) {
        (Some(name), Some(version)) => Ok(format!("{name}@{version}")),
        _ => Err(format!("{what} has no crate name and version")),
    }
}

fn read_about(text: &str) -> Result<(HashSet<String>, Vec<AboutLicense>)> {
    let parsed: Value = serde_json::from_str(text)
        .map_err(|error| format!("cargo-about output is not JSON: {error}"))?;
    let (Some(crates), Some(licenses)) = (
        parsed.get("crates").and_then(Value::as_array),
        parsed.get("licenses").and_then(Value::as_array),
    ) else {
        return Err("cargo-about output has no crates and licenses lists".to_owned());
    };
    let mut keys = HashSet::new();
    for (index, entry) in crates.iter().enumerate() {
        keys.insert(key_of(
            entry.get("package"),
            &format!("cargo-about crate {index}"),
        )?);
    }
    let mut out = Vec::new();
    for (index, license) in licenses.iter().enumerate() {
        let (Some(id), Some(body), Some(used_by)) = (
            license.get("id").and_then(Value::as_str),
            license.get("text").and_then(Value::as_str),
            license.get("used_by").and_then(Value::as_array),
        ) else {
            return Err(format!(
                "cargo-about license {index} lacks an id, text, or users"
            ));
        };
        let mut users = Vec::new();
        for (at, user) in used_by.iter().enumerate() {
            users.push(key_of(
                user.get("crate"),
                &format!("cargo-about license {index} user {at}"),
            )?);
        }
        out.push(AboutLicense {
            id: id.to_owned(),
            text: body.to_owned(),
            source_path: license
                .get("source_path")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            users,
        });
    }
    Ok((keys, out))
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn relative(dir: &Path, path: &Path) -> Option<PathBuf> {
    path.strip_prefix(dir).ok().map(Path::to_path_buf)
}

/// The slash path, inside `dir`, of a file holding `text` verbatim:
/// `preferred` when it does, otherwise, unless `exact`, the first such file
/// in sorted order.
fn find_text(dir: &Path, preferred: &str, text: &str, exact: bool) -> Result<Option<String>> {
    let holds = |path: &Path| -> bool {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return false;
        };
        if !meta.is_file() || meta.len() > SEARCH_LIMIT {
            return false;
        }
        std::fs::read(path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .is_some_and(|body| body.contains(text))
    };
    let first = dir.join(preferred);
    if !crate::util::inside(dir, &first) || preferred.split(['/', '\\']).any(|part| part == "..") {
        return Err(format!("{preferred} is outside {}", dir.display()));
    }
    if !text.is_empty() && holds(&first) {
        return Ok(relative(dir, &first).map(|path| slash(&path)));
    }
    if exact || text.is_empty() {
        return Ok(None);
    }
    fn walk(dir: &Path, at: &Path, holds: &dyn Fn(&Path) -> bool) -> Option<String> {
        let mut entries: Vec<(String, std::fs::FileType, PathBuf)> = std::fs::read_dir(at)
            .ok()?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                Some((
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.file_type().ok()?,
                    entry.path(),
                ))
            })
            .collect();
        entries.sort_by(|a, b| locale_cmp(&a.0, &b.0));
        for (_, kind, path) in entries {
            let hit = if kind.is_dir() {
                walk(dir, &path, holds)
            } else if kind.is_file() && holds(&path) {
                relative(dir, &path).map(|path| slash(&path))
            } else {
                None
            };
            if hit.is_some() {
                return hit;
            }
        }
        None
    }
    Ok(walk(dir, dir, &holds))
}

/// The crates.io checksum of each locked package, by `name@version`.
fn lock_checksums(lock: &[u8]) -> HashMap<String, String> {
    let mut checksums = HashMap::new();
    let Ok(text) = std::str::from_utf8(lock) else {
        return checksums;
    };
    let Ok(parsed) = text.parse::<toml::Table>() else {
        return checksums;
    };
    for package in parsed
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        let get = |key: &str| package.get(key).and_then(toml::Value::as_str);
        if let (Some(name), Some(version), Some(checksum)) =
            (get("name"), get("version"), get("checksum"))
        {
            checksums.insert(format!("{name}@{version}"), checksum.to_owned());
        }
    }
    checksums
}

fn decode(bytes: Vec<u8>, what: &str) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| format!("{what} is not UTF-8 text"))
}

fn is_attribution_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["notice", "copyright", "copying"].iter().any(|stem| {
        lower == *stem
            || lower
                .strip_prefix(stem)
                .and_then(|rest| rest.chars().next())
                .is_some_and(|sep| matches!(sep, '.' | '_' | '-'))
                && !name.contains(['/', '\\'])
    })
}

fn sorted_names(dir: &Path) -> Option<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    Some(names)
}

/// The license files of the third-party artifacts a Bake crate embeds:
/// each `LICENSE-*` file in an immediate subdirectory, sorted by path.
fn read_embedded(dir: &Path, root: &Path, problems: &mut Vec<String>) -> Vec<Notice> {
    let mut licenses = Vec::new();
    for entry in sorted_names(dir).unwrap_or_default() {
        let sub = dir.join(&entry);
        if entry == "target" || !std::fs::symlink_metadata(&sub).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        for name in sorted_names(&sub).unwrap_or_default() {
            let path = sub.join(&name);
            let embedded = name
                .strip_prefix("LICENSE-")
                .is_some_and(|rest| !rest.is_empty());
            if !embedded || !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
                continue;
            }
            let file = relative(root, &path).map_or_else(|| slash(&path), |p| slash(&p));
            match std::fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| decode(bytes, &file))
            {
                Ok(body) => licenses.push(Notice {
                    file,
                    sha256: sha256(body.as_bytes()),
                    body,
                }),
                Err(error) => problems.push(error),
            }
        }
    }
    licenses
}

/// Builds the bundle from collected inputs, reading license and notice
/// files from the crates' source directories.
///
/// # Errors
///
/// Every problem, one per line: a crate missing from either side, a
/// license text not found verbatim in the crate's sources, a stale or broad
/// clarification, or an unreadable notice.
#[expect(clippy::too_many_lines, reason = "one pass, ported as it was written")]
pub fn build(inputs: &Inputs) -> Result<Bundle> {
    let clarifications = read_config(&inputs.config)?;
    let upstream = read_upstream(inputs.upstream.as_deref())?;
    let mut supplied: HashSet<String> = HashSet::new();
    let (about_crates, about_licenses) = read_about(&inputs.about)?;
    let mut problems: Vec<String> = Vec::new();

    let members: HashSet<&str> = inputs
        .metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut packages: HashMap<String, Package> = HashMap::new();
    for pkg in inputs
        .metadata
        .get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let get = |key: &str| pkg.get(key).and_then(Value::as_str);
        let (Some(name), Some(version), Some(id), Some(manifest)) =
            (get("name"), get("version"), get("id"), get("manifest_path"))
        else {
            continue;
        };
        let key = format!("{name}@{version}");
        if packages.contains_key(&key) {
            problems.push(format!(
                "cargo metadata lists {key} from two sources; the bundle cannot tell them apart"
            ));
            continue;
        }
        packages.insert(
            key.clone(),
            Package {
                key,
                name: name.to_owned(),
                version: version.to_owned(),
                source: get("source").map(ToOwned::to_owned),
                license: get("license").map(ToOwned::to_owned),
                dir: Path::new(manifest)
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default(),
                workspace: members.contains(id),
            },
        );
    }

    let mut third: BTreeMap<String, ThirdParty> = BTreeMap::new();
    let mut workspace: BTreeMap<String, (Package, BTreeSet<String>)> = BTreeMap::new();
    let mut per_platform: BTreeMap<String, usize> = BTreeMap::new();
    let never = never_edges(&inputs.metadata);
    let tree_ids: HashSet<&str> = inputs.trees.iter().map(|(id, _)| id.as_str()).collect();
    for (id, _) in PLATFORMS {
        if !tree_ids.contains(id) {
            problems.push(format!("no cargo tree for {id}"));
        }
    }
    for (id, tree) in &inputs.trees {
        let linked_entries = parse_tree(tree)?;
        let linked: HashSet<String> = linked_entries.iter().map(TreeEntry::key).collect();
        if linked_entries.first().map(|entry| entry.name.as_str()) != Some(ROOT_PACKAGE) {
            problems.push(format!("the {id} tree does not start at {ROOT_PACKAGE}"));
        }
        let host_side = parse_host_side(&inputs.host_tree, &linked, &never)?;
        let compiled: Vec<TreeEntry> = linked_entries
            .iter()
            .cloned()
            .chain(
                host_side
                    .into_iter()
                    .filter(|entry| !linked.contains(&entry.key())),
            )
            .collect();
        let mut count = 0;
        for entry in compiled {
            let key = entry.key();
            let Some(pkg) = packages.get(&key) else {
                problems.push(format!(
                    "{key} is in the {id} tree but not in cargo metadata"
                ));
                continue;
            };
            if pkg.workspace {
                workspace
                    .entry(key)
                    .or_insert_with(|| (pkg.clone(), BTreeSet::new()))
                    .1
                    .insert(id.clone());
                continue;
            }
            if pkg.source.as_deref() != Some(CRATES_IO) {
                problems.push(format!(
                    "{key} comes from {}; only crates.io sources are supported",
                    pkg.source.as_deref().unwrap_or("a path")
                ));
                continue;
            }
            count += 1;
            let role = if linked.contains(&key) {
                "runtime"
            } else {
                "build"
            };
            third
                .entry(key)
                .or_insert_with(|| ThirdParty {
                    pkg: pkg.clone(),
                    roles: BTreeMap::new(),
                    texts: Vec::new(),
                    notices: Vec::new(),
                    clarified: false,
                })
                .roles
                .insert(id.clone(), role);
        }
        per_platform.insert(id.clone(), count);
    }

    for key in third.keys() {
        if !about_crates.contains(key) {
            problems.push(format!(
                "{key} is compiled into bake but cargo-about did not report it"
            ));
        }
    }
    let mut excluded: Vec<String> = Vec::new();
    for key in &about_crates {
        if third.contains_key(key) || packages.get(key).is_some_and(|pkg| pkg.workspace) {
            continue;
        }
        if packages.contains_key(key) {
            excluded.push(key.clone());
        } else {
            problems.push(format!(
                "cargo-about reported {key}, which is not in the locked dependency graph"
            ));
        }
    }
    excluded.sort_by(|a, b| by_label(a, b));

    let checksums = lock_checksums(&inputs.cargo_lock);
    let mut texts: BTreeMap<String, Text> = BTreeMap::new();
    let mut missing: HashSet<String> = HashSet::new();
    for license in &about_licenses {
        let users: Vec<String> = license
            .users
            .iter()
            .filter(|user| third.contains_key(*user))
            .cloned()
            .collect();
        if users.is_empty() {
            continue;
        }
        let Some(source_path) = &license.source_path else {
            let uncovered: Vec<&String> = users
                .iter()
                .filter(|user| !upstream.contains_key(*user))
                .collect();
            for user in &uncovered {
                missing.insert((*user).clone());
            }
            if !uncovered.is_empty() {
                problems.push(format!(
                    "{}: no {} license text was found in the source package; cargo-about would substitute the generic text, losing the crate's copyright. Add a reviewed, hashed clarification naming the crate's own file, or, when the published crate omits it, a reviewed upstream text in licenses/upstream.toml",
                    uncovered.iter().map(|user| user.as_str()).collect::<Vec<_>>().join(", "),
                    license.id
                ));
            }
            for user in users.iter().filter(|user| upstream.contains_key(*user)) {
                supplied.insert(user.clone());
                let pin = &upstream[user];
                let Some(text) =
                    supply_upstream(&third[user].pkg, pin, inputs, &checksums, &mut problems)
                else {
                    missing.insert(user.clone());
                    continue;
                };
                let id = text.id.clone();
                let path = text
                    .upstream
                    .as_ref()
                    .map_or_else(String::new, |up| up.path.clone());
                texts
                    .entry(id.clone())
                    .or_insert(text)
                    .users
                    .insert(user.clone());
                if let Some(entry) = third.get_mut(user) {
                    entry.set_text(&id, format!("{path} (upstream)"));
                }
            }
            continue;
        };
        let absolute = Path::new(source_path).is_absolute();
        let owner = if absolute {
            users
                .iter()
                .map(|user| &third[user])
                .find(|entry| crate::util::inside(&entry.pkg.dir, Path::new(source_path)))
                .map(|entry| entry.pkg.dir.clone())
        } else {
            None
        };
        if absolute && owner.is_none() {
            for user in &users {
                missing.insert(user.clone());
            }
            problems.push(format!(
                "{}: the {} text was read from outside the crates using it",
                users.join(", "),
                license.id
            ));
            continue;
        }
        let preferred = owner.as_ref().map_or_else(
            || source_path.clone(),
            |dir| relative(dir, Path::new(source_path)).map_or_else(String::new, |p| slash(&p)),
        );
        let mut found: BTreeMap<String, String> = BTreeMap::new();
        for user in &users {
            let entry = &third[user];
            match find_text(&entry.pkg.dir, &preferred, &license.text, owner.is_none()) {
                Ok(Some(file)) => {
                    found.insert(user.clone(), file);
                }
                Ok(None) => problems.push(format!(
                    "{user}: the {} text cargo-about attached is not verbatim in {}",
                    license.id,
                    if owner.is_none() {
                        preferred.clone()
                    } else {
                        "any file of its source package".to_owned()
                    }
                )),
                Err(error) => problems.push(error),
            }
        }
        if found.len() != users.len() {
            for user in &users {
                missing.insert(user.clone());
            }
            continue;
        }
        let mut found_keys: Vec<&String> = found.keys().collect();
        found_keys.sort_by(|a, b| by_label(a, b));
        let first = found_keys[0].clone();
        let digest = sha256(license.text.as_bytes());
        let id = format!("{}-{}", license.id.replace(' ', "_"), &digest[..12]);
        let source = format!("{}, {}", label(&first), found[&first]);
        match texts.get_mut(&id) {
            None => {
                texts.insert(
                    id.clone(),
                    Text {
                        id: id.clone(),
                        license: license.id.clone(),
                        sha256: digest,
                        body: license.text.clone(),
                        source,
                        users: BTreeSet::new(),
                        upstream: None,
                    },
                );
            }
            Some(existing) if source < existing.source => existing.source = source,
            Some(_) => {}
        }
        for user in &users {
            if let Some(text) = texts.get_mut(&id) {
                text.users.insert(user.clone());
            }
            if let Some(entry) = third.get_mut(user) {
                entry.set_text(&id, found[user].clone());
            }
        }
    }

    for entry in third.values() {
        if entry.texts.is_empty()
            && about_crates.contains(&entry.pkg.key)
            && !missing.contains(&entry.pkg.key)
        {
            problems.push(format!(
                "{}: cargo-about attached no license text",
                entry.pkg.key
            ));
        }
    }
    let mut pinned: Vec<&String> = upstream.keys().collect();
    pinned.sort();
    for krate in pinned {
        if supplied.contains(krate) {
            continue;
        }
        problems.push(if third.contains_key(krate) {
            format!("upstream.toml supplies {krate}, whose own package now holds its license text; remove the entry")
        } else {
            format!("upstream.toml supplies {krate}, which no release platform compiles; remove the stale entry")
        });
    }

    for (name, clarification) in &clarifications {
        let matches: Vec<String> = third
            .values()
            .filter(|entry| &entry.pkg.name == name)
            .map(|entry| entry.pkg.key.clone())
            .collect();
        if matches.is_empty() {
            problems.push(format!(
                "about.toml clarifies {name}, which no release platform compiles; remove the stale clarification"
            ));
        }
        for key in matches {
            let Some(entry) = third.get_mut(&key) else {
                continue;
            };
            entry.clarified = true;
            if Some(&clarification.license) != entry.pkg.license.as_ref() {
                problems.push(format!(
                    "about.toml clarifies {} as {:?}, but it declares {:?}; a clarification may locate texts, not change the license",
                    entry.pkg.key,
                    clarification.license,
                    entry.pkg.license.as_deref().unwrap_or("null")
                ));
            }
            for (path, checksum) in &clarification.files {
                let full = entry.pkg.dir.join(path);
                if !crate::util::inside(&entry.pkg.dir, &full)
                    || path.split(['/', '\\']).any(|p| p == "..")
                {
                    problems.push(format!(
                        "about.toml clarifies {} with {path}, outside its package",
                        entry.pkg.key
                    ));
                    continue;
                }
                let actual = std::fs::read(&full)
                    .map_or_else(|_| "missing".to_owned(), |bytes| sha256(&bytes));
                if &actual != checksum {
                    problems.push(format!(
                        "about.toml clarifies {} {path} at {checksum}, but the locked source has {actual}; review the file and update the clarification",
                        entry.pkg.key
                    ));
                }
            }
        }
    }

    for entry in third.values_mut() {
        let Some(names) = sorted_names(&entry.pkg.dir) else {
            problems.push(format!(
                "{}: its source directory is missing; run `cargo fetch --locked` first",
                entry.pkg.key
            ));
            continue;
        };
        for name in names.into_iter().filter(|name| is_attribution_file(name)) {
            let path = entry.pkg.dir.join(&name);
            if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
                continue;
            }
            match std::fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| decode(bytes, &format!("{} {name}", entry.pkg.key)))
            {
                Ok(body) => entry.notices.push(Notice {
                    file: name,
                    sha256: sha256(body.as_bytes()),
                    body,
                }),
                Err(error) => problems.push(error),
            }
        }
    }

    let mut workspace_notices: HashMap<String, Option<Notice>> = HashMap::new();
    let mut embedded: HashMap<String, Vec<Notice>> = HashMap::new();
    for (pkg, _) in workspace.values() {
        if pkg.license.as_deref() != Some("MIT") {
            problems.push(format!(
                "{} is a Bake crate but declares {:?}, not the repository's MIT",
                pkg.key,
                pkg.license.as_deref().unwrap_or("null")
            ));
        }
        embedded.insert(
            pkg.key.clone(),
            read_embedded(&pkg.dir, &inputs.root, &mut problems),
        );
        let path = pkg.dir.join("NOTICE");
        if !path.exists() {
            workspace_notices.insert(pkg.key.clone(), None);
            continue;
        }
        let file = relative(&inputs.root, &path).map_or_else(|| slash(&path), |p| slash(&p));
        match std::fs::read(&path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| decode(bytes, &file))
        {
            Ok(body) => {
                workspace_notices.insert(
                    pkg.key.clone(),
                    Some(Notice {
                        file,
                        sha256: sha256(body.as_bytes()),
                        body,
                    }),
                );
            }
            Err(error) => problems.push(error),
        }
    }

    if !problems.is_empty() {
        let mut seen = HashSet::new();
        problems.retain(|problem| seen.insert(problem.clone()));
        return Err(problems.join("\n"));
    }

    let by_key = |a: &Package, b: &Package| {
        locale_cmp(&a.name, &b.name).then_with(|| compare_versions(&a.version, &b.version))
    };
    let mut crates: Vec<&ThirdParty> = third.values().collect();
    crates.sort_by(|a, b| by_key(&a.pkg, &b.pkg));
    let mut members: Vec<&(Package, BTreeSet<String>)> = workspace.values().collect();
    members.sort_by(|a, b| by_key(&a.0, &b.0));
    let mut text_list: Vec<&Text> = texts.values().collect();
    text_list.sort_by(|a, b| locale_cmp(&a.id, &b.id));

    let lock_digest = sha256(&inputs.cargo_lock);
    let config_digest = sha256(inputs.config.as_bytes());
    let mut files = Vec::new();
    let mut platform_files = Vec::new();
    for (id, triple) in PLATFORMS {
        let path = format!("{id}/{BUNDLE_FILE}");
        let own: Vec<&ThirdParty> = crates
            .iter()
            .copied()
            .filter(|entry| entry.roles.contains_key(id))
            .collect();
        let bake: Vec<(&Package, Option<&Notice>, &[Notice])> = members
            .iter()
            .filter(|(_, platforms)| platforms.contains(id))
            .map(|(pkg, _)| {
                (
                    pkg,
                    workspace_notices.get(&pkg.key).and_then(Option::as_ref),
                    embedded.get(&pkg.key).map_or(&[][..], Vec::as_slice),
                )
            })
            .collect();
        let text = render(
            id,
            triple,
            &own,
            &text_list,
            &bake,
            &lock_digest,
            &config_digest,
            inputs,
        );
        platform_files.push(json!({
            "id": id,
            "triple": triple,
            "file": path,
            "sha256": sha256(text.as_bytes()),
            "crates": per_platform.get(id).copied().unwrap_or(0),
        }));
        files.push((path, text));
    }

    let mut input_files = Map::new();
    input_files.insert(
        "cargoLock".to_owned(),
        json!({"path": "Cargo.lock", "sha256": lock_digest}),
    );
    input_files.insert(
        "config".to_owned(),
        json!({"path": "about.toml", "sha256": config_digest}),
    );
    if let Some(upstream) = &inputs.upstream {
        input_files.insert(
            "upstream".to_owned(),
            json!({"path": "licenses/upstream.toml", "sha256": sha256(upstream.as_bytes())}),
        );
    }
    for text in &text_list {
        if let Some(origin) = &text.upstream {
            input_files.insert(
                origin.file.clone(),
                json!({"path": origin.file, "sha256": text.sha256}),
            );
        }
    }
    let platform_ids: Vec<&str> = PLATFORMS.iter().map(|(id, _)| *id).collect();
    let inventory = json!({
        "schema": SCHEMA,
        "generator": {
            "tool": format!("cargo-about {CARGO_ABOUT_VERSION}"),
            "cargo": inputs.cargo,
            "toolchain": inputs.channel,
        },
        "inputs": Value::Object(input_files),
        "selection": {
            "package": ROOT_PACKAGE,
            "runtime": "cargo tree --target <triple> -e normal,no-proc-macro",
            "build": "crates below build-dependency and proc-macro edges of the linked crates in cargo tree --target all -e normal,build --no-dedupe, for every build host",
            "excluded": "dev-dependencies",
        },
        "platforms": platform_files,
        "workspace": members.iter().map(|(pkg, platforms)| {
            let notice = workspace_notices.get(&pkg.key).and_then(Option::as_ref);
            json!({
                "name": pkg.name,
                "version": pkg.version,
                "license": pkg.license,
                "platforms": platform_ids.iter().filter(|id| platforms.contains(**id)).collect::<Vec<_>>(),
                "notice": notice.map(|notice| json!({"file": notice.file, "sha256": notice.sha256})),
                "embedded": embedded.get(&pkg.key).into_iter().flatten()
                    .map(|license| json!({"file": license.file, "sha256": license.sha256}))
                    .collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "crates": crates.iter().map(|entry| {
            let mut roles = Map::new();
            for id in &platform_ids {
                if let Some(role) = entry.roles.get(*id) {
                    roles.insert((*id).to_owned(), Value::String((*role).to_owned()));
                }
            }
            let mut texts = entry.texts.clone();
            texts.sort_by(|a, b| locale_cmp(&a.0, &b.0));
            json!({
                "name": entry.pkg.name,
                "version": entry.pkg.version,
                "source": entry.pkg.source,
                "license": entry.pkg.license,
                "clarified": entry.clarified,
                "platforms": Value::Object(roles),
                "texts": texts.iter().map(|(id, file)| json!({"id": id, "file": file})).collect::<Vec<_>>(),
                "notices": entry.notices.iter().map(|notice| json!({"file": notice.file, "sha256": notice.sha256})).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "excluded": excluded,
        "texts": text_list.iter().map(|text| {
            let mut entry = json!({
                "id": text.id,
                "license": text.license,
                "sha256": text.sha256,
                "source": text.source,
            });
            if let (Some(origin), Some(object)) = (&text.upstream, entry.as_object_mut()) {
                object.insert("upstream".to_owned(), json!({
                    "repository": origin.repository,
                    "commit": origin.commit,
                    "path": origin.path,
                    "url": origin.url,
                    "file": origin.file,
                }));
            }
            entry
        }).collect::<Vec<_>>(),
    });
    let inventory = serde_json::to_string_pretty(&inventory)
        .map_err(|error| format!("the inventory: {error}"))?;
    files.push((INVENTORY_FILE.to_owned(), format!("{inventory}\n")));
    Ok(Bundle {
        files,
        crates: crates.len(),
        texts: text_list.len(),
        per_platform,
        excluded,
    })
}

/// The reviewed upstream text for a crate whose package omits it, once the
/// crate is shown to be the locked crates.io package built from that commit
/// and path, declaring that license, and the vendored file matches.
fn supply_upstream(
    pkg: &Package,
    text: &UpstreamText,
    inputs: &Inputs,
    checksums: &HashMap<String, String>,
    problems: &mut Vec<String>,
) -> Option<Text> {
    let before = problems.len();
    let pin = text.crates.iter().find(|(krate, _, _)| krate == &pkg.key)?;
    let locked = checksums.get(&pkg.key);
    if locked != Some(&pin.1) {
        problems.push(format!(
            "upstream.toml pins {} at checksum {}, but Cargo.lock has {}",
            pkg.key,
            pin.1,
            locked.map_or("none", String::as_str)
        ));
    }
    let vcs: Value = std::fs::read(pkg.dir.join(".cargo_vcs_info.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| {
            problems.push(format!(
                "{} has no readable .cargo_vcs_info.json to tie it to {} at {}",
                pkg.key, text.repository, text.commit
            ));
            Value::Null
        });
    let sha1 = vcs
        .get("git")
        .and_then(|git| git.get("sha1"))
        .and_then(Value::as_str);
    let path_in_vcs = vcs.get("path_in_vcs").and_then(Value::as_str);
    if sha1 != Some(text.commit.as_str()) || path_in_vcs != Some(pin.2.as_str()) {
        problems.push(format!(
            "{} was published from {} at {}, not {} at {:?} as upstream.toml says",
            pkg.key,
            sha1.unwrap_or("undefined"),
            path_in_vcs.map_or_else(|| "undefined".to_owned(), |p| format!("{p:?}")),
            text.commit,
            pin.2
        ));
    }
    if pkg.license.as_deref() != Some(text.license.as_str()) {
        problems.push(format!(
            "upstream.toml gives {} {:?}, but it declares {:?}",
            pkg.key,
            text.license,
            pkg.license.as_deref().unwrap_or("null")
        ));
    }
    let path = inputs.root.join("licenses").join(&text.file);
    let mut body = None;
    match std::fs::read(&path) {
        Ok(bytes) => {
            let digest = sha256(&bytes);
            if digest == text.sha256 {
                match decode(bytes, &format!("licenses/{}", text.file)) {
                    Ok(decoded) => body = Some(decoded),
                    Err(error) => problems.push(error),
                }
            } else {
                problems.push(format!(
                    "licenses/{} has sha256 {digest}, not the reviewed {}",
                    text.file, text.sha256
                ));
            }
        }
        Err(_) => problems.push(format!("licenses/{} is missing", text.file)),
    }
    if problems.len() > before {
        return None;
    }
    let body = body?;
    Some(Text {
        id: format!("{}-{}", text.license.replace(' ', "_"), &text.sha256[..12]),
        license: text.license.clone(),
        sha256: sha256(body.as_bytes()),
        body,
        source: format!(
            "{} at {}, {} (the published crates omit it)",
            text.repository, text.commit, text.path
        ),
        users: BTreeSet::new(),
        upstream: Some(Upstream {
            repository: text.repository.clone(),
            commit: text.commit.clone(),
            path: text.path.clone(),
            url: text.url.clone(),
            file: format!("licenses/{}", text.file),
        }),
    })
}

fn chomp(body: &str) -> &str {
    body.strip_suffix('\n').unwrap_or(body)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the rendering's inputs, as built"
)]
fn render(
    id: &str,
    triple: &str,
    crates: &[&ThirdParty],
    all_texts: &[&Text],
    members: &[(&Package, Option<&Notice>, &[Notice])],
    lock_digest: &str,
    config_digest: &str,
    inputs: &Inputs,
) -> String {
    let used: HashSet<&str> = crates
        .iter()
        .flat_map(|entry| entry.texts.iter().map(|(id, _)| id.as_str()))
        .collect();
    let texts: Vec<&&Text> = all_texts
        .iter()
        .filter(|text| used.contains(text.id.as_str()))
        .collect();
    let keys: HashSet<&str> = crates.iter().map(|entry| entry.pkg.key.as_str()).collect();
    let mut lines: Vec<String> = vec![
        format!("Third-party licenses for bake, {id} ({triple})"),
        String::new(),
        "Bake is MIT-licensed (see LICENSE). This file reproduces, verbatim, the license"
            .to_owned(),
        "texts and notice files found in the source packages of the Rust crates compiled"
            .to_owned(),
        "into bake for this platform, and the NOTICE files of Bake's own crates with".to_owned(),
        "the licenses of the third-party artifacts they embed.".to_owned(),
        "Each crate remains under its own license.".to_owned(),
        String::new(),
        format!(
            "Generated by bake-release (crates/bake-release) with cargo-about {CARGO_ABOUT_VERSION}"
        ),
        format!(
            "and Rust {} from Cargo.lock (sha256 {lock_digest})",
            inputs.channel
        ),
        format!("and about.toml (sha256 {config_digest})."),
        format!(
            "Selection: \"runtime\" crates are the dependencies of {ROOT_PACKAGE} linked into the"
        ),
        format!("binary for {triple}. \"build\" crates are build scripts, proc macros, and"),
        "their dependencies, which only run while it is built; they are listed for every"
            .to_owned(),
        "build host, so this list can name some that this platform's build does not use."
            .to_owned(),
        String::new(),
        rule(),
        format!("1. Crates ({})", crates.len()),
        rule(),
        String::new(),
    ];
    for entry in crates {
        let notices = if entry.notices.is_empty() {
            String::new()
        } else {
            format!(
                " | notices: {}",
                entry
                    .notices
                    .iter()
                    .map(|notice| notice.file.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let mut ids: Vec<&str> = entry.texts.iter().map(|(id, _)| id.as_str()).collect();
        // A plain JavaScript sort: UTF-16 code units, which is byte order here.
        ids.sort_unstable();
        lines.push(format!(
            "{} {} | {} | {} | {}{notices}",
            entry.pkg.name,
            entry.pkg.version,
            entry.pkg.license.as_deref().unwrap_or("null"),
            entry.roles.get(id).copied().unwrap_or(""),
            ids.join(", ")
        ));
    }
    lines.extend([
        String::new(),
        rule(),
        format!("2. License texts ({})", texts.len()),
        rule(),
    ]);
    for text in &texts {
        let mut users: Vec<&String> = text
            .users
            .iter()
            .filter(|user| keys.contains(user.as_str()))
            .collect();
        users.sort_by(|a, b| by_label(a, b));
        lines.extend([
            String::new(),
            rule(),
            format!("[{}] {}", text.id, text.license),
            format!("Source: {}", text.source),
            format!(
                "Used by: {}",
                users
                    .iter()
                    .map(|user| label(user))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            thin_rule(),
            chomp(&text.body).to_owned(),
        ]);
    }
    let third_notices: Vec<(String, &Notice)> = crates
        .iter()
        .flat_map(|entry| {
            entry.notices.iter().map(move |notice| {
                (
                    format!("{} {}, {}", entry.pkg.name, entry.pkg.version, notice.file),
                    notice,
                )
            })
        })
        .collect();
    let bake_notices: Vec<(String, &Notice)> = members
        .iter()
        .flat_map(|(pkg, notice, embedded)| {
            notice
                .iter()
                .copied()
                .chain(embedded.iter())
                .map(move |notice| (format!("{} (Bake), {}", pkg.name, notice.file), notice))
        })
        .collect();
    lines.extend([
        String::new(),
        rule(),
        format!(
            "3. Notice files ({})",
            bake_notices.len() + third_notices.len()
        ),
        rule(),
    ]);
    for (title, notice) in bake_notices.iter().chain(third_notices.iter()) {
        lines.extend([
            String::new(),
            rule(),
            title.clone(),
            thin_rule(),
            chomp(&notice.body).to_owned(),
        ]);
    }
    format!("{}\n", lines.join("\n"))
}

/// Fetches the locked crates, then gathers and builds the bundle.
pub fn generate(root: &Path) -> Result<Bundle> {
    run("cargo", &["fetch", "--locked"], root)?;
    let cargo_about =
        std::env::var("BAKE_CARGO_ABOUT").unwrap_or_else(|_| "cargo-about".to_owned());
    build(&collect(root, &cargo_about)?)
}

/// Writes `bundle` into `out`, which must be absent or empty.
pub fn write(bundle: &Bundle, out: &Path) -> Result<()> {
    if out.exists()
        && std::fs::read_dir(out)
            .map_err(|error| format!("{}: {error}", out.display()))?
            .next()
            .is_some()
    {
        return Err(format!(
            "{} is not empty; the license bundle is written into an empty directory",
            out.display()
        ));
    }
    for (path, text) in &bundle.files {
        let file = out.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        std::fs::write(&file, text).map_err(|error| format!("{}: {error}", file.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sort_as_locale_compare_does() {
        let mut names = vec![
            "windows_x86_64_gnu",
            "windows-sys",
            "windows",
            "Windows",
            "wasm-bindgen",
            "wasm_bindgen",
            "wasm2",
        ];
        names.sort_by(|a, b| locale_cmp(a, b));
        assert_eq!(
            names,
            [
                "wasm_bindgen",
                "wasm-bindgen",
                "wasm2",
                "windows",
                "Windows",
                "windows_x86_64_gnu",
                "windows-sys"
            ]
        );
        assert_eq!(compare_versions("0.10.0", "0.9.1"), Ordering::Greater);
        assert_eq!(by_label("a@1.0.0", "a@0.9.0"), Ordering::Greater);
    }

    #[test]
    fn trees_parse_as_cargo_prints_them() {
        let linked = parse_tree("bake-cli v0.4.2 (/r/crates/bake-cli)\nserde v1.0.0\nserde v1.0.0\nfoo v0.1.0 (proc-macro) (*)\n").expect("tree");
        assert_eq!(
            linked.iter().map(TreeEntry::key).collect::<Vec<_>>(),
            ["bake-cli@0.4.2", "serde@1.0.0", "foo@0.1.0"]
        );
        assert!(parse_tree("not a package line here\n").is_err());
        let host = "\
bake-cli v0.4.2 (/r)
├── serde v1.0.0
│   └── serde_derive v1.0.0 (proc-macro)
│       └── syn v2.0.0
│   [build-dependencies]
│   └── cc v1.0.0
└── windows v0.1.0
    [build-dependencies]
    └── winres v0.1.0
";
        let linked: HashSet<String> = ["bake-cli@0.4.2", "serde@1.0.0"]
            .map(String::from)
            .into_iter()
            .collect();
        let side = parse_host_side(host, &linked, &HashSet::new()).expect("host");
        assert_eq!(
            side.iter().map(TreeEntry::key).collect::<Vec<_>>(),
            ["serde_derive@1.0.0", "syn@2.0.0", "cc@1.0.0"],
            "windows is not linked here, so its build dependency is not built"
        );
        let never: HashSet<String> = ["serde@1.0.0>serde_derive@1.0.0".to_owned()]
            .into_iter()
            .collect();
        let side = parse_host_side(host, &linked, &never).expect("host");
        assert_eq!(
            side.iter().map(TreeEntry::key).collect::<Vec<_>>(),
            ["cc@1.0.0"]
        );
    }

    #[test]
    fn attribution_files_are_notices_copyrights_and_copying() {
        for name in [
            "NOTICE",
            "notice.txt",
            "COPYRIGHT",
            "Copying-LGPL",
            "NOTICE_x",
        ] {
            assert!(is_attribution_file(name), "{name}");
        }
        for name in ["NOTICES", "LICENSE", "copyrights.md", "noticeboard"] {
            assert!(!is_attribution_file(name), "{name}");
        }
    }

    #[test]
    fn the_committed_policy_and_upstream_texts_read() {
        let root = crate::util::repository_root();
        let config = read_text(&root.join("about.toml")).expect("about.toml");
        assert!(!read_config(&config).expect("policy").is_empty());
        let upstream = read_text(&root.join("licenses/upstream.toml")).expect("upstream.toml");
        assert!(!read_upstream(Some(&upstream)).expect("upstream").is_empty());
        assert!(read_config("accepted = []\n").is_err());
        assert!(read_config(&format!("{config}\n[serde]\nworkarounds = []\n")).is_err());
    }
}
