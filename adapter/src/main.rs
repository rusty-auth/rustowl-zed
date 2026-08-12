mod graph;
mod protocol;
mod semantic;

use std::{
    collections::{HashMap, HashSet},
    env,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, bail};
use graph::{GraphQueryResponse, GraphSlice, graph_decorations};
use protocol::{read_message, write_message};
use semantic::{
    CursorResponse, Decoration, Position, Range, TOKEN_TYPES, contains, decoration_markdown,
    decoration_presentation, document_range, identifier_positions, inlay_hints, inspect_range_at,
    ownership_flow_markdown, position_for_rustowl, range_for_lsp, semantic_tokens,
};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader, BufWriter},
    process::Command,
    sync::mpsc,
};

#[derive(Debug)]
enum Event {
    Client(Value),
    Server(Value),
    ClientClosed,
    ServerClosed,
    ReadError(&'static str, anyhow::Error),
}

#[derive(Debug)]
enum CursorPurpose {
    Hover(Value),
    Prefetch,
}

#[derive(Debug)]
struct PendingCursor {
    purpose: CursorPurpose,
    uri: String,
    position: Position,
    generation: u64,
}

#[derive(Debug)]
enum GraphPurpose {
    Hover {
        client_id: Value,
        position: Position,
    },
    Prefetch,
}

#[derive(Debug)]
struct PendingGraph {
    purpose: GraphPurpose,
    uri: String,
    range: Range,
    generation: u64,
}

const MAX_PREFETCH_POSITIONS: usize = 256;

#[derive(Default)]
struct State {
    next_internal_id: u64,
    initialize_ids: HashSet<String>,
    pending_analyzes: HashMap<String, (String, u64)>,
    internal_client_ids: HashSet<String>,
    pending_cursors: HashMap<String, PendingCursor>,
    pending_graphs: HashMap<String, PendingGraph>,
    prefetched_positions: HashMap<String, HashSet<Position>>,
    requested_graph_ranges: HashMap<String, HashSet<Range>>,
    requested_inlay_ranges: HashMap<String, Range>,
    open_document_ranges: HashMap<String, Range>,
    analyzed_documents: HashSet<String>,
    document_generations: HashMap<String, u64>,
    document_versions: HashMap<String, i64>,
    decorations: HashMap<String, Vec<Decoration>>,
    graph_slices: HashMap<String, Vec<GraphSlice>>,
    graph_api_available: bool,
}

impl State {
    fn internal_id(&mut self, purpose: &str) -> Value {
        self.next_internal_id += 1;
        Value::String(format!("rustowl-zed/{purpose}/{}", self.next_internal_id))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    register_zed_worktree()?;
    let rustowl =
        resolve_rustowl_binary(env::var("RUSTOWL_BINARY").unwrap_or_else(|_| "rustowl".into()));
    ensure_rustowl_toolchain(&rustowl).await?;
    let mut child = Command::new(&rustowl)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start RustOwl at {rustowl:?}"))?;

    let child_stdin = child
        .stdin
        .take()
        .context("RustOwl stdin was unavailable")?;
    let child_stdout = child
        .stdout
        .take()
        .context("RustOwl stdout was unavailable")?;
    let child_stderr = child
        .stderr
        .take()
        .context("RustOwl stderr was unavailable")?;

    let (events_tx, mut events_rx) = mpsc::channel(64);
    spawn_reader(BufReader::new(tokio::io::stdin()), events_tx.clone(), true);
    spawn_reader(BufReader::new(child_stdout), events_tx.clone(), false);
    tokio::spawn(async move {
        let mut lines = BufReader::new(child_stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[rustowl] {line}");
        }
    });

    let mut client_writer = BufWriter::new(tokio::io::stdout());
    let mut server_writer = BufWriter::new(child_stdin);
    let mut state = State::default();

    while let Some(event) = events_rx.recv().await {
        match event {
            Event::Client(message) => {
                handle_client_message(message, &mut state, &mut client_writer, &mut server_writer)
                    .await?;
            }
            Event::Server(message) => {
                handle_server_message(message, &mut state, &mut client_writer, &mut server_writer)
                    .await?;
            }
            Event::ClientClosed => break,
            Event::ServerClosed => bail!("RustOwl stopped before the editor connection closed"),
            Event::ReadError(endpoint, error) => {
                return Err(error).with_context(|| format!("failed reading from {endpoint}"));
            }
        }
    }

    child.kill().await.ok();
    Ok(())
}

fn register_zed_worktree() -> Result<()> {
    let session = env::var("RUSTOWL_ZED_SESSION_ID").ok();
    let worktree_id = env::var("RUSTOWL_ZED_WORKTREE_ID").ok();
    let (session, worktree_id) = match (session, worktree_id) {
        (Some(session), Some(worktree_id)) => (session, worktree_id),
        (None, None) => return Ok(()),
        _ => bail!("RustOwl Zed worktree registration is missing its session or worktree id"),
    };
    if !safe_registry_component(&session) || !safe_registry_component(&worktree_id) {
        bail!("RustOwl Zed worktree registration contains an invalid identifier");
    }
    let root = env::current_dir()
        .context("could not resolve the RustOwl Zed worktree")?
        .canonicalize()
        .context("could not canonicalize the RustOwl Zed worktree")?;
    let directory = env::temp_dir()
        .join("rustowl-zed")
        .join("worktrees")
        .join(session);
    std::fs::create_dir_all(&directory)
        .context("could not create the RustOwl Zed worktree registry")?;
    let destination = directory.join(&worktree_id);
    let temporary = directory.join(format!(".{worktree_id}.{}.tmp", std::process::id()));
    std::fs::write(&temporary, root.to_string_lossy().as_bytes())
        .context("could not stage the RustOwl Zed worktree registry")?;
    if let Err(error) = std::fs::rename(&temporary, &destination) {
        if cfg!(windows) && error.kind() == std::io::ErrorKind::AlreadyExists {
            std::fs::remove_file(&destination)
                .context("could not replace the RustOwl Zed worktree registry")?;
            std::fs::rename(&temporary, &destination)
                .context("could not publish the RustOwl Zed worktree registry")?;
        } else {
            let _ = std::fs::remove_file(&temporary);
            return Err(error).context("could not publish the RustOwl Zed worktree registry");
        }
    }
    Ok(())
}

fn safe_registry_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn resolve_rustowl_binary(binary: String) -> String {
    let Ok(adapter_executable) = env::current_exe() else {
        return binary;
    };
    let Some(candidate) = sibling_installation_path(&adapter_executable, Path::new(&binary)) else {
        return binary;
    };
    if candidate.is_file() {
        candidate.to_string_lossy().into_owned()
    } else {
        binary
    }
}

fn sibling_installation_path(adapter_executable: &Path, binary: &Path) -> Option<PathBuf> {
    if binary.is_absolute() || binary.components().count() < 2 {
        return None;
    }
    let extension_work_dir = adapter_executable.parent()?.parent()?;
    Some(extension_work_dir.join(binary))
}

async fn ensure_rustowl_toolchain(rustowl: &str) -> Result<()> {
    if env::var("RUSTOWL_AUTO_SETUP").as_deref() != Ok("1") {
        return Ok(());
    }

    let runtime_dir = Path::new(rustowl)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let ready_marker = runtime_dir.join(".rustowl-zed-toolchain-ready");
    if ready_marker.is_file() {
        return Ok(());
    }

    if toolchain_looks_complete(runtime_dir) {
        std::fs::write(&ready_marker, b"ready\n")
            .context("failed to record the RustOwl toolchain setup")?;
        return Ok(());
    }

    eprintln!("[rustowl-zed] installing RustOwl's required Rust toolchain (first run only)");
    let status = Command::new(rustowl)
        .args(["toolchain", "install", "--path"])
        .arg(runtime_dir)
        .arg("--skip-rustowl-toolchain")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .await
        .with_context(|| format!("failed to start RustOwl toolchain setup at {rustowl:?}"))?;

    if !status.success() || !toolchain_looks_complete(runtime_dir) {
        bail!(
            "RustOwl toolchain setup failed; run `{rustowl} toolchain install --path {} --skip-rustowl-toolchain`",
            runtime_dir.display()
        );
    }
    std::fs::write(&ready_marker, b"ready\n")
        .context("failed to record the RustOwl toolchain setup")?;

    Ok(())
}

fn toolchain_looks_complete(runtime_dir: &Path) -> bool {
    let cargo = if cfg!(windows) { "cargo.exe" } else { "cargo" };
    let rustc = if cfg!(windows) { "rustc.exe" } else { "rustc" };
    std::fs::read_dir(runtime_dir.join("sysroot"))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let root = entry.path();
            root.join("bin").join(cargo).is_file()
                && root.join("bin").join(rustc).is_file()
                && root.join("lib").join("rustlib").is_dir()
        })
}

fn spawn_reader<R>(mut reader: R, sender: mpsc::Sender<Event>, client: bool)
where
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match read_message(&mut reader).await {
                Ok(Some(message)) => {
                    let event = if client {
                        Event::Client(message)
                    } else {
                        Event::Server(message)
                    };
                    if sender.send(event).await.is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let event = if client {
                        Event::ClientClosed
                    } else {
                        Event::ServerClosed
                    };
                    sender.send(event).await.ok();
                    return;
                }
                Err(error) => {
                    let endpoint = if client { "Zed" } else { "RustOwl" };
                    sender.send(Event::ReadError(endpoint, error)).await.ok();
                    return;
                }
            }
        }
    });
}

async fn handle_client_message<C, S>(
    message: Value,
    state: &mut State,
    client: &mut C,
    server: &mut S,
) -> Result<()>
where
    C: tokio::io::AsyncWrite + Unpin,
    S: tokio::io::AsyncWrite + Unpin,
{
    if let Some(id) = message.get("id") {
        let key = id_key(id);
        if state.internal_client_ids.remove(&key) && message.get("method").is_none() {
            return Ok(());
        }
    }

    match message.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            if let Some(id) = message.get("id") {
                state.initialize_ids.insert(id_key(id));
            }
            write_message(server, &message).await
        }
        Some("textDocument/hover") => {
            let client_id = message
                .get("id")
                .cloned()
                .context("hover request had no id")?;
            let uri = message
                .pointer("/params/textDocument/uri")
                .and_then(Value::as_str)
                .context("hover request had no document URI")?
                .to_owned();
            let position: Position = serde_json::from_value(
                message
                    .pointer("/params/position")
                    .cloned()
                    .context("hover request had no position")?,
            )?;
            if state.graph_api_available {
                let graph_id = state.internal_id("inspect-hover");
                let range = inspect_range_at(&uri, position);
                let generation = document_generation(state, &uri);
                state.pending_graphs.insert(
                    id_key(&graph_id),
                    PendingGraph {
                        purpose: GraphPurpose::Hover {
                            client_id,
                            position,
                        },
                        uri: uri.clone(),
                        range,
                        generation,
                    },
                );
                return send_inspect_range(
                    graph_id,
                    &uri,
                    range,
                    state.document_versions.get(&uri).copied(),
                    server,
                )
                .await;
            }
            let cursor_id = state.internal_id("cursor");
            let generation = document_generation(state, &uri);
            state.pending_cursors.insert(
                id_key(&cursor_id),
                PendingCursor {
                    purpose: CursorPurpose::Hover(client_id),
                    uri: uri.clone(),
                    position,
                    generation,
                },
            );
            let rustowl_position = position_for_rustowl(&uri, position);
            write_message(
                server,
                &json!({
                    "jsonrpc": "2.0",
                    "id": cursor_id,
                    "method": "rustowl/cursor",
                    "params": {
                        "position": rustowl_position,
                        "document": { "uri": uri }
                    }
                }),
            )
            .await
        }
        Some("textDocument/semanticTokens/full") => {
            let id = message
                .get("id")
                .cloned()
                .context("semantic token request had no id")?;
            let uri = message
                .pointer("/params/textDocument/uri")
                .and_then(Value::as_str)
                .context("semantic token request had no document URI")?;
            let data = semantic_tokens(
                uri,
                state.decorations.get(uri).map(Vec::as_slice).unwrap_or(&[]),
            );
            write_message(
                client,
                &json!({"jsonrpc": "2.0", "id": id, "result": {"data": data}}),
            )
            .await?;
            if state.graph_api_available
                && let Some(range) = document_range(uri)
            {
                schedule_graph_prefetch(uri, range, state, server).await?;
            }
            Ok(())
        }
        Some("textDocument/inlayHint") => {
            let id = message
                .get("id")
                .cloned()
                .context("inlay hint request had no id")?;
            let uri = message
                .pointer("/params/textDocument/uri")
                .and_then(Value::as_str)
                .context("inlay hint request had no document URI")?
                .to_owned();
            let requested_range: Range = serde_json::from_value(
                message
                    .pointer("/params/range")
                    .cloned()
                    .context("inlay hint request had no range")?,
            )?;
            let hints = inlay_hints(
                &uri,
                requested_range,
                state
                    .decorations
                    .get(&uri)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            );
            state
                .requested_inlay_ranges
                .insert(uri.clone(), requested_range);
            write_message(
                client,
                &json!({"jsonrpc": "2.0", "id": id, "result": hints}),
            )
            .await?;
            if state.graph_api_available {
                schedule_graph_prefetch(&uri, requested_range, state, server).await?;
            } else if state.analyzed_documents.contains(&uri) {
                schedule_visible_prefetch(&uri, requested_range, state, server).await?;
            }
            Ok(())
        }
        Some("textDocument/didOpen") => {
            let uri = message
                .pointer("/params/textDocument/uri")
                .and_then(Value::as_str)
                .context("open notification had no document URI")?
                .to_owned();
            if let Some(version) = message
                .pointer("/params/textDocument/version")
                .and_then(Value::as_i64)
            {
                state.document_versions.insert(uri.clone(), version);
            }
            reset_document_visuals(state, &uri);
            if let Some(range) = document_range(&uri) {
                state.open_document_ranges.insert(uri.clone(), range);
            }
            write_message(server, &message).await?;
            if state.graph_api_available
                && let Some(range) = state.open_document_ranges.get(&uri).copied()
            {
                schedule_graph_prefetch(&uri, range, state, server).await?;
            }
            Ok(())
        }
        Some("textDocument/didSave") => {
            let uri = message
                .pointer("/params/textDocument/uri")
                .and_then(Value::as_str)
                .context("save notification had no document URI")?
                .to_owned();
            let generation = reset_document_visuals(state, &uri);
            request_visual_refresh(state, client).await?;
            write_message(server, &message).await?;
            if state.graph_api_available {
                return Ok(());
            }
            let id = state.internal_id("analyze");
            state
                .pending_analyzes
                .insert(id_key(&id), (uri, generation));
            write_message(
                server,
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "rustowl/analyze",
                    "params": {}
                }),
            )
            .await
        }
        Some("textDocument/didChange") => {
            if let Some(uri) = message
                .pointer("/params/textDocument/uri")
                .and_then(Value::as_str)
            {
                if let Some(version) = message
                    .pointer("/params/textDocument/version")
                    .and_then(Value::as_i64)
                {
                    state.document_versions.insert(uri.to_owned(), version);
                }
                reset_document_visuals(state, uri);
                request_visual_refresh(state, client).await?;
            }
            write_message(server, &message).await
        }
        Some("textDocument/didClose") => {
            if let Some(uri) = message
                .pointer("/params/textDocument/uri")
                .and_then(Value::as_str)
            {
                reset_document_visuals(state, uri);
                state.open_document_ranges.remove(uri);
                state.requested_inlay_ranges.remove(uri);
                state.document_versions.remove(uri);
            }
            write_message(server, &message).await
        }
        _ => write_message(server, &message).await,
    }
}

async fn handle_server_message<C, S>(
    mut message: Value,
    state: &mut State,
    client: &mut C,
    server: &mut S,
) -> Result<()>
where
    C: tokio::io::AsyncWrite + Unpin,
    S: tokio::io::AsyncWrite + Unpin,
{
    if message.get("method").and_then(Value::as_str) == Some("rustowl/analysisUpdated") {
        if state.graph_api_available {
            let visible: Vec<_> = state
                .open_document_ranges
                .iter()
                .map(|(uri, range)| (uri.clone(), *range))
                .collect();
            for (uri, _) in &visible {
                reset_document_visuals(state, uri);
                state.analyzed_documents.insert(uri.clone());
            }
            for (uri, range) in visible {
                schedule_graph_prefetch(&uri, range, state, server).await?;
            }
            request_visual_refresh(state, client).await?;
        }
        return Ok(());
    }
    let Some(id) = message.get("id").cloned() else {
        return write_message(client, &message).await;
    };
    let key = id_key(&id);

    if let Some((uri, generation)) = state.pending_analyzes.remove(&key) {
        if let Some(error) = message.get("error") {
            eprintln!("[rustowl-zed] RustOwl analysis request failed: {error}");
        } else if document_generation(state, &uri) == generation {
            state.analyzed_documents.insert(uri.clone());
            if let Some(requested_range) = state.requested_inlay_ranges.get(&uri).copied() {
                schedule_visible_prefetch(&uri, requested_range, state, server).await?;
            }
            request_visual_refresh(state, client).await?;
        }
        return Ok(());
    }
    if let Some(pending) = state.pending_cursors.remove(&key) {
        return handle_cursor_response(message, pending, state, client).await;
    }
    if let Some(pending) = state.pending_graphs.remove(&key) {
        return handle_graph_response(message, pending, state, client).await;
    }
    if state.initialize_ids.remove(&key) {
        state.graph_api_available = supports_indexed_graph(&message);
        augment_initialize_response(&mut message);
    }
    write_message(client, &message).await
}

async fn handle_cursor_response<C>(
    message: Value,
    pending: PendingCursor,
    state: &mut State,
    client: &mut C,
) -> Result<()>
where
    C: tokio::io::AsyncWrite + Unpin,
{
    if let Some(error) = message.get("error") {
        return match pending.purpose {
            CursorPurpose::Hover(client_id) => {
                write_message(
                    client,
                    &json!({"jsonrpc": "2.0", "id": client_id, "error": error}),
                )
                .await
            }
            CursorPurpose::Prefetch => {
                if has_pending_prefetch(state, &pending.uri, pending.generation) {
                    Ok(())
                } else {
                    request_visual_refresh(state, client).await
                }
            }
        };
    }

    let response: CursorResponse = serde_json::from_value(
        message
            .get("result")
            .cloned()
            .context("RustOwl cursor response had no result")?,
    )?;
    let lsp_decorations: Vec<_> = response
        .decorations
        .into_iter()
        .map(|mut decoration| {
            decoration.range = range_for_lsp(&pending.uri, decoration.range);
            decoration
        })
        .collect();

    if document_generation(state, &pending.uri) != pending.generation {
        return match pending.purpose {
            CursorPurpose::Hover(client_id) => {
                write_message(
                    client,
                    &json!({"jsonrpc": "2.0", "id": client_id, "result": null}),
                )
                .await
            }
            CursorPurpose::Prefetch => Ok(()),
        };
    }

    if response.is_analyzed {
        state.analyzed_documents.insert(pending.uri.clone());
    }
    let ownership_flow = ownership_flow_markdown(&pending.uri, &lsp_decorations);
    merge_decorations(
        state.decorations.entry(pending.uri.clone()).or_default(),
        lsp_decorations,
    );

    match pending.purpose {
        CursorPurpose::Hover(client_id) => {
            let hover = hover_result(
                pending.position,
                state
                    .decorations
                    .get(&pending.uri)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                response.status.as_ref(),
                response.is_analyzed,
                ownership_flow.as_deref(),
            );
            write_message(
                client,
                &json!({"jsonrpc": "2.0", "id": client_id, "result": hover}),
            )
            .await?;
            request_visual_refresh(state, client).await
        }
        CursorPurpose::Prefetch => {
            if has_pending_prefetch(state, &pending.uri, pending.generation) {
                Ok(())
            } else {
                request_visual_refresh(state, client).await
            }
        }
    }
}

async fn handle_graph_response<C>(
    message: Value,
    pending: PendingGraph,
    state: &mut State,
    client: &mut C,
) -> Result<()>
where
    C: tokio::io::AsyncWrite + Unpin,
{
    if let Some(error) = message.get("error") {
        state
            .requested_graph_ranges
            .entry(pending.uri.clone())
            .or_default()
            .remove(&pending.range);
        return match pending.purpose {
            GraphPurpose::Hover { client_id, .. } => {
                write_message(
                    client,
                    &json!({"jsonrpc": "2.0", "id": client_id, "error": error}),
                )
                .await
            }
            GraphPurpose::Prefetch => Ok(()),
        };
    }

    let response: GraphQueryResponse = serde_json::from_value(
        message
            .get("result")
            .cloned()
            .context("RustOwl graph response had no result")?,
    )?;
    if document_generation(state, &pending.uri) != pending.generation {
        return match pending.purpose {
            GraphPurpose::Hover { client_id, .. } => {
                write_message(
                    client,
                    &json!({"jsonrpc": "2.0", "id": client_id, "result": null}),
                )
                .await
            }
            GraphPurpose::Prefetch => Ok(()),
        };
    }

    let mut new_decorations = Vec::new();
    if let Some(slice) = response.result.as_ref() {
        if slice.schema_version != 1 {
            eprintln!(
                "[rustowl-zed] unsupported ownership graph schema {}",
                slice.schema_version
            );
        } else {
            match graph_decorations(&pending.uri, slice) {
                Ok(decorations) => new_decorations = decorations,
                Err(error) => eprintln!("[rustowl-zed] could not render graph evidence: {error}"),
            }
            let slices = state.graph_slices.entry(pending.uri.clone()).or_default();
            slices.push(slice.clone());
            if slices.len() > 8 {
                slices.remove(0);
            }
        }
    }
    if response.is_analyzed {
        state.analyzed_documents.insert(pending.uri.clone());
    }
    merge_decorations(
        state.decorations.entry(pending.uri.clone()).or_default(),
        new_decorations,
    );

    match pending.purpose {
        GraphPurpose::Hover {
            client_id,
            position,
        } => {
            let decorations = state
                .decorations
                .get(&pending.uri)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let ownership_flow = ownership_flow_markdown(&pending.uri, decorations);
            let hover = hover_result(
                position,
                decorations,
                response.status.as_ref(),
                response.is_analyzed,
                ownership_flow.as_deref(),
            );
            write_message(
                client,
                &json!({"jsonrpc": "2.0", "id": client_id, "result": hover}),
            )
            .await?;
            request_visual_refresh(state, client).await
        }
        GraphPurpose::Prefetch => {
            if has_pending_graph_prefetch(state, &pending.uri, pending.generation) {
                Ok(())
            } else {
                request_visual_refresh(state, client).await
            }
        }
    }
}

fn has_pending_graph_prefetch(state: &State, uri: &str, generation: u64) -> bool {
    state.pending_graphs.values().any(|request| {
        matches!(request.purpose, GraphPurpose::Prefetch)
            && request.uri == uri
            && request.generation == generation
    })
}

async fn schedule_graph_prefetch<S>(
    uri: &str,
    range: Range,
    state: &mut State,
    server: &mut S,
) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    if !state
        .requested_graph_ranges
        .entry(uri.to_owned())
        .or_default()
        .insert(range)
    {
        return Ok(());
    }
    let id = state.internal_id("inspect-range");
    state.pending_graphs.insert(
        id_key(&id),
        PendingGraph {
            purpose: GraphPurpose::Prefetch,
            uri: uri.to_owned(),
            range,
            generation: document_generation(state, uri),
        },
    );
    send_inspect_range(
        id,
        uri,
        range,
        state.document_versions.get(uri).copied(),
        server,
    )
    .await
}

async fn send_inspect_range<S>(
    id: Value,
    uri: &str,
    range: Range,
    document_version: Option<i64>,
    server: &mut S,
) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    write_message(
        server,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "rustowl/inspectRange",
            "params": {
                "uri": uri,
                "range": range,
                "documentVersion": document_version,
                "limits": {
                    "max_nodes": 500,
                    "max_edges": 1000,
                    "max_depth": 8
                }
            }
        }),
    )
    .await
}

fn supports_indexed_graph(message: &Value) -> bool {
    message
        .pointer("/result/capabilities/experimental/rustowl/methods")
        .and_then(Value::as_array)
        .is_some_and(|methods| {
            methods
                .iter()
                .any(|method| method.as_str() == Some("rustowl/inspectRange"))
        })
}

fn has_pending_prefetch(state: &State, uri: &str, generation: u64) -> bool {
    state.pending_cursors.values().any(|cursor| {
        matches!(cursor.purpose, CursorPurpose::Prefetch)
            && cursor.uri == uri
            && cursor.generation == generation
    })
}

fn merge_decorations(existing: &mut Vec<Decoration>, incoming: Vec<Decoration>) -> bool {
    let mut seen: HashSet<_> = existing.iter().cloned().collect();
    let mut changed = false;
    for decoration in incoming {
        if seen.insert(decoration.clone()) {
            existing.push(decoration);
            changed = true;
        }
    }
    changed
}

fn document_generation(state: &State, uri: &str) -> u64 {
    state.document_generations.get(uri).copied().unwrap_or(0)
}

fn reset_document_visuals(state: &mut State, uri: &str) -> u64 {
    let generation = state
        .document_generations
        .entry(uri.to_owned())
        .or_default();
    *generation += 1;
    state.decorations.remove(uri);
    state.prefetched_positions.remove(uri);
    state.requested_graph_ranges.remove(uri);
    state.graph_slices.remove(uri);
    state.analyzed_documents.remove(uri);
    *generation
}

async fn schedule_visible_prefetch<S>(
    uri: &str,
    requested_range: Range,
    state: &mut State,
    server: &mut S,
) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let generation = document_generation(state, uri);
    let positions = identifier_positions(uri, requested_range);
    for position in positions {
        let prefetched = state
            .prefetched_positions
            .entry(uri.to_owned())
            .or_default();
        if prefetched.len() >= MAX_PREFETCH_POSITIONS {
            break;
        }
        if !prefetched.insert(position) {
            continue;
        }

        let cursor_id = state.internal_id("prefetch");
        state.pending_cursors.insert(
            id_key(&cursor_id),
            PendingCursor {
                purpose: CursorPurpose::Prefetch,
                uri: uri.to_owned(),
                position,
                generation,
            },
        );
        let rustowl_position = position_for_rustowl(uri, position);
        write_message(
            server,
            &json!({
                "jsonrpc": "2.0",
                "id": cursor_id,
                "method": "rustowl/cursor",
                "params": {
                    "position": rustowl_position,
                    "document": { "uri": uri }
                }
            }),
        )
        .await?;
    }
    Ok(())
}

fn hover_result(
    position: Position,
    decorations: &[Decoration],
    status: Option<&Value>,
    is_analyzed: bool,
    ownership_flow: Option<&str>,
) -> Value {
    let mut matching: Vec<_> = decorations
        .iter()
        .filter(|decoration| contains(decoration.range, position))
        .collect();
    matching.sort_by_key(|decoration| {
        std::cmp::Reverse((
            decoration
                .hover_text
                .as_deref()
                .is_some_and(|text| text.starts_with("### RustOwl ·")),
            decoration_presentation(&decoration.kind)
                .map(|presentation| presentation.priority)
                .unwrap_or_default(),
        ))
    });

    if matching.is_empty() && is_analyzed && status.and_then(Value::as_str) == Some("finished") {
        return Value::Null;
    }

    let mut markdown = String::new();
    let mut primary_is_indexed_graph = false;
    if let Some(primary) = matching.first() {
        primary_is_indexed_graph = primary
            .hover_text
            .as_deref()
            .is_some_and(|text| text.starts_with("### RustOwl ·"));
        if let Some(primary_markdown) = decoration_markdown(primary) {
            markdown = primary_markdown;
        } else if let Some(report) = primary
            .hover_text
            .as_deref()
            .filter(|report| !report.is_empty())
        {
            markdown = format!("### RustOwl\n\n> **RustOwl report** · {report}");
        }

        let mut seen_kinds = HashSet::from([primary.kind.as_str()]);
        let mut also_here = Vec::new();
        for decoration in matching.iter().skip(1) {
            if !seen_kinds.insert(decoration.kind.as_str()) {
                continue;
            }
            let Some(presentation) = decoration_presentation(&decoration.kind) else {
                continue;
            };
            if !primary_is_indexed_graph && also_here.len() < 4 {
                also_here.push(presentation.title);
            }
        }
        if !also_here.is_empty() {
            markdown.push_str(&format!("\n\n**Also active** · {}", also_here.join(" · ")));
        }
    }

    if !primary_is_indexed_graph && let Some(ownership_flow) = ownership_flow {
        if !markdown.is_empty() {
            markdown.push_str("\n\n");
        }
        markdown.push_str(ownership_flow);
    }

    let analysis = analysis_markdown(status, is_analyzed);
    if !analysis.is_empty() {
        if !markdown.is_empty() {
            markdown.push_str("\n\n");
        }
        markdown.push_str(&analysis);
    }
    let mut result = Map::from_iter([(
        "contents".into(),
        json!({"kind": "markdown", "value": markdown}),
    )]);
    if let Some(primary) = matching.first() {
        result.insert("range".into(), serde_json::to_value(primary.range).unwrap());
    }
    Value::Object(result)
}

fn analysis_markdown(status: Option<&Value>, is_analyzed: bool) -> String {
    if !is_analyzed {
        return match status.and_then(Value::as_str) {
            Some("analyzing") => {
                "**Analysis** · Indexing this workspace — compiler ownership evidence will appear automatically.".into()
            }
            Some("error") => {
                "**Analysis** · Indexing failed — check the RustOwl language-server output.".into()
            }
            _ => "**Analysis** · Waiting for a saved result — save this file to run RustOwl.".into(),
        };
    }
    match status.and_then(Value::as_str) {
        Some("finished") => String::new(),
        Some("analyzing") => {
            "**Analysis** · Updating — ownership ranges may change shortly.".into()
        }
        Some("error") => "**Analysis** · Error — check the RustOwl language-server output.".into(),
        Some(status) => format!("**Analysis** · `{status}`"),
        None => "**Analysis** · Result available".into(),
    }
}

async fn request_visual_refresh<C>(state: &mut State, client: &mut C) -> Result<()>
where
    C: tokio::io::AsyncWrite + Unpin,
{
    let semantic_id = state.internal_id("semantic-refresh");
    state.internal_client_ids.insert(id_key(&semantic_id));
    write_message(
        client,
        &json!({
            "jsonrpc": "2.0",
            "id": semantic_id,
            "method": "workspace/semanticTokens/refresh",
            "params": null
        }),
    )
    .await?;

    let inlay_id = state.internal_id("inlay-refresh");
    state.internal_client_ids.insert(id_key(&inlay_id));
    write_message(
        client,
        &json!({
            "jsonrpc": "2.0",
            "id": inlay_id,
            "method": "workspace/inlayHint/refresh",
            "params": null
        }),
    )
    .await
}

fn augment_initialize_response(message: &mut Value) {
    let Some(capabilities) = message
        .pointer_mut("/result/capabilities")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    capabilities.insert("hoverProvider".into(), Value::Bool(true));
    capabilities.insert("inlayHintProvider".into(), Value::Bool(true));
    capabilities.insert("positionEncoding".into(), Value::String("utf-16".into()));
    capabilities.insert(
        "semanticTokensProvider".into(),
        json!({
            "legend": {
                "tokenTypes": TOKEN_TYPES,
                "tokenModifiers": []
            },
            "full": true,
            "range": false
        }),
    );
}

fn id_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| "null".into())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::{
        Decoration, Position, Range, TOKEN_TYPES, augment_initialize_response, hover_result,
        merge_decorations, sibling_installation_path,
    };

    #[test]
    fn advertises_adapter_capabilities() {
        let mut message = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"capabilities": {"textDocumentSync": 1}}
        });
        augment_initialize_response(&mut message);
        assert_eq!(message["result"]["capabilities"]["hoverProvider"], true);
        assert_eq!(message["result"]["capabilities"]["inlayHintProvider"], true);
        assert_eq!(
            message["result"]["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"],
            json!(TOKEN_TYPES)
        );
    }

    #[test]
    fn resolves_managed_rustowl_next_to_the_adapter_installation() {
        let adapter = Path::new("/zed-work/rustowl/adapter-v0.1.3/rustowl-zed-adapter");
        let rustowl = Path::new("rustowl-v0.4.0/rustowl");
        assert_eq!(
            sibling_installation_path(adapter, rustowl).unwrap(),
            Path::new("/zed-work/rustowl/rustowl-v0.4.0/rustowl")
        );
        assert!(sibling_installation_path(adapter, Path::new("rustowl")).is_none());
        assert!(sibling_installation_path(adapter, Path::new("/usr/local/bin/rustowl")).is_none());
    }

    #[test]
    fn merges_ownership_flows_from_multiple_prefetched_values() {
        let borrow = Decoration {
            kind: "imm_borrow".into(),
            range: Range {
                start: Position {
                    line: 6,
                    character: 19,
                },
                end: Position {
                    line: 6,
                    character: 27,
                },
            },
            hover_text: Some("immutable borrow".into()),
            overlapped: false,
        };
        let moved = Decoration {
            kind: "move".into(),
            range: Range {
                start: Position {
                    line: 11,
                    character: 31,
                },
                end: Position {
                    line: 11,
                    character: 36,
                },
            },
            hover_text: Some("variable moved".into()),
            overlapped: false,
        };
        let mut combined = vec![borrow.clone()];

        assert!(merge_decorations(
            &mut combined,
            vec![borrow, moved.clone()]
        ));
        assert_eq!(combined.len(), 2);
        assert!(combined.contains(&moved));
        assert!(!merge_decorations(&mut combined, vec![moved]));
    }

    #[test]
    fn builds_an_educational_native_markdown_hover() {
        let range = Range {
            start: Position {
                line: 2,
                character: 19,
            },
            end: Position {
                line: 2,
                character: 27,
            },
        };
        let decorations = vec![
            Decoration {
                kind: "lifetime".into(),
                range,
                hover_text: Some("lifetime of variable `message`".into()),
                overlapped: false,
            },
            Decoration {
                kind: "imm_borrow".into(),
                range,
                hover_text: Some("immutable borrow".into()),
                overlapped: false,
            },
        ];

        let hover = hover_result(
            Position {
                line: 2,
                character: 21,
            },
            &decorations,
            Some(&json!("finished")),
            true,
            Some("**Flow** · `L3 shared borrow` → `L4 last use`"),
        );
        let markdown = hover["contents"]["value"].as_str().unwrap();
        assert!(markdown.starts_with("### RustOwl · Shared borrow"));
        assert!(markdown.contains("**Ownership** · the source keeps the value"));
        assert!(markdown.contains("> **RustOwl report** · immutable borrow"));
        assert!(markdown.contains("**Also active**"));
        assert!(markdown.contains("Lifetime region"));
        assert!(markdown.contains("**Flow** · `L3 shared borrow` → `L4 last use`"));
        assert!(!markdown.contains("**Analysis**"));
        assert_eq!(hover["range"], json!(range));
    }

    #[test]
    fn prefers_compiler_graph_evidence_over_the_legacy_cursor_report() {
        let range = Range {
            start: Position {
                line: 2,
                character: 19,
            },
            end: Position {
                line: 2,
                character: 27,
            },
        };
        let graph_markdown = "### RustOwl · Shared borrow\n\nCompiler-backed summary.\n\n**What this means** · `message` remains owned.\n\n**Compiler evidence**\n\n- **MIR flow** · `borrows shared`";
        let decorations = vec![
            Decoration {
                kind: "imm_borrow".into(),
                range,
                hover_text: Some("immutable borrow".into()),
                overlapped: false,
            },
            Decoration {
                kind: "imm_borrow".into(),
                range,
                hover_text: Some(graph_markdown.into()),
                overlapped: false,
            },
        ];

        let hover = hover_result(
            Position {
                line: 2,
                character: 21,
            },
            &decorations,
            Some(&json!("finished")),
            true,
            Some("**Flow** · legacy heuristic"),
        );
        let markdown = hover["contents"]["value"].as_str().unwrap();
        assert!(markdown.starts_with(graph_markdown));
        assert!(!markdown.contains("legacy heuristic"));
        assert!(!markdown.contains("RustOwl report"));
    }

    #[test]
    fn suppresses_status_only_hover_after_analysis_finishes() {
        let hover = hover_result(
            Position {
                line: 0,
                character: 0,
            },
            &[],
            Some(&json!("finished")),
            true,
            None,
        );
        assert!(hover.is_null());
    }

    #[test]
    fn explains_when_a_file_has_not_been_analyzed() {
        let hover = hover_result(
            Position {
                line: 0,
                character: 0,
            },
            &[],
            Some(&json!("finished")),
            false,
            None,
        );
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("save this file to run RustOwl")
        );
    }

    #[test]
    fn explains_initial_workspace_indexing_without_asking_for_a_save() {
        let hover = hover_result(
            Position {
                line: 0,
                character: 0,
            },
            &[],
            Some(&json!("analyzing")),
            false,
            None,
        );
        let markdown = hover["contents"]["value"].as_str().unwrap();
        assert!(markdown.contains("Indexing this workspace"));
        assert!(!markdown.contains("save this file"));
    }
}
