//! How `prepare` and `publish` run the tools they drive: git, gh, cargo,
//! curl, node, npm, and `bake-release` itself. The steps decide what to run
//! and when to skip; a [`Runner`] only runs it, so the steps are tested
//! against a stand-in for the outside world.

use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;

use crate::util::Result;

/// One command: a program, its arguments, and its working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    pub dir: PathBuf,
}

impl Command {
    pub fn new<S: AsRef<str>>(program: &str, args: &[S], dir: impl Into<PathBuf>) -> Self {
        Self {
            program: program.to_owned(),
            args: args.iter().map(|arg| arg.as_ref().to_owned()).collect(),
            dir: dir.into(),
        }
    }
}

/// The command as a person would type it in a POSIX shell.
impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&quote(&self.program))?;
        for arg in &self.args {
            write!(f, " {}", quote(arg))?;
        }
        Ok(())
    }
}

/// `text` quoted for a POSIX shell when it holds anything but plain
/// characters.
pub fn quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:=@+,%^{}".contains(&b));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// What runs the commands and shows the progress.
pub trait Runner {
    /// Runs a command that changes nothing and returns its standard output,
    /// or `None` when it exits unsuccessfully. It runs in a dry run too.
    ///
    /// # Errors
    ///
    /// When the program cannot start.
    fn query(&mut self, command: &Command) -> Result<Option<String>>;

    /// Runs a command with effects, its output shown as it runs.
    ///
    /// # Errors
    ///
    /// When it cannot start or exits unsuccessfully.
    fn effect(&mut self, command: &Command) -> Result<()>;

    /// Shows one line of progress.
    fn say(&mut self, line: &str);
}

/// The [`Runner`] of a real run: processes on this host, progress on
/// standard output.
#[derive(Debug, Default)]
pub struct System;

impl Runner for System {
    fn query(&mut self, command: &Command) -> Result<Option<String>> {
        let output = std::process::Command::new(&command.program)
            .args(&command.args)
            .current_dir(&command.dir)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("{} could not start: {error}", command.program))?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned()))
    }

    fn effect(&mut self, command: &Command) -> Result<()> {
        let status = std::process::Command::new(&command.program)
            .args(&command.args)
            .current_dir(&command.dir)
            .stdin(Stdio::null())
            .status()
            .map_err(|error| format!("{} could not start: {error}", command.program))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("`{command}` exited {status}"))
        }
    }

    fn say(&mut self, line: &str) {
        println!("{line}");
    }
}

#[cfg(test)]
pub mod fake {
    //! A recording [`Runner`] for tests, over a scripted world.

    use super::{Command, Runner};
    use crate::util::Result;

    /// Answers queries with `answer`, applies effects with `apply`, and
    /// records every line: `? <query>`, `+ <effect>`, and what was said.
    pub struct Fake<W> {
        pub world: W,
        pub log: Vec<String>,
        pub answer: fn(&mut W, &Command) -> Result<Option<String>>,
        pub apply: fn(&mut W, &Command) -> Result<()>,
    }

    impl<W> Fake<W> {
        /// The effects run, as typed.
        pub fn effects(&self) -> Vec<&str> {
            self.log
                .iter()
                .filter_map(|line| line.strip_prefix("+ "))
                .collect()
        }
    }

    impl<W> Runner for Fake<W> {
        fn query(&mut self, command: &Command) -> Result<Option<String>> {
            self.log.push(format!("? {command}"));
            (self.answer)(&mut self.world, command)
        }

        fn effect(&mut self, command: &Command) -> Result<()> {
            self.log.push(format!("+ {command}"));
            (self.apply)(&mut self.world, command)
        }

        fn say(&mut self, line: &str) {
            self.log.push(line.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_reads_as_typed() {
        let command = Command::new(
            "gh",
            &[
                "release",
                "create",
                "v1.2.3",
                "--title",
                "Bake 1.2.3",
                "it's",
            ],
            "/",
        );
        assert_eq!(
            command.to_string(),
            r"gh release create v1.2.3 --title 'Bake 1.2.3' 'it'\''s'"
        );
        assert_eq!(quote(""), "''");
        assert_eq!(quote("origin/main^{commit}"), "origin/main^{commit}");
    }
}
