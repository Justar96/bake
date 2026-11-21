//! Building a session from the command line and the Bake home: its
//! resources, tools, MCP servers, children, and the services it owns.

use super::*;

/// What a session's setup reads besides its session file and model
/// selection: the context files, skills, prompt templates, permission, and
/// prompt overrides, all for the session's working directory. Pi's
/// resource loader and `buildSessionOptions`, without extensions.
pub(super) struct Resources {
    context_files: Vec<crate::system_prompt::ContextFile>,
    /// The `mcp.json` servers to start; empty under `--no-mcp`.
    mcp_servers: Vec<crate::mcp::McpServerEntry>,
    skills: Vec<crate::skills::Skill>,
    prompt_templates: Vec<crate::prompt_templates::PromptTemplate>,
    pub(super) permission: crate::permissions::SessionPermission,
    custom_prompt: Option<String>,
    append_system_prompt: Option<String>,
    plugins: crate::plugins::PluginRegistry,
    /// The hooks files `settings.json` names ([`crate::hooks`]).
    hooks: Vec<crate::hooks::HookFile>,
}

/// Settles `session`'s permission, writing any change to its file
/// before anything can run, and loads the resources for its working
/// directory. `permission_flags` applies `--permission`, `--sandbox`, and
/// `--approval`; a resumed session leaves them out and keeps its own.
/// The Wasm plugins the home enables are loaded under `plugins`; an
/// invalid or untrusted `plugins.json`, or a plugins directory inside the
/// session's working directory or a git repository, fails the setup
/// before any tool can run.
/// Blocking.
#[allow(clippy::too_many_arguments)]
pub(super) fn load_resources(
    session: &mut SessionManager,
    settings: &Settings,
    args: &Args,
    home: &Path,
    user_home: Option<PathBuf>,
    plugins: &WasmSetup,
    permission_flags: bool,
    warnings: &mut Vec<String>,
) -> Result<Resources, String> {
    let loaded = PluginHome::new(home)
        .load(plugins, Some(Path::new(session.cwd())))
        .map_err(|error| error.to_string())?;
    warnings.extend(loaded.warnings);
    let permission = prepare_session_permission(
        session,
        &PermissionInputs {
            settings: settings.permission.as_ref(),
            preset: args.permission.as_deref().filter(|_| permission_flags),
            sandbox: args.sandbox.filter(|_| permission_flags),
            approval: args.approval.filter(|_| permission_flags),
        },
    )?;
    warnings.extend(permission.warnings.iter().cloned());
    let mcp_servers = if args.no_mcp {
        Vec::new()
    } else {
        load_mcp_servers(
            home,
            Path::new(session.cwd()),
            user_home.as_deref(),
            warnings,
        )
    };
    let (hooks, hook_warnings) = crate::hooks::load_hook_files(
        &settings.hooks,
        home,
        Path::new(session.cwd()),
        user_home.as_deref(),
    );
    warnings.extend(hook_warnings.iter().map(|warning| terminal_safe(warning)));
    let context_files = if args.no_context_files {
        Vec::new()
    } else {
        let agents_dir = user_home.as_deref().map(user_agents_dir);
        let (files, context_warnings) =
            load_project_context_files(session.cwd(), agents_dir.as_deref());
        warnings.extend(context_warnings);
        files
    };
    // Pi's print mode does not report skill or prompt-template diagnostics.
    let skills = load_skills(&LoadSkillsOptions {
        cwd: session.cwd().to_owned(),
        user_home,
        skill_paths: args.skills.clone(),
        include_defaults: !args.no_skills,
    })
    .skills;
    let prompt_templates = load_prompt_templates(&LoadPromptTemplatesOptions {
        cwd: session.cwd().to_owned(),
        agent_dir: home.to_path_buf(),
        prompt_paths: args.prompt_templates.clone(),
        include_defaults: !args.no_prompt_templates,
    })
    .templates;
    let custom_prompt =
        resolve_prompt_input(args.system_prompt.as_deref(), session.cwd(), warnings);
    let append_parts: Vec<String> = args
        .append_system_prompt
        .iter()
        .filter_map(|input| resolve_prompt_input(Some(input), session.cwd(), warnings))
        .collect();
    let append_system_prompt = (!append_parts.is_empty()).then(|| append_parts.join("\n\n"));
    Ok(Resources {
        context_files,
        mcp_servers,
        skills,
        prompt_templates,
        permission,
        custom_prompt,
        append_system_prompt,
        plugins: loaded.registry,
        hooks,
    })
}

/// Reads the Bake home's `mcp.json` for a session in `cwd`
/// ([`crate::mcp::load_home_mcp_config`]), adding a warning for the file's
/// refusal or each refused entry. Blocking.
fn load_mcp_servers(
    home: &Path,
    cwd: &Path,
    user_home: Option<&Path>,
    warnings: &mut Vec<String>,
) -> Vec<crate::mcp::McpServerEntry> {
    let config = crate::mcp::load_home_mcp_config(home, cwd, user_home);
    warnings.extend(
        config
            .errors
            .iter()
            .map(|error| terminal_safe(&format!("MCP: {error}"))),
    );
    config.servers
}

/// `text` as one line, with every control character, line breaks
/// included, and every bidirectional format character shown as a `\u{…}`
/// escape, so text a configuration file or a server supplies can neither
/// reach the terminal as an escape sequence nor reorder or forge a line.
pub(crate) fn terminal_safe(text: &str) -> String {
    let mut safe = String::with_capacity(text.len());
    for character in text.chars() {
        // Control characters, and the format characters that reorder
        // or hide text (bidirectional embeddings, overrides, isolates).
        let hidden = matches!(character, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
        if character.is_control() || hidden {
            safe.push_str(&format!("\\u{{{:x}}}", u32::from(character)));
        } else {
            safe.push(character);
        }
    }
    safe
}

/// The first line of each MCP server that is not serving once the startup
/// wait ends, or Pi's note that servers are still connecting.
pub(super) fn mcp_problems(servers: &crate::mcp::McpServers) -> Vec<String> {
    use crate::mcp::McpServerState;
    let mut problems = Vec::new();
    let mut connecting = false;
    for status in servers.statuses() {
        let error = match status.state {
            McpServerState::Refused { reason } => reason,
            McpServerState::Failed { error } | McpServerState::Disconnected { error } => error,
            McpServerState::Connecting => {
                connecting = true;
                continue;
            }
            _ => continue,
        };
        let line = error.lines().next().unwrap_or_default();
        problems.push(terminal_safe(line));
    }
    if connecting {
        problems.push(
            "MCP servers are still connecting; their tools become available once connected."
                .to_owned(),
        );
    }
    problems
}

/// What [`build_session`] makes a session from.
pub(super) struct Build {
    pub(super) session: SessionManager,
    pub(super) apis: Arc<ApiRegistry>,
    pub(super) resources: Resources,
    pub(super) settings: Settings,
    pub(super) registry: crate::model_registry::ModelRuntime,
    pub(super) model: Option<bake_ai::Model>,
    pub(super) thinking_level: Option<bake_ai::ModelThinkingLevel>,
    pub(super) home: PathBuf,
    pub(super) docs_command: String,
    pub(super) sandbox_provider: Arc<dyn SandboxProvider>,
    pub(super) approver: Option<Arc<dyn Approver>>,
    /// The process's telecom environment ([`CliEnvironment::telecom_env`]).
    pub(super) telecom_env: crate::telecom::TelecomEnv,
    pub(super) role: Role,
    /// The interactive terminal shows this session, so it draws what the
    /// `visualize` tool shows.
    pub(super) drawn: bool,
}

/// Whether a session can start subagents, or is one.
pub(super) enum Role {
    /// A top-level session; with a factory it starts subagents built as
    /// it was ([`crate::subagents`]).
    Root(Option<Box<SessionFactory>>),
    /// A subagent, which messages its parent over this link and starts no
    /// subagents of its own.
    Child(crate::subagents::ParentLink),
}

/// What a parent's subagents are built with: its setup, model runtime,
/// settings, place, and permission, which every child inherits.
struct ChildSetup {
    factory: SessionFactory,
    apis: Arc<ApiRegistry>,
    registry: crate::model_registry::ModelRuntime,
    settings: Settings,
    cwd: String,
    place: PathBuf,
    persisted: bool,
    parent_id: String,
    parent_file: Option<PathBuf>,
    permission: crate::permissions::SessionPermission,
    /// The parent's standing permission, which `/permission` may have
    /// switched since `permission` was settled: a child takes it as it is
    /// when the child is built.
    standing: Arc<crate::permissions::StandingPermissions>,
}

/// The builder of a parent's subagents ([`crate::subagents::ChildBuilder`]).
fn child_builder(setup: ChildSetup) -> crate::subagents::ChildBuilder {
    let setup = Arc::new(setup);
    Arc::new(move |spec| build_child(Arc::clone(&setup), spec))
}

/// Builds one subagent as its parent was built: the parent's arguments
/// with its sandbox mode and approval policy, its settings, model runtime,
/// home, plugins, MCP servers, sandbox provider, and approver, and the
/// child's tools in place of the parent's.
fn build_child(
    setup: Arc<ChildSetup>,
    spec: crate::subagents::ChildSpec,
) -> bake_agent::BoxFuture<'static, Result<AgentSession, String>> {
    Box::pin(async move {
        let crate::subagents::ChildSpec {
            source,
            link,
            route,
        } = spec;
        // The parent's preset now, or its knobs when they match none.
        let mut args = setup.factory.args.clone();
        let preset = setup.standing.preset();
        let custom = setup.permission.table.spec(&preset).is_none();
        args.sandbox = custom.then(|| setup.standing.sandbox());
        args.approval = custom.then(|| setup.standing.approval());
        args.permission = (!custom).then_some(preset);
        let loaded = {
            let setup = Arc::clone(&setup);
            let args = args.clone();
            tokio::task::spawn_blocking(move || {
                let mut session = match source {
                    crate::subagents::ChildSource::New { id, label } => {
                        crate::subagents::new_child_store(
                            &setup.cwd,
                            &setup.place,
                            setup.persisted,
                            &setup.parent_id,
                            setup.parent_file.as_deref(),
                            &id,
                            &label,
                        )?
                    }
                    crate::subagents::ChildSource::Open { path } => SessionManager::open(
                        &path,
                        Some(&crate::subagents::children_dir(&setup.place)),
                        None,
                    )
                    .map_err(|error| error.to_string())?,
                };
                let mut warnings = Vec::new();
                let factory = &setup.factory;
                let resources = load_resources(
                    &mut session,
                    &setup.settings,
                    &args,
                    &factory.home,
                    factory.user_home.clone(),
                    &factory.plugins,
                    true,
                    &mut warnings,
                )?;
                Ok::<_, String>((session, resources))
            })
            .await
            .map_err(|error| format!("Subagent setup failed: {error}"))??
        };
        let (session, resources) = loaded;
        let factory = &setup.factory;
        let Built { session, servers } = build_session(
            &args,
            Build {
                session,
                apis: Arc::clone(&setup.apis),
                resources,
                // Reminders belong to the user's session, not to a subagent.
                settings: Settings {
                    schedules: false,
                    ..setup.settings.clone()
                },
                registry: setup.registry.clone(),
                model: route.as_ref().map(|route| route.model.clone()),
                thinking_level: route.and_then(|route| route.thinking_level),
                home: factory.home.clone(),
                docs_command: factory.docs_command.clone(),
                sandbox_provider: Arc::clone(&factory.sandbox_provider),
                approver: factory.approver.clone(),
                telecom_env: factory.telecom_env.clone(),
                role: Role::Child(link),
                drawn: false,
            },
        )
        .await?;
        if let Err(error) = start_servers(&session, servers) {
            session.shutdown().await;
            return Err(error);
        }
        Ok(session)
    })
}

/// Pi's `_buildRuntime` tools, `createAgentSession` tool selection, and
/// the system prompt, then the session itself.
pub(super) async fn build_session(args: &Args, build: Build) -> Result<Built, String> {
    let Build {
        session,
        apis,
        resources,
        mut settings,
        registry,
        model,
        thinking_level,
        home,
        docs_command,
        sandbox_provider,
        approver,
        telecom_env,
        role,
        drawn,
    } = build;
    let Resources {
        context_files,
        mcp_servers,
        skills,
        prompt_templates,
        permission,
        custom_prompt,
        append_system_prompt,
        plugins,
        hooks,
    } = resources;
    let session_cwd = session.cwd().to_owned();
    let session_id = session.session_id().to_owned();
    // A subagent's prompts come from its parent's tool calls, so it runs
    // only the tool hooks.
    let hook_scope = match role {
        Role::Child(_) => crate::hooks::HookScope::ToolsOnly,
        Role::Root(_) => crate::hooks::HookScope::Session,
    };
    let can_ask = approver.is_some();
    // Someone answers a top-level session with an approver: a terminal, or
    // an RPC client. A subagent's questions reach no one.
    let run_mode = if can_ask && matches!(role, Role::Root(_)) {
        crate::system_prompt::RunMode::Interactive
    } else {
        crate::system_prompt::RunMode::Print
    };
    let approver: Arc<dyn Approver> = approver.unwrap_or_else(|| Arc::new(NoApprover));
    // One standing permission for the tools, the MCP servers' admission,
    // and the hooks' confinement.
    let standing = Arc::new(permission.standing(Path::new(&session_cwd), Some(session_id.clone())));
    // The session grants its file records apply again, where someone can
    // still answer; without an answerer, as in `-p`, no grant applies.
    if can_ask {
        for grant in crate::permissions::approval_history(&session).session_grants() {
            standing.grant(grant);
        }
    }
    let tool_permissions = ToolPermissions::new(
        Arc::clone(&standing),
        Arc::clone(&approver),
        Arc::clone(&sandbox_provider),
    );
    // As the prompt states `never` without an answerer, the tools offer no
    // escalation that would fail.
    let tool_permissions = if can_ask {
        tool_permissions
    } else {
        tool_permissions.without_escalation()
    };
    let session_file = session
        .session_file()
        .map(|file| file.to_string_lossy().into_owned());
    // The read tool asks for the session's model when it runs, and bash
    // reads the model and thinking level for its `BAKE_*` variables when
    // each call starts; both read what the session publishes now, so a
    // model change reaches the next call.
    let published = crate::agent_session::SessionSelection::default();
    // The session's owner of its work, given to the tools so that shutdown
    // waits for their blocking work and every tree they start.
    let work = crate::owned_work::WorkGate::default();
    // The session's background jobs, shared by `bash`, the job tools, and
    // the session that delivers their notices.
    let jobs = crate::jobs::JobRegistry::new(work.clone(), crate::jobs::JobLimits::default());
    // How long a `bash` call without `timeout` waits before its command
    // continues as a job; an invalid value stops the start.
    let foreground_budget = settings.bash_foreground_budget()?;
    // What the skills list may take of the prompt; likewise.
    let skills_listing_tokens = settings.skills_listing_tokens()?;
    let bash_context: crate::tools::bash::BashToolContextFn = {
        let selection = published.clone();
        let session_cwd = PathBuf::from(&session_cwd);
        Arc::new(move || {
            let selected = selection.get();
            crate::tools::bash::BashToolContext {
                cwd: Some(session_cwd.clone()),
                session_id: session_id.clone(),
                session_file: session_file.clone(),
                thinking_level: Some(selected.thinking_level.as_str().to_owned()),
                model: selected.model.map(|model| crate::tools::bash::BashModel {
                    provider: model.provider,
                    id: model.id,
                }),
            }
        })
    };
    let tools_options = ToolsOptions {
        read: ReadToolOptions {
            auto_resize_images: settings.image_auto_resize(),
            resize_options: None,
            model: Some({
                let selection = published.clone();
                Arc::new(move || selection.get().model)
            }),
        },
        bash: Some(sandboxed_bash_tool(
            Path::new(&session_cwd),
            crate::tools::bash::BashToolOptions {
                context: Some(bash_context.clone()),
                owner: Some(work.clone()),
                foreground_budget,
                ..Default::default()
            },
            &tool_permissions,
        )?),
        owner: Some(work.clone()),
        jobs: Some(jobs.clone()),
        web: Some(crate::tools::web_tools::WebTools {
            fetcher: crate::web::fetch::HttpFetcher::new(
                crate::web::fetch::HttpFetchLimits::default(),
                Some(work.clone()),
            ),
            search: crate::web::search::CliProxySearch::new(
                registry.clone(),
                Some(published.clone()),
                settings.web_search_model.clone(),
                Some(work.clone()),
            ),
        }),
    };
    let mut definitions = owned_sandbox_file_tools(
        create_bake_tool_definitions(Path::new(&session_cwd), &tools_options),
        Path::new(&session_cwd),
        &tool_permissions,
        Some(work.clone()),
    );
    // With `subagents` on, a top-level session starts subagents built as it
    // is; a subagent messages its parent and starts none (depth 1).
    let mut subagent_host = None;
    // Only a top-level session joins its repository's telecom team; an
    // in-process subagent talks to its parent through its own tools.
    let telecom =
        (matches!(role, Role::Root(_)) && telecom_env.enabled(settings.telecom)).then(|| {
            let options = telecom_env
                .station_options(
                    Some(&home),
                    Path::new(&session_cwd),
                    session.session_id(),
                    model
                        .as_ref()
                        .map(|model| format!("{}/{}", model.provider, model.id)),
                    settings.callsigns.clone(),
                )
                .ok_or_else(|| "telecom: no Bake home to place the team in".to_owned())?;
            crate::telecom::Station::start(options, jobs.clone(), &work)
        });
    if let Some(Ok(station)) = &telecom {
        definitions.push(crate::telecom::tool::tool(station.clone()));
    }
    // A top-level session with someone to answer (a terminal, or an RPC
    // client such as a parent Bake) can put questions to the person; the
    // tool is one of its defaults, by the owner's decision, though no
    // paired evaluation has measured it.
    let questions =
        (matches!(role, Role::Root(_)) && can_ask).then(crate::questions::QuestionBroker::new);
    if let Some(broker) = &questions {
        definitions.push(crate::questions::tool(broker.clone()));
    }
    // Agents run as headless processes of this executable and talk through
    // telecom, so a top-level session with a station spawns them; a
    // spawned agent spawns none.
    let spawned = match &telecom {
        Some(Ok(station)) if !telecom_env.spawned => std::env::current_exe().ok().map(|program| {
            let selection = published.clone();
            crate::spawned::SpawnHost::new(crate::spawned::HostOptions {
                program,
                cwd: PathBuf::from(&session_cwd),
                station: Some(station.clone()),
                jobs: jobs.clone(),
                work: work.clone(),
                questions: questions.clone(),
                standing: Arc::clone(&standing),
                model: Arc::new(move || {
                    selection
                        .get()
                        .model
                        .map(|model| format!("{}/{}", model.provider, model.id))
                }),
                stop_grace: crate::spawned::STOP_GRACE,
            })
        }),
        _ => None,
    };
    if let Some(host) = &spawned {
        definitions.extend(crate::spawned::tools(host));
    }
    let subagent_tools: &[&str] = match role {
        Role::Root(Some(factory)) if settings.subagents => {
            let parent_file = session
                .is_persisted()
                .then(|| session.session_file().map(Path::to_path_buf))
                .flatten();
            let host = crate::subagents::SubagentHost::new(crate::subagents::HostOptions {
                parent_id: session.session_id().to_owned(),
                parent_file: parent_file.clone(),
                jobs: jobs.clone(),
                work: work.clone(),
                selection: published.clone(),
                registry: registry.clone(),
                settings: settings.clone(),
                builder: child_builder(ChildSetup {
                    factory: *factory,
                    apis: Arc::clone(&apis),
                    registry: registry.clone(),
                    settings: settings.clone(),
                    cwd: session_cwd.clone(),
                    place: session.session_dir().to_path_buf(),
                    persisted: session.is_persisted(),
                    parent_id: session.session_id().to_owned(),
                    parent_file,
                    permission: permission.clone(),
                    standing: Arc::clone(&standing),
                }),
                recovered: crate::subagents::recover(&session),
            });
            let model_choice = registry.snapshot().available().len() > 1;
            definitions.extend(crate::subagents::parent_tools(&host, model_choice));
            subagent_host = Some(host);
            // The job tools say a subagent's id is not a job id.
            let job_tools =
                crate::tools::job_tools::create_job_tool_definitions_with_subagents(&jobs);
            for definition in &mut definitions {
                if let Some(replacement) = job_tools
                    .iter()
                    .find(|tool| tool.name() == definition.name())
                {
                    *definition = replacement.clone();
                }
            }
            &crate::subagents::PARENT_TOOL_NAMES
        }
        Role::Root(_) => &[],
        Role::Child(link) => {
            definitions.extend(crate::subagents::child_tools(&link));
            &crate::subagents::CHILD_TOOL_NAMES
        }
    };
    // Two ways to delegate: the rules say which is for what.
    if spawned.is_some() && subagent_host.is_some() {
        for definition in &mut definitions {
            if definition.name() == crate::spawned::TOOL_NAMES[0] {
                definition.prompt_guidelines =
                    Some(vec![crate::spawned::WITH_SUBAGENTS_GUIDELINE.to_owned()]);
            }
        }
    }
    let mut available: Vec<String> = definitions
        .iter()
        .map(|definition| definition.name().to_owned())
        .collect();
    if settings.task_documents {
        available.push(crate::agent_session::task_documents::NAME.to_owned());
    }
    if settings.schedules {
        available.extend(crate::agent_session::schedules::NAMES.map(str::to_owned));
    }
    // The session's own `codemode` tool, a Bake default where Pi leaves it
    // inactive: `--tools` without it, `--no-tools`, `--exclude-tools
    // codemode`, a `-codemode` entry, or a `defaultTools` list of names that
    // omits it turns it off.
    available.push(crate::tools::codemode::CODEMODE_TOOL_NAME.to_owned());
    // A model profile's `tools` name tools Bake has in some session; one
    // that names no such tool stops the start, as any malformed profile
    // does. A known tool this session lacks is left out when it applies.
    // An explicit selection wins over the profiles, as over `defaultTools`.
    settings.explicit_tools = args.tools.is_some() || args.no_tools || args.no_builtin_tools;
    // Without context files, none is found while working either.
    settings.no_context_files = args.no_context_files;
    {
        let mut known = available.clone();
        known.extend(
            [
                crate::agent_session::task_documents::NAME,
                crate::telecom::tool::TOOL_NAME,
                crate::questions::TOOL_NAME,
            ]
            .into_iter()
            .chain(crate::agent_session::schedules::NAMES)
            .chain(crate::subagents::PARENT_TOOL_NAMES)
            .chain(crate::subagents::CHILD_TOOL_NAMES)
            .chain(crate::spawned::TOOL_NAMES)
            .map(str::to_owned),
        );
        for (pattern, profile) in settings.model_profiles()? {
            if let Some(name) = profile.unknown_tool(&known) {
                return Err(format!(
                    "Invalid modelProfiles[\"{pattern}\"].tools setting: unknown tool \"{name}\"."
                ));
            }
        }
    }
    let mut bake_defaults: Vec<String> = crate::tools::index::BAKE_DEFAULT_TOOL_NAMES
        .map(str::to_owned)
        .to_vec();
    if settings.task_documents {
        bake_defaults.push(crate::agent_session::task_documents::NAME.to_owned());
    }
    bake_defaults.extend(subagent_tools.iter().map(|name| (*name).to_owned()));
    if matches!(telecom, Some(Ok(_))) {
        bake_defaults.push(crate::telecom::tool::TOOL_NAME.to_owned());
    }
    if spawned.is_some() {
        bake_defaults.extend(crate::spawned::TOOL_NAMES.map(str::to_owned));
    }
    if questions.is_some() {
        bake_defaults.push(crate::questions::TOOL_NAME.to_owned());
    }
    // The interactive terminal draws what a top-level session shows, so
    // there the `visualize` tool is one of its defaults unless the
    // `visualize` setting is false: the owner's decision of 2026-11-14.
    // Print and RPC runs draw nothing, so they leave it opt-in.
    if questions.is_some() && drawn && settings.visualize {
        bake_defaults.push(crate::tools::visualize::VISUALIZE_TOOL_NAME.to_owned());
    }
    if settings.schedules {
        bake_defaults.extend(crate::agent_session::schedules::NAMES.map(str::to_owned));
    }
    // A `defaultTools` list of names replaces Bake's defaults; one of only
    // `+name` and `-name` entries changes them.
    let defaults = match settings.default_tools.as_deref() {
        Some(entries) => crate::tools::index::resolve_default_tools_over(entries, &bake_defaults),
        None => bake_defaults,
    };
    let selection_options = ToolSelectionOptions {
        tools: args.tools.clone(),
        exclude_tools: args.exclude_tools.clone(),
        no_tools: if args.no_tools {
            Some(NoTools::All)
        } else if args.no_builtin_tools {
            Some(NoTools::Builtin)
        } else {
            None
        },
    };
    let selection = select_tools(&selection_options, Some(&defaults), &available)?;
    // The job tools control `bash`'s background jobs, their only producer:
    // without `bash` they are not active.
    let mut selection = selection;
    if !selection.active.iter().any(|name| name == "bash") {
        selection
            .active
            .retain(|name| !crate::tools::job_tools::JOB_TOOL_NAMES.contains(&name.as_str()));
    }
    let mut registered: Vec<_> = definitions
        .into_iter()
        .filter(|definition| {
            selection
                .registered
                .iter()
                .any(|name| name == definition.name())
        })
        .collect();
    // `web_search` names `web_fetch` only when the model can call it.
    if !selection.active.iter().any(|name| name == "web_fetch") {
        for definition in &mut registered {
            if definition.name() == "web_search"
                && let Some(web) = &tools_options.web
            {
                *definition =
                    crate::tools::web_tools::web_search_tool_definition(web.search.clone(), false);
            }
        }
    }
    // `bash` takes `run_in_background` only when the model can read the
    // job it starts.
    if selection.active.iter().any(|name| name == "job_output") {
        let background = sandboxed_bash_tool(
            Path::new(&session_cwd),
            crate::tools::bash::BashToolOptions {
                context: Some(bash_context),
                owner: Some(work.clone()),
                jobs: Some(jobs.clone()),
                foreground_budget,
                ..Default::default()
            },
            &tool_permissions,
        )?;
        for definition in &mut registered {
            if definition.name() == "bash" {
                *definition = background.clone();
            }
        }
    }

    // The session header's start, so a resumed session states the same
    // date and patches nothing.
    let started_ms = session
        .header()
        .and_then(|header| header.timestamp())
        .and_then(parse_date_ms)
        .unwrap_or_else(bake_ai::now_ms);
    let mut system_prompt = SystemPromptOptions::new(session_cwd.clone());
    system_prompt.custom_prompt = custom_prompt;
    if let Some(append) = append_system_prompt {
        system_prompt.append_system_prompt = append;
    }
    system_prompt.context_files = context_files;
    system_prompt.skills = skills;
    system_prompt.skills_listing_tokens = skills_listing_tokens;
    // Without an approver, escalations fail closed, and the prompt
    // states the `never` policy whatever the session's.
    system_prompt.permissions = permission.prompt_text(Path::new(&session_cwd), can_ask);
    system_prompt.run_mode = run_mode;
    system_prompt.environment = Environment::detect(Path::new(&session_cwd), started_ms);
    let mut session = AgentSession::create(AgentSessionOptions {
        session,
        registry,
        apis,
        settings,
        settings_store: Some(SettingsStore::new(&home)),
        selection: Some(published),
        model,
        thinking_level,
        tools: registered
            .iter()
            .map(|definition| Arc::new(definition.tool.clone()))
            .collect(),
        active_tool_names: selection.active,
        tool_prompts: registered
            .iter()
            .map(|definition| definition.prompt())
            .collect(),
        system_prompt,
        docs_command,
        plugins,
        work: Some(work),
        jobs: Some(jobs),
    })
    .await?;
    // Before any prompt: each approval request and its answer is written to
    // this session's file before the call can run.
    if let Err(message) = session.record_approvals(tool_permissions.engine.recorder_slot()) {
        session.shutdown().await;
        return Err(message);
    }
    session.set_prompt_templates(prompt_templates);
    if let Some(host) = subagent_host {
        session.set_subagents(host);
    }
    if let Some(telecom) = telecom {
        session.set_telecom(telecom);
    }
    if let Some(broker) = questions {
        session.set_questions(broker);
    }
    if let Some(host) = spawned {
        session.set_spawned(host);
    }
    session.set_permission(crate::permissions::LivePermission {
        table: permission.table.clone(),
        standing: Arc::clone(&standing),
        can_ask,
    });
    if let Err(message) = session.set_hooks(crate::hooks::HooksOptions {
        files: hooks,
        scope: hook_scope,
        standing: Arc::clone(&standing),
        sandbox: Arc::clone(&sandbox_provider),
        approver,
        default_timeout: crate::hooks::DEFAULT_HOOK_TIMEOUT,
    }) {
        session.shutdown().await;
        return Err(message);
    }
    // The same restrictions the built-in tools had, for MCP tools whenever
    // they are published. With nothing declarable (`--no-tools` alone) no
    // server is started: none of its tools could be declared or called.
    let (allow, exclude) = crate::tools::index::dynamic_tool_restrictions(&selection_options);
    let tool_filter = crate::mcp::McpToolFilter::new(allow, exclude);
    let servers =
        (!mcp_servers.is_empty() && !tool_filter.allows_none()).then(|| crate::mcp::McpOptions {
            servers: mcp_servers,
            standing,
            sandbox: sandbox_provider,
            timings: crate::mcp::McpTimings::default(),
            tool_filter,
            http: crate::mcp::McpHttpOptions::default(),
            resolver: None,
        });
    // Checked now, so starting them later can fail only if the session has
    // been shut down.
    if let Some(options) = &servers
        && let Err(message) = crate::mcp::validate(options)
    {
        session.shutdown().await;
        return Err(message);
    }
    Ok(Built { session, servers })
}

/// A session [`build_session`] built, and the MCP servers it is to start.
pub(super) struct Built {
    pub(super) session: AgentSession,
    pub(super) servers: Option<crate::mcp::McpOptions>,
}

/// Starts `servers` for `session` ([`AgentSession::start_mcp`]). From then
/// on the session owns them: whoever holds it shuts it down, which stops
/// them and waits until they are gone.
pub(super) fn start_servers(
    session: &AgentSession,
    servers: Option<crate::mcp::McpOptions>,
) -> Result<(), String> {
    match servers {
        Some(options) => session.start_mcp(options).map(drop),
        None => Ok(()),
    }
}
