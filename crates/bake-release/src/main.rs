//! `bake-release`: Bake's release steps. Run `bake-release help`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use bake_release::util::{Result, repository_root};
use bake_release::{
    assemble, glibc, http, licenses, live, manifest, pack, preflight, rehearse, workspace,
};

const USAGE: &str = "\
bake-release <command>

  preflight [--tag v<version>] [--notes <file>] [--offline] [--retry] [--dry-run]
      check that the workspace is ready to release the tag; write its notes
  licenses [--out <dir>]
      generate the third-party license bundle (needs cargo-about 0.9.2)
  pack [--binary <path>] [--out <dir>]
      archive the built release bake for this host, checked before and after
  glibc-floor <binary> [--floor GLIBC_2.34]
      fail when a Linux binary needs a newer glibc than the floor
  rehearse --archive <path>
      install the archive with the shipped install.sh from a loopback host
  assemble <version> [--complete] [--ephemeral-key | --key-file <pem>] [--offline]
      stage the archives under distribution/host/public and sign the manifest
  verify-live <version> [--complete]
      check what the download service serves, as a client does

The host is BAKE_RELEASE_BASE_URL, or https://bake.justar.dev.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("bake-release: {error}");
            ExitCode::FAILURE
        }
    }
}

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

/// The first argument after the command that is not a flag or its value.
fn positional(args: &[String], takes_value: &[&str]) -> Option<String> {
    let mut skip = false;
    for arg in args.iter().skip(1) {
        if skip {
            skip = false;
            continue;
        }
        if takes_value.contains(&arg.as_str()) {
            skip = true;
            continue;
        }
        if !arg.starts_with("--") {
            return Some(arg.trim_start_matches('v').to_owned());
        }
    }
    None
}

/// The live channel's version, checked with the committed key.
fn live_version(root: &Path) -> Result<Option<String>> {
    let keys = vec![manifest::committed_key(root)?];
    let base = http::base_url();
    http::runtime()?.block_on(async {
        let client = http::client(std::time::Duration::from_secs(30))?;
        http::published_release(&client, &base, &keys)
            .await
            .map(|manifest| manifest.map(|manifest| manifest.version))
            .map_err(|error| {
                format!("Could not read the published native release from {base}: {error}")
            })
    })
}

fn run(args: &[String]) -> Result<ExitCode> {
    let root = repository_root();
    match args.first().map(String::as_str) {
        Some("preflight") => {
            let dry_run = flag(args, "--dry-run");
            let live = if dry_run || flag(args, "--offline") {
                None
            } else {
                live_version(&root)?
            };
            let tag = if dry_run { None } else { value(args, "--tag") };
            let ready = preflight::preflight(&root, tag, live.as_deref(), flag(args, "--retry"));
            let (version, notes) = match ready {
                Ok(ready) => {
                    println!("Bake {} is ready to release", ready.version);
                    (ready.version, ready.notes)
                }
                Err(problems) if dry_run => {
                    let version = workspace::version(&root)?;
                    println!(
                        "A dry run builds Bake {version}; a release tag would be refused:\n{problems}"
                    );
                    let notes = workspace::changelog_section(
                        &bake_release::util::read_text(&root.join("CHANGELOG.md"))?,
                        &version,
                    )
                    .unwrap_or_default();
                    (version, notes)
                }
                Err(problems) => return Err(problems),
            };
            if let Some(file) = value(args, "--notes") {
                std::fs::write(file, format!("{notes}\n"))
                    .map_err(|error| format!("{file}: {error}"))?;
            }
            if let Ok(output) = std::env::var("GITHUB_OUTPUT") {
                use std::io::Write as _;
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&output)
                    .map_err(|error| format!("{output}: {error}"))?;
                writeln!(file, "version={version}").map_err(|error| error.to_string())?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("licenses") => {
            let bundle = licenses::generate(&root)?;
            if let Some(out) = value(args, "--out") {
                licenses::write(&bundle, Path::new(out))?;
                println!("Wrote {out}");
            }
            println!(
                "{} crates ({}), {} license texts",
                bundle.crates,
                bundle
                    .per_platform
                    .iter()
                    .map(|(id, count)| format!("{id} {count}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                bundle.texts
            );
            if !bundle.excluded.is_empty() {
                println!(
                    "Excluded, as no release platform compiles them: {}",
                    bundle.excluded.join(", ")
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("pack") => {
            let inputs = workspace::inputs(&root)?;
            let binary = value(args, "--binary")
                .map_or_else(|| inputs.target_dir.join("release/bake"), PathBuf::from);
            let out = value(args, "--out").map_or_else(
                || root.join(pack::OUTPUT_DIR).join(&inputs.version),
                PathBuf::from,
            );
            // Before the bundle: a refused directory should cost nothing.
            pack::check_output_dir(&root, &out)?;
            let bundle = licenses::generate(&root)?;
            let packed = pack::pack(&root, &inputs, &binary, &bundle, &out)?;
            println!("{}", packed.listing.join("\n"));
            println!("Packed {}\n{}", packed.archive.display(), packed.record);
            Ok(ExitCode::SUCCESS)
        }
        Some("glibc-floor") => {
            let binary = positional(args, &["--floor"]).ok_or(USAGE)?;
            let limit = value(args, "--floor").unwrap_or(glibc::DEFAULT_FLOOR);
            Ok(if glibc::run(Path::new(&binary), limit)? {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Some("rehearse") => {
            let archive = value(args, "--archive").ok_or(USAGE)?;
            let version = workspace::version(&root)?;
            println!(
                "Native rehearsal: {}",
                rehearse::rehearse(&root, &version, Path::new(archive))?
            );
            Ok(ExitCode::SUCCESS)
        }
        Some("assemble") => {
            let version = positional(args, &["--key-file"]).ok_or(USAGE)?;
            let source = assemble::source_dir(&root, &version);
            let key_file = value(args, "--key-file").map(PathBuf::from).or_else(|| {
                std::env::var_os("BAKE_NATIVE_RELEASE_SIGNING_KEY_FILE").map(PathBuf::from)
            });
            let key = assemble::signing_key(
                &root,
                flag(args, "--ephemeral-key"),
                key_file.as_deref(),
                &source,
            )?;
            let live = if flag(args, "--offline") {
                None
            } else {
                live_version(&root)?
            };
            let options = assemble::Options {
                root: root.clone(),
                version: version.clone(),
                source,
                public: root.join("distribution/host/public"),
                complete: flag(args, "--complete"),
                live,
                now: SystemTime::now(),
            };
            let assembled = assemble::assemble(&options, &key)?;
            println!(
                "Staged and signed {} for native Bake {version}",
                assembled.files.join(", ")
            );
            Ok(ExitCode::SUCCESS)
        }
        Some("verify-live") => {
            let version = positional(args, &[]).ok_or(USAGE)?;
            let base = http::base_url();
            let manifest = live::verify_live(&root, &base, &version, flag(args, "--complete"))?;
            println!(
                "Verified live Bake {} at {base}: {}, with the 0.3 route intact",
                manifest.version,
                manifest
                    .artifacts
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            Ok(ExitCode::SUCCESS)
        }
        Some("help" | "--help" | "-h") | None => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some(other) => Err(format!("unknown command {other}\n\n{USAGE}")),
    }
}
