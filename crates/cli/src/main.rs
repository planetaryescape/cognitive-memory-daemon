// `cm` CLI: full feature parity with the SDK MemoryAdapter.
//
// Every subcommand maps to one daemon request kind. Read commands accept
// `--json` for machine output; write commands print human-readable
// confirmation. Architecture rule §3.1 (AGENTS.md) — this crate may
// depend only on `core`, `protocol`, and `client`.

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use cognitive_memory_client::Client;
use cognitive_memory_protocol::{
    BatchMemoryEntry, BatchUpdateArgs, BridgeScope, ClearArgs, ConvertToStubArgs, CountsArgs,
    DeleteManyMemoryArgs, DeleteMemoryArgs, DiagnosticsRequest, FindFadingArgs, FindStableArgs,
    GetLinkedArgs, GetLinkedManyArgs, GetManyMemoryArgs, GetMemoryArgs, LifecycleRequest,
    LinkMemoryArgs, ListMemoryArgs, MarkSupersededArgs, MemoryRequest, MigrateToColdArgs,
    MigrateToHotArgs, MintBridgeTokenArgs, Request, Response, ResponseData, RetentionUpdate,
    SearchLexicalArgs, SearchMemoryArgs, StoreBatchArgs, StoreMemoryArgs, TickArgs,
    UnlinkMemoryArgs, UpdateMemoryArgs, UpdateRetentionArgs,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "cm", about = "Cognitive Memory CLI", version)]
struct Cli {
    /// Override the daemon socket path.
    #[arg(long, env = "COGNITIVE_MEMORY_SOCKET_PATH", global = true)]
    socket: Option<PathBuf>,

    /// Override the user namespace for the connection.
    #[arg(long, default_value = "default", global = true)]
    user_id: String,

    /// Emit JSON instead of human-readable output (read commands).
    #[arg(long, global = true)]
    json: bool,

    /// Disable auto-spawn. With this, `cm` errors if the daemon isn't running.
    #[arg(long, global = true)]
    no_spawn: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show daemon status (uptime, memory count, version).
    Status,

    /// Per-user tier counts (hot/cold/stub/total).
    Counts,

    /// Store a memory.
    Store {
        content: String,
        #[arg(long, default_value = "semantic")]
        category: String,
        #[arg(long = "type", default_value = "fact")]
        memory_type: String,
        #[arg(long, default_value = "{}")]
        metadata: String,
        /// Shorthand for `--category core` — synaptic tagging at storage
        /// (paper §3.4). Sets retention floor to 0.6 daemon-side.
        #[arg(long)]
        core: bool,
        /// Initial importance in [0.0, 1.0]. Daemon clamps to that range.
        /// Omit to use the daemon's default.
        #[arg(long)]
        importance: Option<f64>,
    },

    /// Store many memories in one call. Co-creation associations link
    /// every pair (paper §3.6).
    StoreBatch {
        contents: Vec<String>,
        #[arg(long, default_value = "semantic")]
        category: String,
        #[arg(long = "type", default_value = "fact")]
        memory_type: String,
        /// Initial weight for the auto-created bidirectional links.
        #[arg(long, default_value_t = 0.5)]
        link_weight: f64,
    },

    /// Search memories by query.
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        deep_recall: bool,
        #[arg(long)]
        hybrid: bool,
        /// Walk the association graph N hops from top hits, scoring
        /// linked memories as relevance * R^α * edge_weight. 0 = off.
        #[arg(long, default_value_t = 0)]
        graph_hops: usize,
        /// Run BFS bridge discovery between top-3 anchors, returning
        /// evidence chains in the response.
        #[arg(long)]
        bridge_discovery: bool,
    },

    /// BM25-only lexical search. Returns matching memory IDs.
    SearchLexical {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },

    /// Fetch one memory by id.
    Get { id: String },

    /// Fetch many memories by id.
    GetMany { ids: Vec<String> },

    /// List memories with optional filters.
    List {
        #[arg(long)]
        category: Option<String>,
        #[arg(long = "type")]
        memory_type: Option<String>,
        #[arg(long)]
        min_retention: Option<f64>,
        #[arg(long)]
        min_importance: Option<f64>,
        #[arg(long, default_value_t = 50)]
        limit: i64,
        #[arg(long, default_value_t = 0)]
        offset: i64,
        #[arg(long)]
        include_superseded: bool,
        #[arg(long)]
        include_cold: bool,
        #[arg(long)]
        include_stubs: bool,
    },

    /// Update one or more fields of an existing memory.
    Update {
        id: String,
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        category: Option<String>,
        #[arg(long = "type")]
        memory_type: Option<String>,
        #[arg(long)]
        metadata: Option<String>,
        #[arg(long)]
        retention_floor: Option<f64>,
        #[arg(long)]
        importance: Option<f64>,
        #[arg(long)]
        stability: Option<f64>,
        #[arg(long)]
        valid_until: Option<i64>,
    },

    /// Delete one memory.
    Delete { id: String },

    /// Delete many memories.
    DeleteMany { ids: Vec<String> },

    /// Create or strengthen a bidirectional link between two memories.
    Link {
        source_id: String,
        target_id: String,
        #[arg(long, default_value_t = 0.1)]
        strength: f64,
        #[arg(long, default_value = "explicit")]
        kind: String,
        /// Make the link directed only (source → target).
        #[arg(long)]
        directed: bool,
    },

    /// Delete a link.
    Unlink {
        source_id: String,
        target_id: String,
        #[arg(long)]
        directed: bool,
    },

    /// List memories linked from a source.
    Linked {
        source_id: String,
        #[arg(long, default_value_t = 0.0)]
        min_strength: f64,
    },

    /// List memories linked from any of the given sources.
    LinkedMany {
        source_ids: Vec<String>,
        #[arg(long, default_value_t = 0.0)]
        min_strength: f64,
    },

    /// Run a maintenance pass.
    Tick {
        #[arg(long)]
        synchronous: bool,
    },

    /// Find memories below a retention threshold.
    FindFading {
        #[arg(long)]
        max_retention: f64,
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },

    /// Find memories above stability/access thresholds.
    FindStable {
        #[arg(long)]
        min_stability: f64,
        #[arg(long)]
        min_access_count: i64,
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },

    /// Mark memories as superseded by a summary.
    MarkSuperseded {
        summary_id: String,
        ids: Vec<String>,
    },

    /// Migrate a memory to cold storage.
    Cold {
        id: String,
        #[arg(long)]
        cold_since: Option<i64>,
    },

    /// Restore a cold memory to hot.
    Hot { id: String },

    /// Convert a memory to an archival stub.
    Stub { id: String, content: String },

    /// Update one memory's retention floor.
    Retention { id: String, floor: f64 },

    /// Atomically update retention floors. Pairs are "id=floor".
    BatchRetention { pairs: Vec<String> },

    /// Delete ALL memories under the user_id. Requires --confirm.
    Clear {
        #[arg(long)]
        confirm: bool,
    },

    /// Mint a bearer token for cm-http. Token shown once.
    MintToken {
        #[arg(long, default_value = "write")]
        scope: String,
        #[arg(long, default_value_t = 30 * 24 * 3600)]
        ttl_seconds: u64,
    },

    /// Download a local LLM model GGUF into the model cache. Default
    /// model: Qwen3-4B-Instruct-2507 Q4_K_M (~2.5 GB, Apache 2.0).
    /// Used by the conflict-judge and consolidation passes when
    /// `cm config set-llm local` is configured.
    DownloadModel {
        /// Model name from the registry (default: qwen3-4b).
        #[arg(long, default_value = "qwen3-4b")]
        model: String,
    },

    /// Show the current daemon LLM configuration.
    ConfigGetLlm,

    /// Switch the daemon's LLM provider. Restart the daemon
    /// (`pkill cm-daemon` then any cm command will auto-spawn) for
    /// the change to take effect.
    ConfigSetLlm {
        /// Provider: local | openai | anthropic | none.
        provider: String,
        /// For provider=local: path to the GGUF file. Defaults to
        /// the cached path written by `cm download-model`.
        #[arg(long)]
        model_path: Option<std::path::PathBuf>,
        /// For provider=openai|anthropic: env var holding the API key.
        #[arg(long)]
        api_key_env: Option<String>,
        /// For provider=openai|anthropic: model id.
        #[arg(long)]
        model: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Local-only commands: model download + config edits. These don't
    // need a daemon connection (the daemon reads its config at next
    // startup), so handle them first and exit.
    match &cli.command {
        Command::DownloadModel { model } => {
            return run_download_model(model).await;
        }
        Command::ConfigGetLlm => {
            return run_config_get_llm();
        }
        Command::ConfigSetLlm {
            provider,
            model_path,
            api_key_env,
            model,
        } => {
            return run_config_set_llm(
                provider,
                model_path.as_deref(),
                api_key_env.as_deref(),
                model.as_deref(),
            );
        }
        _ => {}
    }

    let socket = cli.socket.clone().unwrap_or_else(default_socket_path);
    let mut client = connect_or_spawn(&socket, &cli.user_id, !cli.no_spawn)
        .await
        .with_context(|| format!("connect to daemon at {}", socket.display()))?;

    let resp = run_command(&mut client, &cli).await?;
    print_response(cli.json, &resp)?;
    Ok(())
}

async fn run_command(client: &mut Client, cli: &Cli) -> Result<Response> {
    let user = cli.user_id.clone();
    let req = match &cli.command {
        Command::Status => Request::Diagnostics(DiagnosticsRequest::Status),
        Command::Counts => {
            Request::Diagnostics(DiagnosticsRequest::Counts(CountsArgs { user_id: user }))
        }

        Command::Store {
            content,
            category,
            memory_type,
            metadata,
            core,
            importance,
        } => {
            let cat = if *core {
                "core".to_string()
            } else {
                category.clone()
            };
            Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
                user_id: user,
                content: content.clone(),
                category: cat,
                memory_type: memory_type.clone(),
                metadata: metadata.clone(),
                importance: *importance,
            }))
        }

        Command::StoreBatch {
            contents,
            category,
            memory_type,
            link_weight,
        } => {
            let entries: Vec<BatchMemoryEntry> = contents
                .iter()
                .map(|c| BatchMemoryEntry {
                    content: c.clone(),
                    category: category.clone(),
                    memory_type: memory_type.clone(),
                    metadata: "{}".to_string(),
                })
                .collect();
            Request::Memory(MemoryRequest::StoreBatch(StoreBatchArgs {
                user_id: user,
                memories: entries,
                initial_link_weight: *link_weight,
            }))
        }

        Command::Search {
            query,
            limit,
            deep_recall,
            hybrid,
            graph_hops,
            bridge_discovery,
        } => Request::Memory(MemoryRequest::Search(SearchMemoryArgs {
            user_id: user,
            query: query.clone(),
            limit: *limit,
            deep_recall: *deep_recall,
            hybrid: *hybrid,
            graph_expansion_hops: *graph_hops,
            bridge_discovery: *bridge_discovery,
        })),

        Command::SearchLexical { query, limit } => {
            Request::Memory(MemoryRequest::SearchLexical(SearchLexicalArgs {
                user_id: user,
                query: query.clone(),
                limit: *limit,
            }))
        }

        Command::Get { id } => Request::Memory(MemoryRequest::Get(GetMemoryArgs {
            user_id: user,
            id: id.clone(),
        })),

        Command::GetMany { ids } => Request::Memory(MemoryRequest::GetMany(GetManyMemoryArgs {
            user_id: user,
            ids: ids.clone(),
        })),

        Command::List {
            category,
            memory_type,
            min_retention,
            min_importance,
            limit,
            offset,
            include_superseded,
            include_cold,
            include_stubs,
        } => Request::Memory(MemoryRequest::List(ListMemoryArgs {
            user_id: user,
            categories: category.as_ref().map(|c| vec![c.clone()]),
            memory_types: memory_type.as_ref().map(|t| vec![t.clone()]),
            min_retention_floor: *min_retention,
            min_importance: *min_importance,
            created_after: None,
            created_before: None,
            limit: Some(*limit),
            offset: Some(*offset),
            include_superseded: *include_superseded,
            include_cold: *include_cold,
            include_stubs: *include_stubs,
        })),

        Command::Update {
            id,
            content,
            category,
            memory_type,
            metadata,
            retention_floor,
            importance,
            stability,
            valid_until,
        } => Request::Memory(MemoryRequest::Update(UpdateMemoryArgs {
            user_id: user,
            id: id.clone(),
            content: content.clone(),
            category: category.clone(),
            memory_type: memory_type.clone(),
            metadata: metadata.clone(),
            retention_floor: *retention_floor,
            importance: *importance,
            stability: *stability,
            valid_until: *valid_until,
        })),

        Command::Delete { id } => Request::Memory(MemoryRequest::Delete(DeleteMemoryArgs {
            user_id: user,
            id: id.clone(),
        })),

        Command::DeleteMany { ids } => {
            Request::Memory(MemoryRequest::DeleteMany(DeleteManyMemoryArgs {
                user_id: user,
                ids: ids.clone(),
            }))
        }

        Command::Link {
            source_id,
            target_id,
            strength,
            kind,
            directed,
        } => Request::Memory(MemoryRequest::Link(LinkMemoryArgs {
            user_id: user,
            source_id: source_id.clone(),
            target_id: target_id.clone(),
            strength: *strength,
            bidirectional: !directed,
            kind: kind.clone(),
        })),

        Command::Unlink {
            source_id,
            target_id,
            directed,
        } => Request::Memory(MemoryRequest::Unlink(UnlinkMemoryArgs {
            user_id: user,
            source_id: source_id.clone(),
            target_id: target_id.clone(),
            bidirectional: !directed,
        })),

        Command::Linked {
            source_id,
            min_strength,
        } => Request::Memory(MemoryRequest::GetLinked(GetLinkedArgs {
            user_id: user,
            source_id: source_id.clone(),
            min_strength: *min_strength,
        })),

        Command::LinkedMany {
            source_ids,
            min_strength,
        } => Request::Memory(MemoryRequest::GetLinkedMany(GetLinkedManyArgs {
            user_id: user,
            source_ids: source_ids.clone(),
            min_strength: *min_strength,
        })),

        Command::Tick { synchronous } => Request::Lifecycle(LifecycleRequest::Tick(TickArgs {
            synchronous: *synchronous,
        })),

        Command::FindFading {
            max_retention,
            limit,
        } => Request::Lifecycle(LifecycleRequest::FindFading(FindFadingArgs {
            user_id: user,
            max_retention: *max_retention,
            limit: *limit,
        })),

        Command::FindStable {
            min_stability,
            min_access_count,
            limit,
        } => Request::Lifecycle(LifecycleRequest::FindStable(FindStableArgs {
            user_id: user,
            min_stability: *min_stability,
            min_access_count: *min_access_count,
            limit: *limit,
        })),

        Command::MarkSuperseded { summary_id, ids } => {
            Request::Lifecycle(LifecycleRequest::MarkSuperseded(MarkSupersededArgs {
                user_id: user,
                ids: ids.clone(),
                summary_id: summary_id.clone(),
            }))
        }

        Command::Cold { id, cold_since } => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Request::Lifecycle(LifecycleRequest::MigrateToCold(MigrateToColdArgs {
                user_id: user,
                id: id.clone(),
                cold_since: cold_since.unwrap_or(now),
            }))
        }

        Command::Hot { id } => {
            Request::Lifecycle(LifecycleRequest::MigrateToHot(MigrateToHotArgs {
                user_id: user,
                id: id.clone(),
            }))
        }

        Command::Stub { id, content } => {
            Request::Lifecycle(LifecycleRequest::ConvertToStub(ConvertToStubArgs {
                user_id: user,
                id: id.clone(),
                stub_content: content.clone(),
            }))
        }

        Command::Retention { id, floor } => {
            Request::Lifecycle(LifecycleRequest::UpdateRetention(UpdateRetentionArgs {
                user_id: user,
                id: id.clone(),
                retention_floor: *floor,
            }))
        }

        Command::BatchRetention { pairs } => {
            let mut updates = Vec::with_capacity(pairs.len());
            for pair in pairs {
                let (id, floor) = pair
                    .split_once('=')
                    .ok_or_else(|| anyhow!("expected id=floor, got {pair:?}"))?;
                let floor: f64 = floor
                    .parse()
                    .with_context(|| format!("parse floor {floor:?}"))?;
                updates.push(RetentionUpdate {
                    id: id.to_string(),
                    retention_floor: floor,
                });
            }
            Request::Memory(MemoryRequest::BatchUpdate(BatchUpdateArgs {
                user_id: user,
                updates,
            }))
        }

        Command::Clear { confirm } => Request::Lifecycle(LifecycleRequest::Clear(ClearArgs {
            user_id: user,
            confirm: *confirm,
        })),

        Command::MintToken { scope, ttl_seconds } => {
            let scope = match scope.as_str() {
                "read" => BridgeScope::Read,
                "admin" => BridgeScope::Admin,
                _ => BridgeScope::Write,
            };
            Request::Diagnostics(DiagnosticsRequest::MintBridgeToken(MintBridgeTokenArgs {
                user_id: user,
                scope,
                ttl_seconds: *ttl_seconds,
            }))
        }

        // These three are detoured at the top of `main` and never
        // reach `run_command`. The arms exist only so the match is
        // exhaustive; reaching them is a programming error.
        Command::DownloadModel { .. } | Command::ConfigGetLlm | Command::ConfigSetLlm { .. } => {
            unreachable!("local-only commands are handled in main() before daemon dispatch");
        }
    };

    Ok(client.request(req).await?)
}

fn print_response(as_json: bool, resp: &Response) -> Result<()> {
    if !resp.ok {
        let err = resp
            .error
            .as_ref()
            .ok_or_else(|| anyhow!("response not ok but no error attached"))?;
        if as_json {
            #[allow(clippy::print_stdout)]
            {
                println!("{}", serde_json::to_string(&err)?);
            }
        } else {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("error: {} ({:?})", err.message, err.kind);
            }
        }
        std::process::exit(2);
    }

    if as_json {
        #[allow(clippy::print_stdout)]
        {
            println!("{}", serde_json::to_string(&resp.data)?);
        }
        return Ok(());
    }

    print_human(resp);
    Ok(())
}

fn print_human(resp: &Response) {
    use std::fmt::Write;
    let mut out = String::new();
    match resp.data.as_ref() {
        Some(ResponseData::Status(s)) => {
            let _ = writeln!(
                out,
                "daemon: {} (memories: {}, uptime: {})",
                s.daemon_version,
                s.memory_count,
                format_uptime(s.uptime_seconds)
            );
        }
        Some(ResponseData::Counts(c)) => {
            let _ = writeln!(out, "hot:   {}", c.hot);
            let _ = writeln!(out, "cold:  {}", c.cold);
            let _ = writeln!(out, "stub:  {}", c.stub);
            let _ = writeln!(out, "total: {}", c.total);
        }
        Some(ResponseData::MemoryStored(s)) => {
            let _ = writeln!(out, "stored: {}", s.id);
        }
        Some(ResponseData::MemoryStoredBatch(s)) => {
            let _ = writeln!(
                out,
                "stored {} memories with {} associations created:",
                s.ids.len(),
                s.associations_created
            );
            for id in &s.ids {
                let _ = writeln!(out, "  {id}");
            }
        }
        Some(ResponseData::MemorySearchResults(r)) => {
            if r.results.is_empty() {
                let _ = writeln!(out, "(no results)");
            } else {
                for hit in &r.results {
                    let _ = writeln!(out, "{:.3}\t{}\t{}", hit.score, hit.memory_id, hit.content);
                }
            }
            if !r.bridge_paths.is_empty() {
                let _ = writeln!(out, "bridges:");
                for path in &r.bridge_paths {
                    let _ = writeln!(out, "  {}", path.join(" -> "));
                }
            }
        }
        Some(ResponseData::Memory(m)) => {
            let _ = writeln!(out, "id:       {}", m.id);
            let _ = writeln!(out, "content:  {}", m.content);
            let _ = writeln!(out, "category: {}", m.category);
            let _ = writeln!(out, "type:     {}", m.memory_type);
            let _ = writeln!(out, "floor:    {:.3}", m.retention_floor);
            let _ = writeln!(out, "metadata: {}", m.metadata);
        }
        Some(ResponseData::Memories(ms)) => {
            if ms.memories.is_empty() {
                let _ = writeln!(out, "(no memories)");
            } else {
                for m in &ms.memories {
                    let _ = writeln!(out, "{}\t{}\t{}", m.id, m.category, m.content);
                }
            }
        }
        Some(ResponseData::Affected(a)) => {
            let _ = writeln!(out, "affected: {}", a.affected);
        }
        Some(ResponseData::LinkedMemories(ls)) => {
            if ls.memories.is_empty() {
                let _ = writeln!(out, "(no linked memories)");
            } else {
                for lm in &ls.memories {
                    let _ = writeln!(
                        out,
                        "{:.3}\t{}\t{}",
                        lm.link_strength, lm.memory.id, lm.memory.content
                    );
                }
            }
        }
        Some(ResponseData::LinkStrength(s)) => {
            let _ = writeln!(out, "strength: {:.3}", s.strength);
        }
        Some(ResponseData::LexicalIds(l)) => {
            for id in &l.ids {
                let _ = writeln!(out, "{id}");
            }
        }
        Some(ResponseData::Tick(t)) => {
            let _ = writeln!(
                out,
                "tick: completed={} memories_decayed={}",
                t.completed, t.memories_decayed
            );
        }
        Some(ResponseData::BridgeToken(t)) => {
            let _ = writeln!(out, "token (store now, not shown again):");
            let _ = writeln!(out, "  {}", t.token);
            let _ = writeln!(out, "  expires_at_unix: {}", t.expires_at_unix);
        }
        None => {
            let _ = writeln!(out, "(empty response)");
        }
    }
    #[allow(clippy::print_stdout)]
    {
        print!("{out}");
    }
}

fn default_socket_path() -> PathBuf {
    dirs::data_dir()
        .expect("data dir resolvable")
        .join("cognitive-memory")
        .join("cm.sock")
}

// ===========================================================================
// Local-only commands: model download + config edits
// ===========================================================================

/// Where downloaded models live. Honours XDG via `dirs::cache_dir`.
fn model_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .expect("cache dir resolvable")
        .join("cognitive-memory")
        .join("models")
}

/// Registry of named models the CLI knows how to download. Adding a
/// new entry is the only thing required to ship a new local-LLM
/// option through `cm download-model --model NAME`.
fn model_registry(name: &str) -> Result<(&'static str, &'static str)> {
    // (huggingface_url, on-disk filename)
    match name {
        "qwen3-4b" => Ok((
            "https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/main/Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
            "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        )),
        "gemma-3-4b" => Ok((
            "https://huggingface.co/unsloth/gemma-3-4b-it-GGUF/resolve/main/gemma-3-4b-it-Q4_K_M.gguf",
            "gemma-3-4b-it-Q4_K_M.gguf",
        )),
        other => Err(anyhow::anyhow!(
            "unknown model `{other}`; available: qwen3-4b, gemma-3-4b"
        )),
    }
}

#[allow(clippy::print_stdout)]
async fn run_download_model(name: &str) -> Result<()> {
    let (url, filename) = model_registry(name)?;
    let target_dir = model_cache_dir();
    std::fs::create_dir_all(&target_dir)
        .with_context(|| format!("create {}", target_dir.display()))?;
    let dest = target_dir.join(filename);
    if dest.exists() {
        println!("model already downloaded: {}", dest.display());
        return Ok(());
    }
    println!("downloading {name} ({url})");
    println!("  → {}", dest.display());

    let resp = reqwest::get(url)
        .await
        .with_context(|| "fetch model GGUF")?
        .error_for_status()
        .with_context(|| "model download HTTP error")?;
    let total = resp.content_length().unwrap_or(0);
    if total > 0 {
        println!("  size: {:.1} MB", total as f64 / 1_048_576.0);
    }
    let bytes = resp.bytes().await.with_context(|| "read model body")?;
    std::fs::write(&dest, &bytes).with_context(|| format!("write {}", dest.display()))?;
    println!("done: {} bytes written", bytes.len());

    println!("\nnext step:");
    println!("  cm config set-llm local --model-path {}", dest.display());
    Ok(())
}

#[allow(clippy::print_stdout)]
fn run_config_get_llm() -> Result<()> {
    let cfg = cognitive_memory_core::DaemonConfig::load().with_context(|| "load daemon config")?;
    match cfg.llm {
        None => {
            println!("no LLM configured (heuristic conflict resolution; consolidation skipped)")
        }
        Some(llm) => println!("{}", toml::to_string_pretty(&llm)?),
    }
    Ok(())
}

#[allow(clippy::print_stdout)]
fn run_config_set_llm(
    provider: &str,
    model_path: Option<&std::path::Path>,
    api_key_env: Option<&str>,
    model: Option<&str>,
) -> Result<()> {
    use cognitive_memory_core::LlmConfig;
    let new_llm = match provider {
        "none" => Some(LlmConfig::None),
        "local" => {
            let path = match model_path {
                Some(p) => p.to_path_buf(),
                None => {
                    let default = model_cache_dir().join("Qwen3-4B-Instruct-2507-Q4_K_M.gguf");
                    if !default.exists() {
                        return Err(anyhow::anyhow!(
                            "no --model-path specified and default {} not found; run `cm download-model` first",
                            default.display()
                        ));
                    }
                    default
                }
            };
            Some(LlmConfig::Local { model_path: path })
        }
        "openai" => Some(LlmConfig::Openai {
            api_key_env: api_key_env.unwrap_or("OPENAI_API_KEY").to_string(),
            model: model.unwrap_or("gpt-4o-mini").to_string(),
        }),
        "anthropic" => Some(LlmConfig::Anthropic {
            api_key_env: api_key_env.unwrap_or("ANTHROPIC_API_KEY").to_string(),
            model: model.unwrap_or("claude-haiku-4-5-20251001").to_string(),
        }),
        other => {
            return Err(anyhow::anyhow!(
                "unknown provider `{other}`; expected one of: local, openai, anthropic, none"
            ))
        }
    };
    // Load → mutate → save so we don't clobber adjacent sections like
    // [lifecycle] if the user hand-edited them. `load` returns Default
    // on missing-file (Phase 0a-daemon).
    let mut cfg = cognitive_memory_core::DaemonConfig::load()
        .with_context(|| "load existing daemon config")?;
    cfg.llm = new_llm;
    cfg.save().with_context(|| "save daemon config")?;
    println!(
        "saved config to {}",
        cognitive_memory_core::config_path().display()
    );
    println!("restart the daemon for changes to take effect (`pkill cm-daemon`)");
    Ok(())
}

/// Render a duration in seconds as `1d 2h 3m 4s`, dropping leading zero
/// units. `< 1s` becomes `0s`.
fn format_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 || !parts.is_empty() {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 || !parts.is_empty() {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{seconds}s"));
    parts.join(" ")
}

async fn connect_or_spawn(socket: &Path, user_id: &str, auto_spawn: bool) -> Result<Client> {
    match Client::connect(socket, "cm-cli", user_id).await {
        Ok(client) => return Ok(client),
        Err(e) if !auto_spawn => return Err(e.into()),
        Err(_) => {}
    }
    spawn_daemon(socket)?;
    wait_for_socket(socket, Duration::from_secs(2)).await?;
    Client::connect(socket, "cm-cli", user_id)
        .await
        .map_err(Into::into)
}

fn spawn_daemon(socket: &Path) -> Result<()> {
    use std::process::{Command, Stdio};

    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let bin = std::env::var("COGNITIVE_MEMORY_DAEMON_BIN")
        .map(PathBuf::from)
        .or_else(|_| {
            std::env::current_exe().map(|exe| {
                exe.parent()
                    .map(|d| d.join("cm-daemon"))
                    .unwrap_or_else(|| PathBuf::from("cm-daemon"))
            })
        })
        .unwrap_or_else(|_| PathBuf::from("cm-daemon"));

    Command::new(&bin)
        .env("COGNITIVE_MEMORY_SOCKET_PATH", socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn {}", bin.display()))?;

    Ok(())
}

async fn wait_for_socket(socket: &Path, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    let poll_interval = Duration::from_millis(20);
    loop {
        if socket.exists() && tokio::net::UnixStream::connect(socket).await.is_ok() {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(anyhow!(
                "daemon did not bind {} within {}s",
                socket.display(),
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(poll_interval).await;
    }
}
