//! `bake-rs plugin install | enable | disable | list`: the owner's
//! management of the Wasm plugins in the Bake home
//! ([`bake_coding_agent::plugins::installed`]). Sessions started
//! afterwards load what `plugins.json` enables; a running session keeps
//! what it loaded.
//!
//! - `install <dir>` reads `plugin.json` and `plugin.wasm` from `<dir>`
//!   once, without following links, starts the package in a helper as
//!   `plugin check` does (checked and compiled there, never in this
//!   process), and only after a clean shutdown copies the bytes it read
//!   into the store under their hash. It runs nothing from `<dir>` and
//!   writes nothing there, and it enables nothing.
//! - `enable <sha256>` rehashes the store copy and records it in
//!   `plugins.json`. `disable <id>` removes the record and any recorded
//!   failure; disabling and enabling again is how a failed plugin is reset.
//! - `list` prints each enabled plugin, its hash, its tools, and whether it
//!   is ready, failed, or unloadable.
//!
//! Every plugin-supplied text is printed with control characters escaped.
//! Exit 0 on success, 1 with an `Error:` line otherwise, 130 after Ctrl+C
//! during `install`'s check.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use bake_coding_agent::plugins::installed::{EnabledPlugin, PluginHome};
use bake_coding_agent::plugins::wasm::WasmPackage;

use crate::plugin_check::{self, Failure, escape, fail, line};

/// A management command.
#[derive(Debug, PartialEq, Eq)]
pub enum Manage {
    Install(PathBuf),
    Enable(String),
    Disable(String),
    List,
}

/// Runs `command` against the Bake home.
pub fn run(command: &Manage) -> ExitCode {
    plugin_check::exit(manage(command))
}

fn home() -> Result<PluginHome, Failure> {
    bake_coding_agent::home::bake_home()
        .map(|home| PluginHome::new(&home))
        .ok_or_else(|| {
            fail("Cannot find the Bake home: set BAKE_HOME or make the user's home directory resolvable")
        })
}

fn manage(command: &Manage) -> Result<(), Failure> {
    let home = home()?;
    match command {
        Manage::Install(dir) => install(&home, dir),
        Manage::Enable(hash) => {
            let enabled = home
                .enable(hash)
                .map_err(|error| fail(&error.to_string()))?;
            line(&format!(
                "{} {} {} (sha256:{}), vetoes {}",
                if enabled.changed {
                    "enabled"
                } else {
                    "already enabled:"
                },
                escape(&enabled.id),
                escape(&enabled.record.version),
                enabled.record.sha256,
                escape(&enabled.record.veto_tools.join(", "))
            ))?;
            if let Some(failure) = &enabled.record.failure {
                line(&format!(
                    "it is failed ({}); run 'bake-rs plugin disable {}' then enable it again to reset it",
                    escape(&failure.kind),
                    escape(&enabled.id)
                ))?;
            }
            line("New sessions load it; running sessions keep what they loaded.")
        }
        Manage::Disable(id) => match home.disable(id).map_err(|error| fail(&error.to_string()))? {
            Some(record) => line(&format!(
                "disabled {} (sha256:{}); its store copy stays installed",
                escape(id),
                record.sha256
            )),
            None => Err(fail(&format!("plugin {id} is not enabled"))),
        },
        Manage::List => list(&home),
    }
}

fn install(home: &PluginHome, dir: &Path) -> Result<(), Failure> {
    // Read once: the bytes checked in the helper are the bytes stored.
    let package = WasmPackage::read_dir(dir).map_err(|error| fail(&error.to_string()))?;
    plugin_check::check_package(package.clone(), None)?;
    let installed = home
        .install(&package)
        .map_err(|error| fail(&error.to_string()))?;
    line(&format!(
        "{} {} {} as sha256:{}",
        if installed.already {
            "already installed:"
        } else {
            "installed"
        },
        escape(&installed.id),
        escape(&installed.version),
        installed.sha256
    ))?;
    line(&format!(
        "enable it with: bake-rs plugin enable {}",
        installed.sha256
    ))
}

fn list(home: &PluginHome) -> Result<(), Failure> {
    let enabled = home.enabled().map_err(|error| fail(&error.to_string()))?;
    if enabled.is_empty() {
        return line(&format!(
            "No plugin is enabled ({}).",
            home.config_path().display()
        ));
    }
    for (id, record) in &enabled {
        line(&format!(
            "{} {} sha256:{} vetoes {}: {}",
            escape(id),
            escape(&record.version),
            record.sha256,
            escape(&record.veto_tools.join(", ")),
            state(home, record)
        ))?;
    }
    Ok(())
}

/// Whether `record` is ready, failed, or unloadable, as a session would
/// find it.
fn state(home: &PluginHome, record: &EnabledPlugin) -> String {
    if let Some(failure) = &record.failure {
        return format!(
            "failed ({}: {}), blocking its tools until disabled",
            escape(&failure.kind),
            escape(&failure.message)
        );
    }
    match home.read_installed(&record.sha256) {
        Ok(_) => "ready".to_owned(),
        Err(error) => format!(
            "unloadable, blocking its tools: {}",
            escape(&error.to_string())
        ),
    }
}
