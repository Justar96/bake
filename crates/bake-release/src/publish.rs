//! `bake-release publish`: the release steps after the archives exist.
//!
//! In order: tag `v<version>` on `origin/main` and push it, create the
//! version's GitHub release with the archives, assemble and sign the
//! manifest, attach the manifest and the installers to the release, run the
//! deploy's own gate, deploy the download service, and check what it
//! serves. Each step first looks at what already happened and skips what is
//! done, so a failed run is run again, or resumed with `--from <step>`. A
//! step never replaces what a release already published: an asset with
//! other bytes, a tag at another commit, or a draft release stops the run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bake_update::manifest::{RELEASE_TARGETS, archive_name, parse_manifest};
use serde_json::Value;

use crate::assemble::source_dir;
use crate::runner::{Command, Runner};
use crate::util::{Result, Scratch, read_text, sha256_file};
use crate::workspace::{changelog_section, is_stable, version_in};

/// The steps, in the order they run.
pub const STEPS: [&str; 7] = [
    "tag",
    "gh-release",
    "assemble",
    "upload",
    "verify",
    "deploy",
    "verify-live",
];

/// The download service's directory.
const HOST: &str = "distribution/host";

/// What [`publish`] is asked to do.
#[derive(Debug, Clone)]
pub struct Options {
    pub root: PathBuf,
    pub version: String,
    /// The native release signing key, PKCS#8 PEM.
    pub key_file: PathBuf,
    /// Skip `assemble`'s live channel check, for a release that rotates
    /// the native key.
    pub offline: bool,
    pub dry_run: bool,
    /// The index in [`STEPS`] of the first step to run.
    pub from: usize,
    /// This executable, which runs `assemble` and `verify-live`.
    pub exe: PathBuf,
    /// The download service, `BAKE_RELEASE_BASE_URL`.
    pub base: String,
}

/// The index in [`STEPS`] of `name`.
pub fn step_index(name: &str) -> Result<usize> {
    STEPS
        .iter()
        .position(|step| *step == name)
        .ok_or_else(|| format!("No step {name}; the steps are {}", STEPS.join(", ")))
}

/// The archive of every release platform, which must all be packed.
pub fn archives(root: &Path, version: &str) -> Result<Vec<PathBuf>> {
    let dir = source_dir(root, version);
    let paths: Vec<PathBuf> = RELEASE_TARGETS
        .iter()
        .map(|target| dir.join(archive_name(version, target)))
        .collect();
    let missing: Vec<String> = paths
        .iter()
        .filter(|path| !path.is_file())
        .map(|path| path.display().to_string())
        .collect();
    if missing.is_empty() {
        Ok(paths)
    } else {
        Err(format!(
            "Missing {}; pack the release on each platform first",
            missing.join(" and ")
        ))
    }
}

/// A GitHub release as `gh release view --json isDraft,assets` shows it.
#[derive(Debug, PartialEq, Eq)]
pub struct Release {
    pub draft: bool,
    /// Each asset's SHA-256, lowercase hex, when GitHub reports it.
    pub assets: BTreeMap<String, Option<String>>,
}

/// Reads `gh release view --json isDraft,assets`.
pub fn parse_release(json: &str) -> Result<Release> {
    let value: Value = serde_json::from_str(json)
        .map_err(|error| format!("gh printed a release that is not JSON: {error}"))?;
    let draft = value
        .get("isDraft")
        .and_then(Value::as_bool)
        .ok_or_else(|| "gh printed a release without isDraft".to_owned())?;
    let mut assets = BTreeMap::new();
    for asset in value
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| "gh printed a release without assets".to_owned())?
    {
        let name = asset
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "gh printed a release asset without a name".to_owned())?;
        let digest = asset
            .get("digest")
            .and_then(Value::as_str)
            .and_then(|digest| digest.strip_prefix("sha256:"))
            .map(str::to_ascii_lowercase);
        assets.insert(name.to_owned(), digest);
    }
    Ok(Release { draft, assets })
}

/// The commit `git ls-remote` lists for `tag`, peeled when it is annotated.
pub fn remote_tag_commit(listing: &str, tag: &str) -> Option<String> {
    let name = format!("refs/tags/{tag}");
    let peeled = format!("{name}^{{}}");
    let mut direct = None;
    for line in listing.lines() {
        let Some((sha, reference)) = line.split_once('\t') else {
            continue;
        };
        if reference == peeled {
            return Some(sha.trim().to_owned());
        }
        if reference == name {
            direct = Some(sha.trim().to_owned());
        }
    }
    direct
}

/// How a release asset compares with the local file.
#[derive(Debug, PartialEq, Eq)]
enum Asset {
    Missing,
    Same,
    Different,
}

fn compare(release: &Release, name: &str, sha256: &str) -> Asset {
    match release.assets.get(name) {
        None => Asset::Missing,
        // An asset GitHub reports no digest for is taken as uploaded.
        Some(None) => Asset::Same,
        Some(Some(digest)) if digest == sha256 => Asset::Same,
        Some(Some(_)) => Asset::Different,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

struct Run<'a> {
    options: &'a Options,
    runner: &'a mut dyn Runner,
    archives: Vec<PathBuf>,
    tag: String,
    scratch: Scratch,
}

impl Run<'_> {
    fn root(&self) -> &Path {
        &self.options.root
    }

    fn public(&self) -> PathBuf {
        self.root().join(HOST).join("public/native")
    }

    fn command<S: AsRef<str>>(&self, program: &str, args: &[S]) -> Command {
        Command::new(program, args, self.root())
    }

    fn say(&mut self, line: &str) {
        self.runner.say(line);
    }

    fn query(&mut self, command: &Command) -> Result<Option<String>> {
        self.runner.query(command)
    }

    fn effect(&mut self, command: &Command) -> Result<()> {
        if self.options.dry_run {
            self.runner.say(&format!("would run: {command}"));
            Ok(())
        } else {
            self.runner.say(&format!("$ {command}"));
            self.runner.effect(command)
        }
    }

    fn release(&mut self) -> Result<Option<Release>> {
        let tag = self.tag.clone();
        let view = self.command("gh", &["release", "view", &tag, "--json", "isDraft,assets"]);
        self.query(&view)?.as_deref().map(parse_release).transpose()
    }

    fn step(&mut self, name: &str) -> Result<()> {
        match name {
            "tag" => self.tag(),
            "gh-release" => self.gh_release(),
            "assemble" => self.assemble(),
            "upload" => self.upload(),
            "verify" => {
                let gate = self.command(
                    "node",
                    &[
                        "distribution/host/verify-native-manifest.mjs",
                        "--complete",
                        "--required",
                    ],
                );
                self.effect(&gate)
            }
            "deploy" => self.deploy(),
            "verify-live" => {
                let exe = display(&self.options.exe);
                let check =
                    self.command(&exe, &["verify-live", &self.options.version, "--complete"]);
                self.effect(&check)
            }
            other => Err(format!("No step {other}")),
        }
    }

    fn tag(&mut self) -> Result<()> {
        let version = self.options.version.clone();
        let tag = self.tag.clone();
        self.effect(&self.command("git", &["fetch", "origin"]))?;
        let main = self
            .query(&self.command("git", &["rev-parse", "--verify", "origin/main^{commit}"]))?
            .map(|sha| sha.trim().to_owned())
            .ok_or_else(|| "origin/main is not known here; run `git fetch origin`".to_owned())?;
        let manifest = self
            .query(&self.command("git", &["show", "origin/main:Cargo.toml"]))?
            .ok_or_else(|| "origin/main has no Cargo.toml".to_owned())?;
        let at_main = version_in(&manifest)?;
        if at_main != version {
            return Err(format!(
                "origin/main is at {at_main}, not {version}; merge the release pull request first"
            ));
        }
        let local = self.query(&self.command(
            "git",
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/tags/{tag}^{{commit}}"),
            ],
        ))?;
        match local.map(|sha| sha.trim().to_owned()) {
            Some(sha) if sha == main => self.say(&format!("The tag {tag} is at origin/main")),
            Some(sha) => {
                return Err(format!(
                    "The tag {tag} is at {sha}, not origin/main ({main}); delete it by hand if it was never pushed"
                ));
            }
            None => self.effect(&self.command(
                "git",
                &["tag", "-a", &tag, &main, "-m", &format!("Bake {version}")],
            ))?,
        }
        let listing = self
            .query(&self.command(
                "git",
                &[
                    "ls-remote",
                    "--tags",
                    "origin",
                    &format!("refs/tags/{tag}"),
                    &format!("refs/tags/{tag}^{{}}"),
                ],
            ))?
            .ok_or_else(|| "Could not list origin's tags".to_owned())?;
        match remote_tag_commit(&listing, &tag) {
            Some(sha) if sha == main => {
                self.say(&format!("origin has {tag} at origin/main"));
                Ok(())
            }
            Some(sha) => Err(format!(
                "origin's tag {tag} is at {sha}, not origin/main ({main})"
            )),
            None => {
                self.effect(&self.command("git", &["push", "origin", &format!("refs/tags/{tag}")]))
            }
        }
    }

    fn gh_release(&mut self) -> Result<()> {
        let version = self.options.version.clone();
        let tag = self.tag.clone();
        let notes = changelog_section(&read_text(&self.root().join("CHANGELOG.md"))?, &version)
            .filter(|notes| !notes.is_empty())
            .ok_or_else(|| format!("CHANGELOG.md has no notes for {version}"))?;
        let archives = self.archives.clone();
        let Some(release) = self.release()? else {
            let notes_file = self.scratch.path().join("notes.md");
            std::fs::write(&notes_file, format!("{notes}\n"))
                .map_err(|error| format!("{}: {error}", notes_file.display()))?;
            self.say(&format!("The notes are CHANGELOG.md's {version} section"));
            let mut args = vec![
                "release".to_owned(),
                "create".to_owned(),
                tag,
                "--verify-tag".to_owned(),
                "--title".to_owned(),
                format!("Bake {version}"),
                "--notes-file".to_owned(),
                display(&notes_file),
            ];
            args.extend(archives.iter().map(|path| display(path)));
            return self.effect(&self.command("gh", &args));
        };
        if release.draft {
            return Err(format!(
                "The GitHub release {tag} is a draft; publish or delete it by hand, then run again"
            ));
        }
        let mut missing = Vec::new();
        for path in &archives {
            let (sha256, _) = sha256_file(path)?;
            match compare(&release, &file_name(path), &sha256) {
                Asset::Missing => missing.push(display(path)),
                Asset::Same => {}
                Asset::Different => {
                    return Err(format!(
                        "The GitHub release {tag} holds {} with other bytes than {}",
                        file_name(path),
                        path.display()
                    ));
                }
            }
        }
        if missing.is_empty() {
            self.say(&format!(
                "The GitHub release {tag} exists with its archives"
            ));
            return Ok(());
        }
        let mut args = vec!["release".to_owned(), "upload".to_owned(), tag];
        args.extend(missing);
        self.effect(&self.command("gh", &args))
    }

    fn assemble(&mut self) -> Result<()> {
        let version = self.options.version.clone();
        let staged = self.public().join("stable.json");
        if staged.is_file()
            && parse_manifest(&read_text(&staged)?)
                .is_ok_and(|manifest| manifest.version == version)
        {
            self.say(&format!("{} already names {version}", staged.display()));
            return Ok(());
        }
        if self
            .release()?
            .is_some_and(|release| release.assets.contains_key("stable.json"))
        {
            return Err(format!(
                "The GitHub release {tag} already holds a signed stable.json that {public} does not; download it and stable.json.sig into {public}, and the archives into {public}/releases/{version}, with `gh release download {tag} --dir <dir>`, then resume with --from upload",
                tag = self.tag,
                public = self.public().display()
            ));
        }
        let key = self.options.key_file.clone();
        if !key.is_file() {
            return Err(format!(
                "No signing key at {}; name it with --key-file or BAKE_NATIVE_RELEASE_SIGNING_KEY_FILE",
                key.display()
            ));
        }
        let exe = display(&self.options.exe);
        let mut args = vec![
            "assemble".to_owned(),
            version,
            "--complete".to_owned(),
            "--key-file".to_owned(),
            display(&key),
        ];
        if self.options.offline {
            args.push("--offline".to_owned());
        }
        self.effect(&self.command(&exe, &args))
    }

    fn upload(&mut self) -> Result<()> {
        let tag = self.tag.clone();
        let host = self.root().join(HOST);
        let files = [
            self.public().join("stable.json"),
            self.public().join("stable.json.sig"),
            host.join("install.sh"),
            host.join("install.ps1"),
        ];
        let release = match self.release()? {
            Some(release) => Some(release),
            None if self.options.dry_run => None,
            None => {
                return Err(format!(
                    "No GitHub release {tag}; resume with --from gh-release"
                ));
            }
        };
        let mut missing = Vec::new();
        for path in &files {
            let local = if path.is_file() {
                Some(sha256_file(path)?.0)
            } else if self.options.dry_run {
                None
            } else {
                return Err(format!("{} does not exist", path.display()));
            };
            let found = match (&release, &local) {
                (Some(release), Some(sha256)) => compare(release, &file_name(path), sha256),
                (Some(release), None) if release.assets.contains_key(&file_name(path)) => {
                    Asset::Same
                }
                _ => Asset::Missing,
            };
            match found {
                Asset::Missing => missing.push(display(path)),
                Asset::Same => {}
                Asset::Different => {
                    return Err(format!(
                        "The GitHub release {tag} holds {} with other bytes than {}; a release's files never change",
                        file_name(path),
                        path.display()
                    ));
                }
            }
        }
        if missing.is_empty() {
            self.say(&format!(
                "The GitHub release {tag} holds the manifest and installers"
            ));
            return Ok(());
        }
        let mut args = vec!["release".to_owned(), "upload".to_owned(), tag];
        args.extend(missing);
        self.effect(&self.command("gh", &args))
    }

    fn deploy(&mut self) -> Result<()> {
        let version = self.options.version.clone();
        let url = format!("{}/native/stable.json", self.options.base);
        let served = self.query(&self.command("curl", &["-fsSL", "--max-time", "30", &url]))?;
        if served
            .as_deref()
            .and_then(|text| parse_manifest(text).ok())
            .is_some_and(|manifest| manifest.version == version)
        {
            self.say(&format!("{url} already names {version}"));
            return Ok(());
        }
        let deploy = Command::new(
            "npm",
            &[
                "run",
                "deploy",
                "--",
                "--message",
                &format!("Bake {version}"),
            ],
            self.root().join(HOST),
        );
        if self.options.dry_run {
            self.say(&format!("would run: (cd {HOST} && {deploy})"));
            return Ok(());
        }
        self.say(&format!("$ (cd {HOST} && {deploy})"));
        self.runner.effect(&deploy)
    }
}

/// Runs the steps from `options.from`, or with `dry_run` shows the commands
/// that change anything without running them.
pub fn publish(options: &Options, runner: &mut dyn Runner) -> Result<()> {
    let version = options.version.as_str();
    if !is_stable(version) {
        return Err(format!("{version} is not a canonical release version"));
    }
    let archives = archives(&options.root, version)?;
    let mut run = Run {
        options,
        runner,
        archives,
        tag: format!("v{version}"),
        scratch: Scratch::new("bake-release-publish")?,
    };
    if options.dry_run {
        run.say(&format!(
            "A dry run of publishing Bake {version}: queries run, nothing is changed"
        ));
    }
    let mut stopped = Vec::new();
    for name in STEPS.iter().skip(options.from) {
        run.say(&format!("== {name}"));
        if let Err(error) = run.step(name) {
            if options.dry_run {
                run.say(&format!("A real run would stop here: {error}"));
                stopped.push(*name);
            } else {
                return Err(format!(
                    "{name}: {error}\nOnce that is fixed, resume with `bake-release publish {version} --from {name}`"
                ));
            }
        }
    }
    if !stopped.is_empty() {
        return Err(format!("A real run would stop at {}", stopped.join(", ")));
    }
    run.say(&if options.dry_run {
        format!("The dry run of Bake {version} found nothing in the way")
    } else {
        format!("Published Bake {version}. Check an install with install.ps1 on Windows x64.")
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::Fake;
    use crate::util::sha256;
    use bake_update::manifest::Artifact;

    const VERSION: &str = "1.2.3";
    const MAIN: &str = "aaaa";
    const BASE: &str = "https://downloads.invalid";
    const EXE: &str = "/bin/bake-release";

    /// GitHub, origin, and the download service, as the commands see them.
    struct World {
        main_version: String,
        local_tag: Option<String>,
        remote_tag: Option<String>,
        /// Draft, and each asset's SHA-256.
        release: Option<(bool, BTreeMap<String, String>)>,
        served: Option<String>,
        /// An effect whose command contains this fails.
        fail: Option<&'static str>,
        public: PathBuf,
    }

    fn manifest(version: &str) -> String {
        let artifacts = RELEASE_TARGETS
            .iter()
            .map(|target| {
                (
                    (*target).to_owned(),
                    Artifact {
                        file: archive_name(version, target),
                        sha256: "0".repeat(64),
                        size: 1,
                    },
                )
            })
            .collect();
        String::from_utf8(crate::manifest::manifest_bytes(
            version,
            "2026-11-20T00:00:00Z",
            &artifacts,
        ))
        .expect("utf-8")
    }

    fn answer(world: &mut World, command: &Command) -> Result<Option<String>> {
        let args: Vec<&str> = command.args.iter().map(String::as_str).collect();
        Ok(match (command.program.as_str(), args.as_slice()) {
            ("git", ["rev-parse", "--verify", "origin/main^{commit}"]) => {
                Some(format!("{MAIN}\n"))
            }
            ("git", ["show", "origin/main:Cargo.toml"]) => Some(format!(
                "[workspace.package]\nversion = \"{}\"\n",
                world.main_version
            )),
            ("git", ["rev-parse", "--verify", "--quiet", _]) => {
                world.local_tag.as_ref().map(|sha| format!("{sha}\n"))
            }
            ("git", ["ls-remote", ..]) => Some(world.remote_tag.as_ref().map_or_else(
                String::new,
                |sha| format!("tagobject\trefs/tags/v{VERSION}\n{sha}\trefs/tags/v{VERSION}^{{}}\n"),
            )),
            ("gh", ["release", "view", ..]) => world.release.as_ref().map(|(draft, assets)| {
                serde_json::json!({
                    "isDraft": draft,
                    "assets": assets.iter().map(|(name, sha)| serde_json::json!({"name": name, "digest": format!("sha256:{sha}")})).collect::<Vec<_>>(),
                })
                .to_string()
            }),
            ("curl", _) => world.served.clone(),
            _ => return Err(format!("unexpected query {command}")),
        })
    }

    fn upload(world: &mut World, paths: &[String]) {
        let assets = &mut world.release.as_mut().expect("a release").1;
        for path in paths {
            let path = Path::new(path);
            assets.insert(file_name(path), sha256_file(path).expect("asset").0);
        }
    }

    fn apply(world: &mut World, command: &Command) -> Result<()> {
        if world
            .fail
            .is_some_and(|fail| command.to_string().contains(fail))
        {
            return Err(format!("`{command}` failed"));
        }
        let args: Vec<&str> = command.args.iter().map(String::as_str).collect();
        match (command.program.as_str(), args.as_slice()) {
            ("git", ["fetch", "origin"]) => {}
            ("git", ["tag", "-a", _, commit, ..]) => world.local_tag = Some((*commit).to_owned()),
            ("git", ["push", ..]) => world.remote_tag = world.local_tag.clone(),
            ("gh", ["release", "create", _, rest @ ..]) => {
                world.release = Some((false, BTreeMap::new()));
                let files: Vec<String> = rest
                    .iter()
                    .filter(|arg| arg.ends_with(".tar.gz"))
                    .map(|arg| (*arg).to_owned())
                    .collect();
                upload(world, &files);
            }
            ("gh", ["release", "upload", _, rest @ ..]) => {
                let files: Vec<String> = rest.iter().map(|arg| (*arg).to_owned()).collect();
                upload(world, &files);
            }
            (EXE, ["assemble", version, ..]) => {
                std::fs::create_dir_all(&world.public).expect("public");
                std::fs::write(world.public.join("stable.json"), manifest(version))
                    .expect("manifest");
                std::fs::write(world.public.join("stable.json.sig"), "signature\n")
                    .expect("signature");
            }
            ("node", _) | (EXE, ["verify-live", ..]) => {}
            ("npm", _) => world.served = Some(manifest(VERSION)),
            _ => return Err(format!("unexpected effect {command}")),
        }
        Ok(())
    }

    /// A checkout with both archives packed, the key, and the notes.
    fn checkout(scratch: &Scratch) -> (Options, Fake<World>) {
        let root = scratch.path().join("repo");
        let source = source_dir(&root, VERSION);
        std::fs::create_dir_all(&source).expect("source");
        for target in RELEASE_TARGETS {
            std::fs::write(source.join(archive_name(VERSION, target)), target).expect("archive");
        }
        std::fs::create_dir_all(root.join(HOST)).expect("host");
        std::fs::write(root.join(HOST).join("install.sh"), "sh").expect("install.sh");
        std::fs::write(root.join(HOST).join("install.ps1"), "ps1").expect("install.ps1");
        std::fs::write(
            root.join("CHANGELOG.md"),
            format!("## [Unreleased]\n\n## [{VERSION}] - 2026-11-20\n\n- New.\n"),
        )
        .expect("changelog");
        let key = scratch.path().join("key.pem");
        std::fs::write(&key, "key").expect("key");
        let options = Options {
            root: root.clone(),
            version: VERSION.to_owned(),
            key_file: key,
            offline: false,
            dry_run: false,
            from: 0,
            exe: PathBuf::from(EXE),
            base: BASE.to_owned(),
        };
        let runner = Fake {
            world: World {
                main_version: VERSION.to_owned(),
                local_tag: None,
                remote_tag: None,
                release: None,
                served: Some(manifest("1.2.2")),
                fail: None,
                public: root.join(HOST).join("public/native"),
            },
            log: Vec::new(),
            answer,
            apply,
        };
        (options, runner)
    }

    fn programs(runner: &Fake<World>) -> Vec<String> {
        runner
            .effects()
            .iter()
            .map(|effect| effect.split(' ').take(3).collect::<Vec<_>>().join(" "))
            .collect()
    }

    #[test]
    fn a_first_run_takes_every_step_in_order() {
        let scratch = Scratch::new("bake-release-publish-test").expect("scratch");
        let (options, mut runner) = checkout(&scratch);
        publish(&options, &mut runner).expect("publish");
        assert_eq!(
            programs(&runner),
            [
                "git fetch origin",
                "git tag -a",
                "git push origin",
                "gh release create",
                "/bin/bake-release assemble 1.2.3",
                "gh release upload",
                "node distribution/host/verify-native-manifest.mjs --complete",
                "npm run deploy",
                "/bin/bake-release verify-live 1.2.3",
            ]
        );
        let names: Vec<&str> = runner
            .log
            .iter()
            .filter_map(|line| line.strip_prefix("== "))
            .collect();
        assert_eq!(names, STEPS);
        let assets = &runner.world.release.as_ref().expect("release").1;
        assert_eq!(
            assets.keys().collect::<Vec<_>>(),
            [
                "bake-v1.2.3-linux-x64.tar.gz",
                "bake-v1.2.3-win32-x64.tar.gz",
                "install.ps1",
                "install.sh",
                "stable.json",
                "stable.json.sig"
            ]
        );
        let create = runner
            .effects()
            .into_iter()
            .find(|effect| effect.starts_with("gh release create"))
            .expect("create")
            .to_owned();
        assert!(
            create.starts_with(
                "gh release create v1.2.3 --verify-tag --title 'Bake 1.2.3' --notes-file "
            ),
            "{create}"
        );
        assert_eq!(runner.world.local_tag.as_deref(), Some(MAIN));
    }

    #[test]
    fn a_second_run_skips_what_is_done() {
        let scratch = Scratch::new("bake-release-publish-test").expect("scratch");
        let (options, mut runner) = checkout(&scratch);
        publish(&options, &mut runner).expect("first");
        runner.log.clear();
        publish(&options, &mut runner).expect("again");
        assert_eq!(
            programs(&runner),
            [
                "git fetch origin",
                "node distribution/host/verify-native-manifest.mjs --complete",
                "/bin/bake-release verify-live 1.2.3",
            ]
        );
    }

    #[test]
    fn a_failed_step_is_resumed_from_where_it_stopped() {
        let scratch = Scratch::new("bake-release-publish-test").expect("scratch");
        let (mut options, mut runner) = checkout(&scratch);
        runner.world.fail = Some("npm run deploy");
        let error = publish(&options, &mut runner).expect_err("deploy fails");
        assert!(error.starts_with("deploy: "), "{error}");
        assert!(error.contains("--from deploy"), "{error}");
        assert!(!programs(&runner).iter().any(|p| p.contains("verify-live")));
        runner.world.fail = None;
        runner.log.clear();
        options.from = step_index("deploy").expect("step");
        publish(&options, &mut runner).expect("resume");
        assert_eq!(
            programs(&runner),
            ["npm run deploy", "/bin/bake-release verify-live 1.2.3"]
        );
    }

    #[test]
    fn a_partial_release_gets_only_its_missing_assets() {
        let scratch = Scratch::new("bake-release-publish-test").expect("scratch");
        let (options, mut runner) = checkout(&scratch);
        let linux = source_dir(&options.root, VERSION).join(archive_name(VERSION, "linux-x64"));
        runner.world.local_tag = Some(MAIN.to_owned());
        runner.world.remote_tag = Some(MAIN.to_owned());
        runner.world.release = Some((
            false,
            BTreeMap::from([(file_name(&linux), sha256(b"linux-x64"))]),
        ));
        runner.world.fail = Some("assemble");
        publish(&options, &mut runner).expect_err("assemble fails");
        let effects = runner.effects();
        assert_eq!(effects.len(), 3, "{effects:?}");
        assert!(effects[1].starts_with("gh release upload v1.2.3 "));
        assert!(effects[1].ends_with("bake-v1.2.3-win32-x64.tar.gz"));
    }

    #[test]
    fn published_bytes_are_never_replaced() {
        let scratch = Scratch::new("bake-release-publish-test").expect("scratch");
        let (options, mut runner) = checkout(&scratch);
        runner.world.local_tag = Some(MAIN.to_owned());
        runner.world.remote_tag = Some(MAIN.to_owned());
        runner.world.release = Some((
            false,
            BTreeMap::from([(archive_name(VERSION, "win32-x64"), "f".repeat(64))]),
        ));
        let error = publish(&options, &mut runner).expect_err("other bytes");
        assert!(error.starts_with("gh-release: "), "{error}");
        assert!(error.contains("other bytes"), "{error}");
        assert_eq!(programs(&runner), ["git fetch origin"]);

        runner.world.release.as_mut().expect("release").0 = true;
        runner.world.release.as_mut().expect("release").1.clear();
        assert!(
            publish(&options, &mut runner)
                .expect_err("draft")
                .contains("draft")
        );
    }

    #[test]
    fn the_tag_goes_only_on_the_merged_release() {
        let scratch = Scratch::new("bake-release-publish-test").expect("scratch");
        let (options, mut runner) = checkout(&scratch);
        runner.world.main_version = "1.2.2".to_owned();
        let error = publish(&options, &mut runner).expect_err("not merged");
        assert!(error.contains("merge the release pull request"), "{error}");
        assert_eq!(programs(&runner), ["git fetch origin"]);

        runner.world.main_version = VERSION.to_owned();
        runner.world.remote_tag = Some("bbbb".to_owned());
        runner.log.clear();
        let error = publish(&options, &mut runner).expect_err("moved tag");
        assert!(error.contains("origin's tag v1.2.3 is at bbbb"), "{error}");
        assert!(!programs(&runner).iter().any(|p| p.starts_with("git push")));
    }

    #[test]
    fn a_dry_run_changes_nothing_and_shows_every_step() {
        let scratch = Scratch::new("bake-release-publish-test").expect("scratch");
        let (mut options, mut runner) = checkout(&scratch);
        options.dry_run = true;
        options.offline = true;
        publish(&options, &mut runner).expect("dry run");
        assert!(runner.effects().is_empty());
        assert!(runner.world.local_tag.is_none() && runner.world.release.is_none());
        assert!(!runner.world.public.exists());
        let shown: Vec<&str> = runner
            .log
            .iter()
            .filter_map(|line| line.strip_prefix("would run: "))
            .collect();
        assert_eq!(shown.len(), 9, "{shown:?}");
        assert!(shown.iter().any(|line| line.ends_with("--offline")));
        assert!(
            shown.contains(&"(cd distribution/host && npm run deploy -- --message 'Bake 1.2.3')")
        );
    }

    #[test]
    fn every_archive_must_be_packed_first() {
        let scratch = Scratch::new("bake-release-publish-test").expect("scratch");
        let (options, mut runner) = checkout(&scratch);
        std::fs::remove_file(
            source_dir(&options.root, VERSION).join(archive_name(VERSION, "win32-x64")),
        )
        .expect("remove");
        let error = publish(&options, &mut runner).expect_err("missing");
        assert!(error.contains("win32-x64"), "{error}");
        assert!(runner.log.is_empty());
    }

    #[test]
    fn gh_and_git_listings_are_read() {
        let release = parse_release(
            r#"{"isDraft":false,"assets":[{"name":"a","digest":"sha256:AB"},{"name":"b"}]}"#,
        )
        .expect("release");
        assert_eq!(
            release,
            Release {
                draft: false,
                assets: BTreeMap::from([
                    ("a".to_owned(), Some("ab".to_owned())),
                    ("b".to_owned(), None)
                ]),
            }
        );
        assert!(parse_release("{}").is_err());
        assert!(parse_release("not json").is_err());
        assert_eq!(
            remote_tag_commit("t1\trefs/tags/v1\nc1\trefs/tags/v1^{}\n", "v1").as_deref(),
            Some("c1")
        );
        assert_eq!(
            remote_tag_commit("c2\trefs/tags/v1\n", "v1").as_deref(),
            Some("c2")
        );
        assert_eq!(remote_tag_commit("c3\trefs/tags/v10\n", "v1"), None);
        assert_eq!(step_index("deploy").expect("deploy"), 5);
        assert!(step_index("fetch-legacy").is_err());
    }
}
