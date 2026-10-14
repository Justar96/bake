//! Pi's exclusive output-file creation, inside a private directory as Bake's
//! `docs/defensive-patterns.md` requires. Files persist for later tool reads.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

pub(super) fn validate_prefix(prefix: &str) -> io::Result<()> {
    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Output file prefix must contain only ASCII letters, digits, '-' or '_'.",
        ));
    }
    Ok(())
}

pub(super) fn create(parent: &Path, prefix: &str) -> io::Result<(PathBuf, File)> {
    let mut random = [0; 16];
    getrandom::getrandom(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let directory = parent.join(format!("{prefix}-{id}"));
    let builder = DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(&directory)?;
    let path = directory.join(format!("{prefix}.log"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(file) => Ok((path, file)),
        Err(error) => {
            let _ = std::fs::remove_dir(&directory);
            Err(error)
        }
    }
}
