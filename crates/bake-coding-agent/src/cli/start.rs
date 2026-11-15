//! Starting a run ([`start_with`]): the session, its permissions, and the
//! services a mode needs.

use super::*;

/// Which mode a session is being set up for: what it does without a model,
/// and whether piped standard input is a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StartKind {
    /// Print mode: piped input is the first prompt, and no model fails.
    Print,
    /// Interactive mode: no model still opens the session.
    Interactive,
    /// RPC mode: standard input is the protocol, and no model fails.
    Rpc,
}

pub(super) async fn start_with(
    environment: CliEnvironment,
    args: Args,
    kind: StartKind,
) -> Result<Started, i32> {
    let CliEnvironment {
        cwd,
        home,
        user_home,
        docs_command,
        stdin_is_terminal,
        stdin,
        stdout,
        stderr,
        extra_providers,
        sandbox_provider,
        approver,
        plugins,
        warnings: startup_warnings,
        mut interrupt,
        telecom_env,
    } = environment;
    let mut args = args;
    let Some(home) = home else {
        return Err(error(
            &stderr,
            "Cannot find the Bake home: set BAKE_HOME or make the user's home directory resolvable",
        ));
    };
    let cwd = cwd.to_string_lossy().into_owned();
    let factory = SessionFactory {
        args: args.clone(),
        home: home.clone(),
        user_home: user_home.clone(),
        docs_command: docs_command.clone(),
        sandbox_provider: Arc::clone(&sandbox_provider),
        approver: approver.clone(),
        plugins: plugins.clone(),
        drawn: kind == StartKind::Interactive,
        telecom_env,
    };

    let loaded = {
        let home = home.clone();
        let cwd = cwd.clone();
        let args = args.clone();
        tokio::task::spawn_blocking(move || {
            let settings = Settings::load(&home);
            let mut registry = ModelRegistry::load(&home);
            let mut warnings = startup_warnings;
            warnings.extend(settings.warnings.iter().cloned());
            for (id, config, key) in extra_providers {
                if let Err(message) = registry.add_provider_config(&id, config, key) {
                    warnings.push(message);
                }
            }
            warnings.extend(apply_account(&home, &mut registry));
            if let Some(message) = registry.error() {
                warnings.push(message);
            }
            let session_dir = args
                .session_dir
                .as_deref()
                .map(normalize_path)
                .or_else(|| settings.session_dir.clone());
            let session =
                create_session_manager(&args, &cwd, session_dir.as_deref(), &home, &mut warnings);
            let session = session.and_then(|mut session| {
                // Pi v1.5.2's `main`: outside interactive mode, a session
                // whose stored working directory is gone is refused before
                // anything is written to it. Interactive mode still opens it.
                if kind != StartKind::Interactive
                    && let Some(issue) = get_missing_session_cwd_issue(&session, &cwd)
                {
                    return Err(SessionStartError::Plain(format_missing_session_cwd_error(
                        &issue,
                    )));
                }
                if let Some(name) = args.name.as_deref().and_then(normalize_session_name) {
                    session
                        .append_session_info(&name)
                        .map_err(|error| SessionStartError::Error(error.to_string()))?;
                }
                let resources = load_resources(
                    &mut session,
                    &settings,
                    &args,
                    &home,
                    user_home,
                    &plugins,
                    true,
                    &mut warnings,
                )
                .map_err(SessionStartError::Error)?;
                let processed_files = if args.file_args.is_empty() {
                    Ok(file_processor::ProcessedFiles::default())
                } else {
                    // AgentSession resizes prompt images against the selected model's limits.
                    process_file_arguments(
                        &args.file_args,
                        Path::new(&cwd),
                        ProcessFileOptions {
                            auto_resize_images: false,
                        },
                    )
                    .map_err(SessionStartError::Error)
                }?;
                Ok((session, resources, processed_files))
            });
            (settings, registry, warnings, session)
        })
        .await
    };
    let (settings, mut registry, warnings, session) = match loaded {
        Ok(loaded) => loaded,
        Err(join) => return Err(error(&stderr, &format!("Startup failed: {join}"))),
    };
    let (session, resources, processed_files) = match session {
        Ok(session) => session,
        Err(SessionStartError::Error(message)) => return Err(error(&stderr, &message)),
        Err(SessionStartError::Plain(message)) => {
            write_out(&stderr, &format!("{message}\n"));
            return Err(1);
        }
    };
    let has_existing_session = !session.build_session_context().messages.is_empty();

    // Pi's `resolveModelScope` and `buildSessionOptions`.
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let scoped_models: Vec<ScopedModel> = match args
        .models
        .as_deref()
        .or(settings.enabled_models.as_deref())
    {
        Some(patterns) if !patterns.is_empty() => {
            let candidates = if args.api_key.is_some() {
                registry.models()
            } else {
                registry.available()
            };
            let scope = resolve_model_scope_from_models(patterns, &candidates);
            for diagnostic in scope.diagnostics {
                diagnostics.push(Diagnostic {
                    kind: DiagnosticKind::Warning,
                    message: diagnostic.message,
                });
            }
            scope.scoped_models
        }
        _ => Vec::new(),
    };
    let mut model = None;
    let mut thinking_level = args.thinking;
    if let Some(provider) = &args.provider
        && args.model.is_none()
    {
        diagnostics.push(Diagnostic {
            kind: DiagnosticKind::Error,
            message: format!(
                "--provider requires --model (for example: --provider {provider} --model <pattern>)"
            ),
        });
    }
    if let Some(pattern) = &args.model {
        let resolved = resolve_cli_model(
            args.provider.as_deref(),
            Some(pattern),
            args.thinking,
            &registry,
        );
        if let Some(warning) = resolved.warning {
            diagnostics.push(Diagnostic {
                kind: DiagnosticKind::Warning,
                message: warning,
            });
        }
        // Pi points at `--list-models`, which bake does not have.
        if let Some(message) = resolved.error {
            diagnostics.push(Diagnostic {
                kind: DiagnosticKind::Error,
                message: format!("{message} See:\n  {docs_command} models"),
            });
        }
        if let Some(found) = resolved.model {
            model = Some(found);
            if args.thinking.is_none() && resolved.thinking_level.is_some() {
                thinking_level = resolved.thinking_level;
            }
        }
    }
    if model.is_none() && !scoped_models.is_empty() && !has_existing_session {
        let saved_in_scope = settings
            .default_provider
            .as_deref()
            .zip(settings.default_model.as_deref())
            .and_then(|(provider, id)| registry.model(provider, id))
            .and_then(|saved| {
                scoped_models.iter().find(|scoped| {
                    scoped.model.provider == saved.provider && scoped.model.id == saved.id
                })
            });
        let chosen = saved_in_scope.unwrap_or(&scoped_models[0]);
        model = Some(chosen.model.clone());
        if args.thinking.is_none() && chosen.thinking_level.is_some() {
            thinking_level = chosen.thinking_level;
        }
    }
    if let Some(api_key) = &args.api_key {
        match &model {
            Some(selected) => registry.set_runtime_api_key(&selected.provider, api_key),
            None => diagnostics.push(Diagnostic {
                kind: DiagnosticKind::Error,
                message: "--api-key requires a model to be specified via --model, --provider/--model, or --models".into(),
            }),
        }
    }
    for warning in &warnings {
        warn(&stderr, warning);
    }
    report(&stderr, &diagnostics);
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == DiagnosticKind::Error)
    {
        return Err(1);
    }

    let permission = resources.permission.clone();
    let built = match build_session(
        &args,
        Build {
            session,
            apis: Arc::new(ApiRegistry::with_builtins()),
            resources,
            settings,
            registry: registry.into(),
            model,
            thinking_level,
            home: home.clone(),
            docs_command: docs_command.clone(),
            sandbox_provider,
            approver,
            telecom_env: factory.telecom_env.clone(),
            role: Role::Root(Some(Box::new(factory.clone()))),
            drawn: kind == StartKind::Interactive,
        },
    )
    .await
    {
        Ok(built) => built,
        Err(message) => return Err(error(&stderr, &message)),
    };
    let session = built.session;
    // At once, before piped input is read: as in Pi, the servers connect
    // while the rest of startup runs.
    if let Err(message) = start_servers(&session, built.servers) {
        session.shutdown().await;
        return Err(error(&stderr, &message));
    }

    // The session, and any MCP server it started, is shut down whether the
    // read fails or a stop request (print mode's SIGTERM or SIGHUP) comes
    // while a writer holds the pipe open. The read itself cannot be
    // cancelled: its thread ([`read_stdin`]) stays blocked, and nothing
    // waits for it at exit. In RPC mode the stream carries the protocol,
    // as in Pi; nothing reads it here. `-` reads a terminal too, to its
    // end-of-file.
    let mut protocol_stdin = None;
    let piped = if (stdin_is_terminal && !args.read_stdin) || kind == StartKind::Rpc {
        if kind == StartKind::Rpc {
            protocol_stdin = Some(stdin);
        }
        Ok(None)
    } else {
        // Piped input beside another prompt need not come; see the module
        // docs.
        let prompt_elsewhere =
            !args.messages.is_empty() || !args.file_args.is_empty() || args.resume_run.is_some();
        let wait = (prompt_elsewhere && !args.read_stdin).then_some(STDIN_FIRST_BYTE_WAIT);
        tokio::select! {
            read = read_stdin(stdin, wait) => read.map(|piped| match piped {
                Piped::Read(text) => text,
                Piped::Silent => {
                    warn(&stderr, &silent_stdin_warning(STDIN_FIRST_BYTE_WAIT));
                    None
                }
            }),
            code = &mut interrupt => {
                session.shutdown().await;
                return Err(code);
            }
        }
    };
    let piped = match piped {
        Ok(piped) => piped,
        Err(message) => {
            session.shutdown().await;
            return Err(error(&stderr, &message));
        }
    };
    let initial_message = build_initial_message(piped, &processed_files.text, &mut args.messages);

    if session.model().is_none() && kind != StartKind::Interactive {
        session.shutdown().await;
        write_out(
            &stderr,
            &format!("{}\n", format_no_models_available_message(&docs_command)),
        );
        return Err(1);
    }
    let resume = resumes_at_startup(&args);
    Ok(Started {
        session,
        initial_message,
        initial_images: processed_files.images,
        messages: args.messages,
        stdin: protocol_stdin,
        stdout,
        stderr,
        interrupt,
        home,
        factory,
        resume,
        permission,
    })
}
