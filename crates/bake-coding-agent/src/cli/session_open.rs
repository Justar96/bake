//! The session file a run opens or creates: `--session`, `--resume`,
//! `--continue`, `--fork`, and `--session-id`.

use super::*;

/// Pi's `resolveSessionPath` result.
pub(super) enum ResolvedSession {
    Path(PathBuf),
    Global { path: PathBuf, cwd: String },
    NotFound,
}

pub(super) fn is_session_path(arg: &str) -> bool {
    arg.contains('/') || arg.contains('\\') || arg.ends_with(".jsonl")
}

/// Pi's `resolveSessionPath`: a path, else a local id or id prefix, else
/// one in another project. Without `session_dir`, sessions are looked up
/// under `home`.
pub(super) fn resolve_session_path(
    arg: &str,
    cwd: &str,
    session_dir: Option<&Path>,
    home: &Path,
) -> ResolvedSession {
    if is_session_path(arg) {
        // Pi's `resolvePath(sessionArg, cwd)`: `~` and Windows shell paths
        // expanded, then absolute against `cwd` with `.` and `..` removed.
        let normalized = normalize_path(arg);
        let joined = if normalized.is_absolute() {
            normalized
        } else {
            normalize_path(cwd).join(normalized)
        };
        return ResolvedSession::Path(lexical_normalize(&joined));
    }
    let local_dir =
        session_dir.map_or_else(|| default_session_dir_path(cwd, home), Path::to_path_buf);
    if let Ok(Some(exact)) = SessionManager::find_by_id(cwd, arg, Some(&local_dir)) {
        return ResolvedSession::Path(exact);
    }
    let local = SessionManager::list(cwd, Some(&local_dir), None, None).unwrap_or_default();
    if let Some(found) = local
        .iter()
        .find(|info| info.id == arg)
        .or_else(|| local.iter().find(|info| info.id.starts_with(arg)))
    {
        return ResolvedSession::Path(found.path.clone());
    }
    let all = match session_dir {
        Some(dir) => SessionManager::list_all(Some(dir), None, None),
        None => SessionManager::list_all_in(&sessions_dir(home), None, None),
    }
    .unwrap_or_default();
    match all
        .iter()
        .find(|info| info.id == arg)
        .or_else(|| all.iter().find(|info| info.id.starts_with(arg)))
    {
        Some(found) => ResolvedSession::Global {
            path: found.path.clone(),
            cwd: found.cwd.clone(),
        },
        None => ResolvedSession::NotFound,
    }
}

/// Why the session store could not start: Pi prints most failures as
/// errors, and a session id it cannot find as a bare line.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SessionStartError {
    Error(String),
    Plain(String),
}

/// Pi's `createSessionManager` for the ported flags. Without
/// `session_dir`, sessions live under `home`, never the process's own Bake
/// home. Blocking.
pub(super) fn create_session_manager(
    args: &Args,
    cwd: &str,
    session_dir: Option<&Path>,
    home: &Path,
    warnings: &mut Vec<String>,
) -> Result<SessionManager, SessionStartError> {
    let failed = |error: crate::session::SessionError| SessionStartError::Error(error.to_string());
    if args.no_session {
        return SessionManager::in_memory(
            Some(cwd),
            NewSessionOptions {
                id: args.session_id.clone(),
                parent_session: None,
            },
            Vec::new(),
        )
        .map_err(failed);
    }
    let dir = match session_dir {
        Some(dir) => dir.to_path_buf(),
        None => default_session_dir_path(cwd, home),
    };
    if let Some(fork) = &args.fork {
        if let Some(session_id) = &args.session_id
            && SessionManager::find_by_id(cwd, session_id, Some(&dir))
                .ok()
                .flatten()
                .is_some()
        {
            return Err(SessionStartError::Plain(format!(
                "Session already exists with id '{session_id}'"
            )));
        }
        return match resolve_session_path(fork, cwd, session_dir, home) {
            ResolvedSession::Path(path) | ResolvedSession::Global { path, .. } => {
                SessionManager::fork_from(
                    &path,
                    cwd,
                    Some(&dir),
                    NewSessionOptions {
                        id: args.session_id.clone(),
                        parent_session: None,
                    },
                )
                .map_err(failed)
            }
            ResolvedSession::NotFound => Err(SessionStartError::Plain(format!(
                "No session found matching '{fork}'"
            ))),
        };
    }
    if let Some(arg) = &args.session {
        return match resolve_session_path(arg, cwd, session_dir, home) {
            ResolvedSession::Path(path) => {
                if args.resume_run.is_some() && !path.is_file() {
                    return Err(SessionStartError::Error(
                        "--resume-run requires an existing session file.".into(),
                    ));
                }
                SessionManager::open(&path, Some(&dir), None).map_err(failed)
            }
            // Neither mode prompts whether to fork yet; point at `--fork`.
            ResolvedSession::Global { cwd, .. } => Err(SessionStartError::Error(format!(
                "Session found in different project: {cwd}. Use --fork {arg} to fork it into this directory."
            ))),
            ResolvedSession::NotFound => Err(SessionStartError::Plain(format!(
                "No session found matching '{arg}'"
            ))),
        };
    }
    // Pi's `createSessionManager` takes `--resume` before `--continue`:
    // the session picker decides what opens, and nothing else does. The
    // terminal opens the picker over this new session, which writes no
    // file until it holds a message ([`Started::resume`]).
    if resumes_at_startup(args) {
        return SessionManager::create(cwd, Some(&dir), NewSessionOptions::default())
            .map_err(failed);
    }
    if args.continue_session {
        return SessionManager::continue_recent(cwd, Some(&dir)).map_err(failed);
    }
    if let Some(session_id) = &args.session_id {
        if let Some(existing) =
            SessionManager::find_by_id(cwd, session_id, Some(&dir)).map_err(failed)?
        {
            return SessionManager::open(&existing, Some(&dir), None).map_err(failed);
        }
        warnings.push(format!(
            "No project session found with id '{session_id}'; creating a new session with that id."
        ));
    }
    SessionManager::create(
        cwd,
        Some(&dir),
        NewSessionOptions {
            id: args.session_id.clone(),
            parent_session: None,
        },
    )
    .map_err(failed)
}
