mod config;

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use astock_agent_runtime::{
    AgentEvent, AgentRuntime, EngineGateway, MinimaxProvider, RuntimeConfig, RuntimeError,
    RuntimeSession, RuntimeTask, SessionManager, SessionMessageRole, SessionRunOutcome,
    SessionSummary, UserIntent,
};
use astock_engine::Engine;
use astock_minimax::{
    KeyStore, MinimaxClient, ModelCatalog, Region, RegionDetector, ReqwestHttp, SecretKey,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use config::{AppConfig, AppPaths};
use tokio::io::{AsyncBufReadExt, AsyncReadExt};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "astock",
    version,
    about = "Evidence-driven financial research Agent",
    subcommand_negates_reqs = true
)]
struct Cli {
    /// Read ordinary configuration from this TOML file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Diagnostic log filter (for example: info or astock_agent_runtime=debug).
    #[arg(long, global = true, default_value = "warn")]
    log_level: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Ask one question and exit.
    Ask(AskCommand),
    /// Start the inline, scrollback-friendly interactive Agent.
    Chat,
    /// Resume the latest or selected durable research session.
    Resume {
        /// Durable session ID. Omit to resume the most recently updated session.
        session_id: Option<String>,
    },
    /// List durable research sessions.
    Sessions {
        /// Maximum number of sessions to return.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Search session titles and IDs locally.
        #[arg(long)]
        query: Option<String>,
        /// Emit a JSON array.
        #[arg(long)]
        json: bool,
    },
    /// Show messages from the latest or selected durable session.
    History {
        /// Durable session ID. Omit to inspect the most recently updated session.
        session_id: Option<String>,
        /// Emit the stored session as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Branch the latest or selected durable session without changing its history.
    Branch {
        /// Source session ID. Omit to branch the most recently updated session.
        session_id: Option<String>,
        /// Branch at this message ID. Omit to use the latest message.
        #[arg(long)]
        message_id: Option<String>,
        /// Optional title for the new branch.
        #[arg(long)]
        title: Option<String>,
        /// Emit the stored branch as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Refresh the bounded model-context summary without deleting full history.
    Compact {
        /// Durable session ID. Omit to compact the most recently updated session.
        session_id: Option<String>,
        /// Emit the stored session as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Inspect or validate the configuration file.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Diagnose paths, credentials and terminal capabilities without showing secrets.
    Doctor,
    /// List the allowlisted financial tools.
    Tools,
    /// List recent versioned source documents from the local evidence archive.
    Sources {
        /// Maximum number of source documents to return.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Emit a JSON array.
        #[arg(long)]
        json: bool,
    },
    /// Show deterministic local cache/storage counters.
    Cache {
        /// Emit a JSON object.
        #[arg(long)]
        json: bool,
    },
    /// List models available to the configured MiniMax account.
    Models,
    /// Show the configured MiniMax Token Plan quota as JSON.
    Quota,
    /// Print detailed build version information.
    Version,
}

#[derive(Debug, Args)]
struct AskCommand {
    /// Question text, or '-' to read UTF-8 text from stdin.
    query: String,
    /// Optional six-digit A-share symbol supplied as structured context.
    #[arg(long)]
    symbol: Option<String>,
    /// Override research depth for this invocation.
    #[arg(long, value_enum)]
    depth: Option<Depth>,
    /// Emit one final JSON object.
    #[arg(long, conflicts_with = "jsonl")]
    json: bool,
    /// Stream typed Agent events as JSON Lines.
    #[arg(long, conflicts_with = "json")]
    jsonl: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Depth {
    Fast,
    Balanced,
    Deep,
    Exhaustive,
}

impl Depth {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::Deep => "deep",
            Self::Exhaustive => "exhaustive",
        }
    }
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print the selected configuration path.
    Path,
    /// Parse and validate the selected configuration file.
    Validate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Plain,
    Json,
    Jsonl,
}

/// Stack size for the thread that drives the async entry point.
///
/// Windows gives a process's main thread a 1 MiB stack, while Linux and macOS
/// give 8 MiB. The Engine's initialization future is a large async state
/// machine, and on Windows it overflowed that 1 MiB main stack: every command,
/// including read-only ones like `astock sources`, aborted with
/// `STATUS_STACK_OVERFLOW` (`0xC00000FD`). The failure was invisible on Linux
/// and macOS purely because of their larger default.
///
/// Rather than depend on a platform default, drive the runtime from a thread
/// with an explicit stack and give tokio's workers the same, so behaviour is
/// identical on every supported platform.
const MAIN_STACK_BYTES: usize = 16 * 1024 * 1024;

fn main() -> ExitCode {
    match std::thread::Builder::new()
        .name("astock-main".to_owned())
        .stack_size(MAIN_STACK_BYTES)
        .spawn(run)
    {
        Ok(handle) => match handle.join() {
            Ok(code) => code,
            Err(_) => {
                eprintln!("astock: the main worker thread terminated abnormally");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("astock: could not start the main worker thread: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(MAIN_STACK_BYTES)
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("astock: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        let cli = Cli::parse();
        init_tracing(&cli.log_level);
        match dispatch(cli).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("astock: {error}");
                ExitCode::from(exit_code(&error))
            }
        }
    })
}

async fn dispatch(cli: Cli) -> Result<(), RuntimeError> {
    let paths = AppPaths::discover(cli.config.as_deref()).map_err(RuntimeError::Configuration)?;
    if matches!(
        cli.command,
        Some(Command::Config {
            command: ConfigCommand::Path
        })
    ) {
        println!("{}", paths.config_file.display());
        return Ok(());
    }
    let config = AppConfig::load(&paths.config_file).map_err(RuntimeError::Configuration)?;
    config.validate().map_err(RuntimeError::Configuration)?;

    match cli.command {
        Some(Command::Ask(command)) => ask(command, &config, &paths).await,
        Some(Command::Chat) | None => interactive(&config, &paths).await,
        Some(Command::Resume { session_id }) => {
            resume(session_id.as_deref(), &config, &paths).await
        }
        Some(Command::Sessions { limit, query, json }) => {
            list_sessions(&paths, limit, query.as_deref(), json).await
        }
        Some(Command::History { session_id, json }) => {
            history(&paths, session_id.as_deref(), json).await
        }
        Some(Command::Branch {
            session_id,
            message_id,
            title,
            json,
        }) => {
            branch(
                &paths,
                session_id.as_deref(),
                message_id.as_deref(),
                title.as_deref(),
                json,
            )
            .await
        }
        Some(Command::Compact { session_id, json }) => {
            compact(&paths, session_id.as_deref(), json).await
        }
        Some(Command::Config {
            command: ConfigCommand::Validate,
        }) => {
            println!("configuration valid: {}", paths.config_file.display());
            Ok(())
        }
        Some(Command::Config {
            command: ConfigCommand::Path,
        }) => unreachable!("handled before configuration loading"),
        Some(Command::Doctor) => doctor(&config, &paths),
        Some(Command::Tools) => {
            let registry = astock_agent_runtime::default_registry();
            for name in registry.names() {
                let tool = registry.get(name).expect("listed tool exists");
                println!("{:<26} {} [{}]", name, tool.description, tool.freshness);
            }
            Ok(())
        }
        Some(Command::Sources { limit, json }) => sources(&paths, limit, json).await,
        Some(Command::Cache { json }) => cache(&paths, json).await,
        Some(Command::Models) => {
            let client = minimax_client(&config)?;
            let models = client.available_models().await.map_err(map_minimax_error)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&models).map_err(RuntimeError::Json)?
            );
            Ok(())
        }
        Some(Command::Quota) => {
            let client = minimax_client(&config)?;
            let quota = client.quota().await.map_err(map_minimax_error)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&quota).map_err(RuntimeError::Json)?
            );
            Ok(())
        }
        Some(Command::Version) => {
            println!("astock {}", env!("CARGO_PKG_VERSION"));
            println!("agent-runtime rust-agent-runtime-v1");
            println!("engine {}", astock_engine::ENGINE_VERSION);
            println!("target {}-{}", std::env::consts::OS, std::env::consts::ARCH);
            Ok(())
        }
    }
}

async fn ask(
    command: AskCommand,
    config: &AppConfig,
    paths: &AppPaths,
) -> Result<(), RuntimeError> {
    let query = if command.query == "-" {
        let mut input = String::new();
        tokio::io::stdin()
            .read_to_string(&mut input)
            .await
            .map_err(|error| RuntimeError::Configuration(format!("read stdin: {error}")))?;
        input
    } else {
        command.query
    };
    let mut task = RuntimeTask::ask(query);
    task.symbol = command.symbol;
    task.depth = command
        .depth
        .map(Depth::as_str)
        .unwrap_or(&config.agent.depth)
        .to_owned();
    task.tool_policy = config.agent.tool_policy.clone();
    task.language = config.agent.language.clone();
    let mode = if command.json {
        OutputMode::Json
    } else if command.jsonl {
        OutputMode::Jsonl
    } else {
        OutputMode::Plain
    };
    let (runtime, _) = build_runtime(config, paths).await?;
    let session = RuntimeSession::new(task.depth.clone(), task.tool_policy.clone());
    run_session_task(
        &runtime,
        session,
        task,
        mode,
        std::io::stdout().is_terminal(),
    )
    .await
    .map(|_| ())
}

async fn interactive(config: &AppConfig, paths: &AppPaths) -> Result<(), RuntimeError> {
    interactive_with_session(config, paths, None).await
}

async fn resume(
    session_id: Option<&str>,
    config: &AppConfig,
    paths: &AppPaths,
) -> Result<(), RuntimeError> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(RuntimeError::Configuration(
            "resume requires a TTY; use `astock history --json` for non-interactive inspection"
                .into(),
        ));
    }
    let manager = build_session_manager(paths).await?;
    let stored = match session_id {
        Some(session_id) => manager.load(session_id).await?,
        None => manager.latest().await?.ok_or_else(|| {
            RuntimeError::Configuration("there is no durable session to resume".into())
        })?,
    };
    interactive_with_session(config, paths, Some(stored.session)).await
}

async fn interactive_with_session(
    config: &AppConfig,
    paths: &AppPaths,
    restored: Option<RuntimeSession>,
) -> Result<(), RuntimeError> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(RuntimeError::Configuration(
            "interactive mode requires a TTY; use `astock ask -` for piped input".into(),
        ));
    }
    // The Engine, sessions and every control intent work without a model
    // credential. The provider is attached on the first research request, so a
    // user can launch `astock`, look around and ask for help without being
    // asked for a secret first.
    let (engine, gateway, sessions) = build_engine_stack(paths).await?;
    let mut runtime: Option<AgentRuntime> = None;
    let mut session = restored
        .unwrap_or_else(|| RuntimeSession::new(&config.agent.depth, &config.agent.tool_policy));
    println!("AStock Analyst");
    println!(
        "MiniMax · {} · live-data · strict-evidence · session {}  (/help for commands)",
        session.depth, session.session_id
    );
    if !session.messages.is_empty() {
        println!(
            "resumed: {} · {} messages · phase={}",
            session.title,
            session.messages.len(),
            session
                .task
                .as_ref()
                .map(|task| task.phase.as_str())
                .unwrap_or("idle")
        );
    }
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    loop {
        print!("\n> ");
        std::io::stdout()
            .flush()
            .map_err(|error| RuntimeError::Internal(error.to_string()))?;
        let Some(line) = lines
            .next_line()
            .await
            .map_err(|error| RuntimeError::Internal(format!("read terminal input: {error}")))?
        else {
            println!();
            return Ok(());
        };
        let line = line.trim();
        // Every input, slash or conversational, is resolved once into
        // canonical intent. The adapter renders results; it does not decide
        // what the user meant. Adding a slash-only branch here would recreate
        // the second control plane this indirection exists to prevent.
        let intent = UserIntent::interpret(line);
        match intent {
            UserIntent::Research { prompt } if prompt.is_empty() => continue,
            UserIntent::Exit => return Ok(()),
            UserIntent::Help => {
                println!("{}", shortcut_help());
                continue;
            }
            UserIntent::ClearScreen => {
                // Presentation only: durable Agent truth is untouched.
                print!("\x1b[2J\x1b[H");
                std::io::stdout()
                    .flush()
                    .map_err(|error| RuntimeError::Internal(error.to_string()))?;
                continue;
            }
            UserIntent::NewSession => {
                session = RuntimeSession::new(&config.agent.depth, &config.agent.tool_policy);
                println!("new session: {}", session.session_id);
                continue;
            }
            UserIntent::ListSessions => {
                print_session_summaries(&sessions.list(20, None).await?);
                continue;
            }
            UserIntent::ShowHistory => {
                print_session_history(&session);
                continue;
            }
            UserIntent::Compact => {
                if session.refresh_compacted_summary() {
                    sessions.save(&session).await?;
                    println!(
                        "context compacted: full_history={} model_history={} summary_chars={}",
                        session.messages.len(),
                        session.model_history().len(),
                        session
                            .summary
                            .as_ref()
                            .map(|summary| summary.chars().count())
                            .unwrap_or(0)
                    );
                } else {
                    println!("context is within the bounded model-history window");
                }
                continue;
            }
            UserIntent::ShowPlan => {
                match session.task.as_ref().and_then(|task| task.plan.as_ref()) {
                    Some(plan) => println!("{}", plan.render_plain()),
                    None => println!("no active research plan"),
                }
                continue;
            }
            UserIntent::ListTools => {
                // The registry is a static capability list, so it needs no
                // provider and therefore no credential.
                println!(
                    "{}",
                    astock_agent_runtime::default_registry()
                        .names()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                continue;
            }
            UserIntent::ShowSources => {
                sources(paths, 20, false).await?;
                continue;
            }
            UserIntent::ShowCache => {
                cache(paths, false).await?;
                continue;
            }
            UserIntent::ShowEvidence => {
                let ids = session
                    .task
                    .as_ref()
                    .map(|task| task.evidence_ids.as_slice())
                    .unwrap_or_default();
                if ids.is_empty() {
                    println!("no evidence recorded in the current session task");
                } else {
                    println!("{}", ids.join("\n"));
                }
                continue;
            }
            UserIntent::ShowContext => {
                let prompt_history = session.model_history();
                println!(
                    "session={} messages={} model_history={} summary_chars={} evidence={} title={}",
                    session.session_id,
                    session.messages.len(),
                    prompt_history.len(),
                    session
                        .summary
                        .as_ref()
                        .map(|summary| summary.chars().count())
                        .unwrap_or(0),
                    session
                        .task
                        .as_ref()
                        .map(|task| task.evidence_ids.len())
                        .unwrap_or(0),
                    session.title
                );
                continue;
            }
            UserIntent::ShowStatus => {
                println!(
                    "session={} phase={} depth={} tool_policy={} data={}",
                    session.session_id,
                    session
                        .task
                        .as_ref()
                        .map(|task| task.phase.as_str())
                        .unwrap_or("idle"),
                    session.depth,
                    session.tool_policy,
                    paths.data_dir.display()
                );
                continue;
            }
            UserIntent::Cancel => {
                // At the prompt no research is in flight, because the loop is
                // synchronous. Reporting that honestly is better than printing
                // a cancellation that did not happen. Cancelling a running
                // task is handled cooperatively inside `run_session_task`.
                let running = session
                    .task
                    .as_ref()
                    .is_some_and(|task| !task.phase.is_terminal());
                if running {
                    println!(
                        "the durable task is not executing right now; its last phase was {}",
                        session
                            .task
                            .as_ref()
                            .map(|task| task.phase.as_str())
                            .unwrap_or("idle")
                    );
                } else {
                    println!("there is no running task to cancel");
                }
                continue;
            }
            UserIntent::Resume { session_id } => {
                let stored = match session_id.as_deref() {
                    Some(identifier) => sessions.load(identifier).await?,
                    None => sessions.latest().await?.ok_or_else(|| {
                        RuntimeError::Configuration("there is no durable session to resume".into())
                    })?,
                };
                session = stored.session;
                println!(
                    "resumed: {} · {} messages · session {}",
                    session.title,
                    session.messages.len(),
                    session.session_id
                );
                continue;
            }
            UserIntent::Branch { message_id } => {
                let branched = sessions
                    .branch(&session.session_id, message_id.as_deref(), None)
                    .await?;
                session = branched.session;
                println!(
                    "branched: {} · {} messages · session {}",
                    session.title,
                    session.messages.len(),
                    session.session_id
                );
                continue;
            }
            UserIntent::SetDepth { depth } => {
                session.depth = depth.as_str().to_owned();
                if !session.messages.is_empty() {
                    sessions.save(&session).await?;
                }
                println!("depth={depth}");
                continue;
            }
            UserIntent::Research { prompt } => {
                // First research request in this process pays for the
                // credential prompt; later ones reuse the attached provider.
                if runtime.is_none() {
                    runtime = Some(build_agent_runtime(
                        config,
                        engine.as_ref(),
                        gateway.clone(),
                    )?);
                }
                let active = runtime
                    .as_ref()
                    .expect("the provider was just attached for this research request");
                let mut task = RuntimeTask::ask(&prompt);
                task.depth = session.depth.clone();
                task.tool_policy = session.tool_policy.clone();
                task.language = config.agent.language.clone();
                let session_id = session.session_id.clone();
                match run_session_task(active, session, task, OutputMode::Plain, true).await {
                    Ok(outcome) => session = outcome.session,
                    Err(error) => {
                        eprintln!("research failed: {error}");
                        session = sessions.load(&session_id).await?.session;
                    }
                }
            }
        }
    }
}

/// Shortcut surface for experienced users.
///
/// Every entry is an alias for something that can also simply be said in
/// conversation, which is why the help text advertises that rather than
/// presenting the list as the primary way to drive the product.
fn shortcut_help() -> String {
    [
        "Just type what you want in ordinary language — these are only shortcuts.",
        "",
        "  /new                    开一个新的研究会话",
        "  /resume [session]       继续之前的会话",
        "  /branch [message]       从刚才那个结论之前分一个新方向",
        "  /sessions               列出会话",
        "  /history                看一下会话历史",
        "  /compact                整理一下上下文",
        "  /plan                   给我看一下你现在准备怎么分析",
        "  /depth [fast|balanced|deep|exhaustive]   这次给我做最深入的分析",
        "  /tools                  你有哪些工具",
        "  /sources                你目前用了哪些数据源",
        "  /cache                  本地缓存情况",
        "  /evidence               把支持这个结论的证据列出来",
        "  /context                上下文用了多少",
        "  /status                 现在什么状态",
        "  /cancel                 先停一下",
        "  /clear                  clear the screen (local only)",
        "  /help                   this list",
        "  /exit                   退出",
    ]
    .join("\n")
}

/// Build the Engine, its gateway and a session manager.
///
/// None of this needs a model credential, which is what allows the interactive
/// adapter to start and answer control intents before any provider exists.
async fn build_engine_stack(
    paths: &AppPaths,
) -> Result<(Arc<Engine>, Arc<EngineGateway>, SessionManager), RuntimeError> {
    let engine = Arc::new(
        Engine::initialize_at(&paths.data_dir)
            .await
            .map_err(|error| RuntimeError::Store(format!("initialize Engine: {error}")))?,
    );
    let gateway = Arc::new(EngineGateway::new(engine.clone()));
    let sessions = SessionManager::new(gateway.clone());
    Ok((engine, gateway, sessions))
}

/// Attach the model provider to an existing Engine stack.
///
/// This is the only step that needs a MiniMax credential, and it also installs
/// the optional JoinQuant session, so it is deferred until research is actually
/// requested. Asking a user for a secret before they have asked for anything
/// that needs one is both hostile and unnecessary.
fn build_agent_runtime(
    config: &AppConfig,
    engine: &Engine,
    gateway: Arc<EngineGateway>,
) -> Result<AgentRuntime, RuntimeError> {
    let client = minimax_client(config)?;
    let provider = Arc::new(MinimaxProvider::new(client));
    configure_joinquant_session(engine)?;
    let runtime_config = RuntimeConfig {
        max_parallel_tools: config.agent.max_parallel_tools,
        verify_reports: config.research.strict_evidence && config.research.verify_numeric_claims,
        provider_connect_timeout: std::time::Duration::from_secs(
            config.provider.minimax.timeout_secs,
        ),
        provider_idle_timeout: std::time::Duration::from_secs(config.provider.minimax.timeout_secs),
        ..RuntimeConfig::default()
    };
    Ok(AgentRuntime::new(provider, gateway.clone(), gateway).with_config(runtime_config))
}

async fn build_runtime(
    config: &AppConfig,
    paths: &AppPaths,
) -> Result<(AgentRuntime, SessionManager), RuntimeError> {
    let (engine, gateway, sessions) = build_engine_stack(paths).await?;
    let runtime = build_agent_runtime(config, engine.as_ref(), gateway)?;
    Ok((runtime, sessions))
}

async fn build_session_manager(paths: &AppPaths) -> Result<SessionManager, RuntimeError> {
    let gateway = build_inspection_gateway(paths).await?;
    Ok(SessionManager::new(gateway))
}

async fn build_inspection_gateway(paths: &AppPaths) -> Result<Arc<EngineGateway>, RuntimeError> {
    let engine = Arc::new(
        Engine::initialize_at(&paths.data_dir)
            .await
            .map_err(|error| RuntimeError::Store(format!("initialize Engine: {error}")))?,
    );
    Ok(Arc::new(EngineGateway::new(engine)))
}

async fn list_sessions(
    paths: &AppPaths,
    limit: usize,
    query: Option<&str>,
    json: bool,
) -> Result<(), RuntimeError> {
    let sessions = build_session_manager(paths).await?;
    let summaries = sessions.list(limit, query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summaries)?);
    } else {
        print_session_summaries(&summaries);
    }
    Ok(())
}

async fn history(
    paths: &AppPaths,
    session_id: Option<&str>,
    json: bool,
) -> Result<(), RuntimeError> {
    let sessions = build_session_manager(paths).await?;
    let stored = match session_id {
        Some(session_id) => sessions.load(session_id).await?,
        None => sessions.latest().await?.ok_or_else(|| {
            RuntimeError::Configuration("there is no durable session to inspect".into())
        })?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&stored)?);
    } else {
        print_session_history(&stored.session);
    }
    Ok(())
}

async fn branch(
    paths: &AppPaths,
    session_id: Option<&str>,
    message_id: Option<&str>,
    title: Option<&str>,
    json: bool,
) -> Result<(), RuntimeError> {
    let sessions = build_session_manager(paths).await?;
    let source_id = match session_id {
        Some(session_id) => session_id.to_owned(),
        None => {
            sessions
                .latest()
                .await?
                .ok_or_else(|| {
                    RuntimeError::Configuration("there is no durable session to branch".into())
                })?
                .conversation_id
        }
    };
    let stored = sessions.branch(&source_id, message_id, title).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&stored)?);
    } else {
        println!(
            "created branch {} from {} at {}",
            stored.conversation_id,
            stored
                .parent_conversation_id
                .as_deref()
                .unwrap_or(&source_id),
            stored
                .branch_from_message_id
                .as_deref()
                .unwrap_or("unknown-message")
        );
    }
    Ok(())
}

async fn compact(
    paths: &AppPaths,
    session_id: Option<&str>,
    json: bool,
) -> Result<(), RuntimeError> {
    let sessions = build_session_manager(paths).await?;
    let mut stored = match session_id {
        Some(session_id) => sessions.load(session_id).await?,
        None => sessions.latest().await?.ok_or_else(|| {
            RuntimeError::Configuration("there is no durable session to compact".into())
        })?,
    };
    let changed = stored.session.refresh_compacted_summary();
    if changed {
        stored = sessions.save(&stored.session).await?;
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&stored)?);
    } else if changed {
        println!(
            "compacted model context for {}: {} full messages, {} retained messages, {} summary characters",
            stored.conversation_id,
            stored.session.messages.len(),
            stored.session.model_history().len(),
            stored
                .session
                .summary
                .as_ref()
                .map(|summary| summary.chars().count())
                .unwrap_or(0)
        );
    } else {
        println!(
            "session {} is within the bounded model-history window",
            stored.conversation_id
        );
    }
    Ok(())
}

async fn sources(paths: &AppPaths, limit: usize, json: bool) -> Result<(), RuntimeError> {
    let gateway = build_inspection_gateway(paths).await?;
    let rows = gateway
        .recent_sources(limit)
        .await
        .map_err(RuntimeError::Store)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let Some(rows) = rows.as_array() else {
        return Err(RuntimeError::Store(
            "Engine returned a non-array source page".into(),
        ));
    };
    if rows.is_empty() {
        println!("no versioned source documents in the local evidence archive");
        return Ok(());
    }
    println!("{:<14}  {:<12}  URL", "ACCESS", "AUTHORITY");
    for row in rows {
        println!(
            "{:<14}  {:<12}  {}",
            row.get("access_status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown"),
            row.get("authority")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown"),
            row.get("canonical_url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
        );
    }
    Ok(())
}

async fn cache(paths: &AppPaths, json: bool) -> Result<(), RuntimeError> {
    let gateway = build_inspection_gateway(paths).await?;
    let stats = gateway.cache_stats().await.map_err(RuntimeError::Store)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
        return Ok(());
    }
    for (label, key) in [
        ("total", "total_bytes"),
        ("SQLite", "sqlite_bytes"),
        ("K-line Parquet", "kline_parquet_bytes"),
        ("tool cache", "tool_cache_bytes"),
        ("conversation", "chat_bytes"),
        ("disk free", "disk_free_bytes"),
    ] {
        let rendered = stats
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .map(format_bytes)
            .unwrap_or_else(|| "unknown".into());
        println!("{label:<16} {rendered}");
    }
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes_float = bytes as f64;
    if bytes_float >= GIB {
        format!("{:.2} GiB", bytes_float / GIB)
    } else if bytes_float >= MIB {
        format!("{:.1} MiB", bytes_float / MIB)
    } else if bytes_float >= KIB {
        format!("{:.1} KiB", bytes_float / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn print_session_summaries(summaries: &[SessionSummary]) {
    if summaries.is_empty() {
        println!("no durable sessions");
        return;
    }
    println!(
        "{:<36}  {:<14}  {:>4}  {:>4}  TITLE",
        "SESSION", "PHASE", "MSG", "EVID"
    );
    for summary in summaries {
        println!(
            "{:<36}  {:<14}  {:>4}  {:>4}  {}",
            summary.conversation_id,
            summary.phase,
            summary.message_count,
            summary.evidence_count,
            summary.title
        );
    }
}

fn print_session_history(session: &RuntimeSession) {
    println!(
        "{} · {} · {} messages",
        session.session_id,
        session.title,
        session.messages.len()
    );
    if session.messages.is_empty() {
        println!("no messages");
        return;
    }
    for message in &session.messages {
        let role = match message.role {
            SessionMessageRole::User => "user",
            SessionMessageRole::Agent => "agent",
            SessionMessageRole::System => "system",
            SessionMessageRole::Tool => "tool",
        };
        println!(
            "\n[{role} · {} · {}]\n{}",
            message.timestamp, message.id, message.text
        );
    }
}

fn minimax_client(config: &AppConfig) -> Result<MinimaxClient, RuntimeError> {
    let key = KeyStore::new()
        .load_key()
        .ok()
        .flatten()
        .map(Ok)
        .unwrap_or_else(prompt_minimax_key)?;
    let http: Arc<dyn astock_minimax::Http> = match config.network.proxy.as_deref() {
        Some(proxy) => Arc::new(ReqwestHttp::with_proxy(proxy).map_err(map_minimax_error)?),
        None => Arc::new(ReqwestHttp::new()),
    };
    let mut client = MinimaxClient::with_http(key, http.clone());
    let forced_region = match config.provider.minimax.region.as_str() {
        "auto" => None,
        "cn" => Some(Region::Cn),
        "intl" => Some(Region::Intl),
        _ => unreachable!("configuration validation rejects unknown MiniMax regions"),
    };
    if let Some(region) = forced_region {
        let service = region.service_info();
        client = client.with_detector(RegionDetector::with_hosts(
            http,
            vec![(region, service.www_host, service.api_host)],
        ));
    }
    if config.provider.minimax.model != "auto" {
        client = client.with_catalog(ModelCatalog::with_chain(vec![config
            .provider
            .minimax
            .model
            .clone()]));
    }
    Ok(client)
}

fn prompt_minimax_key() -> Result<SecretKey, RuntimeError> {
    if !std::io::stdin().is_terminal() {
        return Err(RuntimeError::Configuration(
            "MiniMax credential is not configured; start astock in a terminal for a hidden session prompt or install it in the OS credential store"
                .into(),
        ));
    }
    eprint!("MiniMax API key (input hidden, session only): ");
    std::io::stderr()
        .flush()
        .map_err(|error| RuntimeError::Internal(format!("flush credential prompt: {error}")))?;
    let raw = rpassword::read_password().map_err(|error| {
        RuntimeError::Configuration(format!("read hidden MiniMax credential: {error}"))
    })?;
    let key = raw.trim();
    if key.len() < 16 || key.len() > 4_096 || key.chars().any(char::is_control) {
        return Err(RuntimeError::Configuration(
            "MiniMax credential has an invalid format".into(),
        ));
    }
    Ok(SecretKey::new(key.to_owned()))
}

fn configure_joinquant_session(engine: &Engine) -> Result<(), RuntimeError> {
    if engine.joinquant_configured() {
        return Ok(());
    }
    if let Some((username, password)) = stored_joinquant_credentials() {
        return engine
            .configure_joinquant_session(username, password)
            .map_err(RuntimeError::Configuration);
    }
    if !std::io::stdin().is_terminal() {
        return Ok(());
    }
    eprintln!("JoinQuant credentials are optional and remain in this process only.");
    eprint!("JoinQuant username (press Enter to skip): ");
    std::io::stderr()
        .flush()
        .map_err(|error| RuntimeError::Internal(format!("flush credential prompt: {error}")))?;
    let mut username = String::new();
    std::io::stdin().read_line(&mut username).map_err(|error| {
        RuntimeError::Configuration(format!("read JoinQuant username: {error}"))
    })?;
    let username = username.trim();
    if username.is_empty() {
        return Ok(());
    }
    eprint!("JoinQuant password (input hidden, session only): ");
    std::io::stderr()
        .flush()
        .map_err(|error| RuntimeError::Internal(format!("flush credential prompt: {error}")))?;
    let password = rpassword::read_password().map_err(|error| {
        RuntimeError::Configuration(format!("read hidden JoinQuant password: {error}"))
    })?;
    engine
        .configure_joinquant_session(
            SecretKey::new(username.to_owned()),
            SecretKey::new(password),
        )
        .map_err(RuntimeError::Configuration)
}

async fn run_session_task(
    runtime: &AgentRuntime,
    session: RuntimeSession,
    task: RuntimeTask,
    mode: OutputMode,
    terminal: bool,
) -> Result<SessionRunOutcome, RuntimeError> {
    let mut stream = runtime.start_session_turn(session, task);
    let task_id = stream.task_id().to_owned();
    let session_id = stream.session_id().to_owned();
    let mut final_report = None;
    let mut evidence_ids = Vec::new();
    loop {
        tokio::select! {
            event = stream.recv() => {
                let Some(event) = event else { break };
                if mode == OutputMode::Jsonl {
                    println!("{}", serde_json::to_string(&serde_json::json!({
                        "session_id": &session_id,
                        "task_id": &task_id,
                        "event": event,
                    }))?);
                    continue;
                }
                match event {
                    AgentEvent::TextDelta { text } if mode == OutputMode::Plain && terminal => {
                        print!("{text}");
                        std::io::stdout().flush().map_err(|error| RuntimeError::Internal(error.to_string()))?;
                    }
                    AgentEvent::ToolStarted { tool, .. } if mode == OutputMode::Plain && terminal => {
                        println!("\n◆ {tool}");
                    }
                    AgentEvent::ToolCompleted { tool, evidence_ids, .. } if mode == OutputMode::Plain && terminal => {
                        println!("✓ {tool} · {} evidence fields", evidence_ids.len());
                    }
                    AgentEvent::ToolFailed { tool, message, .. } if mode == OutputMode::Plain && terminal => {
                        println!("! {tool}: {message}");
                    }
                    AgentEvent::VerificationStarted if mode == OutputMode::Plain && terminal => {
                        println!("\n◆ 数字与证据校验");
                    }
                    AgentEvent::Completed { report, evidence_ids: ids } => {
                        final_report = Some(report);
                        evidence_ids = ids;
                    }
                    _ => {}
                }
            }
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() {
                    stream.cancel();
                }
            }
        }
    }
    let outcome = stream.finish().await?;
    let report = final_report.unwrap_or_else(|| outcome.run.report.clone());
    let evidence_ids = if evidence_ids.is_empty() {
        outcome.run.evidence_ids.clone()
    } else {
        evidence_ids
    };
    if mode == OutputMode::Json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "session_id": &outcome.session.session_id,
                "task_id": &outcome.run.task_id,
                "status": "completed",
                "report": &report,
                "evidence_ids": &evidence_ids,
            }))?
        );
    } else if mode == OutputMode::Plain && !terminal {
        println!("{report}");
    } else if mode == OutputMode::Plain && terminal {
        println!();
    }
    Ok(outcome)
}

fn doctor(config: &AppConfig, paths: &AppPaths) -> Result<(), RuntimeError> {
    println!("version: {}", env!("CARGO_PKG_VERSION"));
    println!(
        "platform: {}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("configuration: {}", paths.config_file.display());
    println!("data: {}", paths.data_dir.display());
    println!("cache: {}", paths.cache_dir.display());
    println!("agent depth: {}", config.agent.depth);
    println!("tool policy: {}", config.agent.tool_policy);
    println!(
        "MiniMax credential: {}",
        if KeyStore::new().load_key().ok().flatten().is_some() {
            "configured via OS credential store (value hidden)"
        } else {
            "not configured (interactive runs will prompt with input hidden)"
        }
    );
    println!(
        "JoinQuant credential: {}",
        if joinquant_credential_store_configured() {
            "configured via OS credential store (values hidden)"
        } else {
            "not configured (interactive Agent runs will prompt; session only)"
        }
    );
    println!(
        "terminal: stdin={} stdout={}",
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal()
    );
    println!(
        "proxy: {}",
        config.network.proxy.as_deref().unwrap_or("not configured")
    );
    Ok(())
}

fn joinquant_credential_store_configured() -> bool {
    stored_joinquant_credentials().is_some()
}

fn stored_joinquant_credentials() -> Option<(SecretKey, SecretKey)> {
    let username = KeyStore::with_service("astock-terminal", "joinquant-username")
        .load_key()
        .ok()
        .flatten();
    let password = KeyStore::with_service("astock-terminal", "joinquant-password")
        .load_key()
        .ok()
        .flatten();
    username.zip(password)
}

fn init_tracing(filter: &str) {
    let filter = EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .without_time()
        .init();
}

fn exit_code(error: &RuntimeError) -> u8 {
    match error {
        RuntimeError::Configuration(_) => 2,
        RuntimeError::Provider(_) => 3,
        RuntimeError::Tool { .. }
        | RuntimeError::ToolTimeout { .. }
        | RuntimeError::ToolResultTooLarge { .. }
        | RuntimeError::UnknownTool(_)
        | RuntimeError::InvalidToolArguments { .. }
        | RuntimeError::Store(_) => 4,
        RuntimeError::VerificationFailed(_) => 5,
        RuntimeError::Cancelled => 130,
        RuntimeError::ModelRoundLimit(_)
        | RuntimeError::EmptyModelTurn
        | RuntimeError::Json(_)
        | RuntimeError::Internal(_) => 1,
    }
}

fn map_minimax_error(error: astock_minimax::MinimaxError) -> RuntimeError {
    use astock_agent_runtime::{ProviderError, ProviderErrorKind};
    let retryable = error.is_transient();
    let kind = match &error {
        astock_minimax::MinimaxError::Auth(_) => ProviderErrorKind::Authentication,
        astock_minimax::MinimaxError::RateLimited { .. } => ProviderErrorKind::RateLimited,
        astock_minimax::MinimaxError::QuotaExhausted { .. } => ProviderErrorKind::Quota,
        astock_minimax::MinimaxError::Network(_) => ProviderErrorKind::Network,
        astock_minimax::MinimaxError::Parse(_) => ProviderErrorKind::MalformedResponse,
        astock_minimax::MinimaxError::Api { .. } | astock_minimax::MinimaxError::KeyStore(_) => {
            ProviderErrorKind::Unavailable
        }
    };
    RuntimeError::Provider(ProviderError::new(kind, error.to_string(), retryable))
}
