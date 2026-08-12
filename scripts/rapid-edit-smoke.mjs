#!/usr/bin/env node

import { spawn } from "node:child_process";
import { once } from "node:events";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const executable = (name) =>
  join(
    repository,
    "engine",
    "target",
    "debug",
    process.platform === "win32" ? `${name}.exe` : name,
  );
const rustowl = resolve(process.argv[2] ?? executable("rustowl"));
const rustowlc = resolve(process.argv[3] ?? executable("rustowlc"));
const mcp = resolve(process.argv[4] ?? executable("rustowl-mcp"));
const testDirectory = mkdtempSync(join(tmpdir(), "rustowl-rapid-edit-"));
const workspace = join(testDirectory, "workspace");
const graphDirectory = join(testDirectory, "graphs");
const sourcePath = join(workspace, "src", "main.rs");

const environment = {
  ...process.env,
  RUSTOWL_GRAPH_DIR: graphDirectory,
  RUSTOWL_GRAPH_BACKEND: "helix",
  RUSTOWL_AUTO_SETUP: "0",
  RUSTOWLC: rustowlc,
  RUSTOWLC_WORKSPACE_WRAPPER: rustowlc,
};

function source(revision) {
  return `fn consume(value: String) -> usize {
    value.len()
}

fn revision_${revision}_marker(value: &String) -> usize {
    value.len()
}

fn main() {
    let token = String::from("revision ${revision}");
    let borrowed = &token;
    println!("{borrowed}");
    let observed = revision_${revision}_marker(&token);
    let consumed = consume(token);
    println!("{observed} {consumed}");
}
`;
}

function lspClient(child) {
  let received = Buffer.alloc(0);
  let nextId = 1;
  const queued = [];
  const waiters = [];

  function send(message) {
    const body = Buffer.from(JSON.stringify(message));
    child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
    child.stdin.write(body);
  }

  function dispatch(message) {
    if (message.method && message.id !== undefined) {
      send({ jsonrpc: "2.0", id: message.id, result: null });
      return;
    }
    const index = waiters.findIndex(({ predicate }) => predicate(message));
    if (index === -1) {
      queued.push(message);
      return;
    }
    const [{ resolveMessage, timer }] = waiters.splice(index, 1);
    clearTimeout(timer);
    resolveMessage(message);
  }

  child.stdout.on("data", (chunk) => {
    received = Buffer.concat([received, chunk]);
    while (true) {
      const headerEnd = received.indexOf("\r\n\r\n");
      if (headerEnd === -1) return;
      const header = received.subarray(0, headerEnd).toString();
      const match = /content-length:\s*(\d+)/i.exec(header);
      if (!match) throw new Error("LSP response omitted Content-Length");
      const length = Number.parseInt(match[1], 10);
      const messageEnd = headerEnd + 4 + length;
      if (received.length < messageEnd) return;
      const body = received.subarray(headerEnd + 4, messageEnd);
      received = received.subarray(messageEnd);
      dispatch(JSON.parse(body.toString()));
    }
  });

  function waitFor(predicate, timeout = 120_000) {
    const index = queued.findIndex(predicate);
    if (index !== -1) return Promise.resolve(queued.splice(index, 1)[0]);
    return new Promise((resolveMessage, reject) => {
      const waiter = {
        predicate,
        resolveMessage,
        timer: setTimeout(() => {
          const index = waiters.indexOf(waiter);
          if (index !== -1) waiters.splice(index, 1);
          reject(new Error("timed out waiting for an LSP response"));
        }, timeout),
      };
      waiters.push(waiter);
    });
  }

  async function request(method, params) {
    const id = nextId++;
    send(
      params === undefined
        ? { jsonrpc: "2.0", id, method }
        : { jsonrpc: "2.0", id, method, params },
    );
    const response = await waitFor((message) => message.id === id);
    if (response.error) throw new Error(`${method}: ${JSON.stringify(response.error)}`);
    return response.result;
  }

  return { request, send };
}

async function waitForRevision(client, afterSequence, expectedVersion) {
  const deadline = Date.now() + 120_000;
  let latest;
  while (Date.now() < deadline) {
    latest = await client.request("rustowl/analysisStatus", {});
    if (latest.status === "error") {
      throw new Error(`rapid-edit analysis failed: ${latest.lastError ?? "unknown error"}`);
    }
    if (
      latest.status === "finished" &&
      latest.activeRevision?.sequence > afterSequence &&
      latest.staleFiles?.length === 0
    ) {
      return latest;
    }
    await new Promise((finish) => setTimeout(finish, 50));
  }
  throw new Error(
    `timed out waiting for document v${expectedVersion}; latest status ${JSON.stringify(latest)}`,
  );
}

function mcpClient(child) {
  const lines = createInterface({ input: child.stdout });
  const waiters = new Map();
  let nextId = 1;
  lines.on("line", (line) => {
    const message = JSON.parse(line);
    const waiter = waiters.get(message.id);
    if (waiter) {
      waiters.delete(message.id);
      clearTimeout(waiter.timer);
      waiter.resolveMessage(message);
    }
  });
  async function request(method, params = {}) {
    const id = nextId++;
    const response = new Promise((resolveMessage, reject) => {
      const timer = setTimeout(() => {
        waiters.delete(id);
        reject(new Error(`timed out waiting for MCP ${method}`));
      }, 15_000);
      waiters.set(id, { resolveMessage, timer });
    });
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    const message = await response;
    if (message.error) throw new Error(`${method}: ${JSON.stringify(message.error)}`);
    return message.result;
  }
  return { request, lines };
}

function toolResult(result) {
  if (result.isError || !result.structuredContent) {
    throw new Error(`MCP tool failed: ${JSON.stringify(result.structuredContent)}`);
  }
  return result.structuredContent;
}

async function main() {
  mkdirSync(dirname(sourcePath), { recursive: true });
  writeFileSync(
    join(workspace, "Cargo.toml"),
    '[package]\nname = "rustowl-rapid-edit"\nversion = "0.1.0"\nedition = "2024"\n',
  );
  writeFileSync(sourcePath, source("one"));

  const lsp = spawn(rustowl, [], {
    env: environment,
    stdio: ["pipe", "pipe", "inherit"],
  });
  const client = lspClient(lsp);
  const rootUri = pathToFileURL(workspace).href;
  const sourceUri = pathToFileURL(sourcePath).href;
  await client.request("initialize", {
    processId: process.pid,
    rootUri,
    workspaceFolders: [{ uri: rootUri, name: "rustowl-rapid-edit" }],
    capabilities: {},
  });
  client.send({ jsonrpc: "2.0", method: "initialized", params: {} });
  client.send({
    jsonrpc: "2.0",
    method: "textDocument/didOpen",
    params: {
      textDocument: {
        uri: sourceUri,
        languageId: "rust",
        version: 1,
        text: readFileSync(sourcePath, "utf8"),
      },
    },
  });
  // didOpen discovers the Cargo target and starts the first analysis. One
  // trigger is enough; a synthetic immediate save would test duplicate target
  // discovery rather than the rapid-edit race below.
  const first = await waitForRevision(client, 0, 1);

  const change = (version, text) => {
    writeFileSync(sourcePath, text);
    client.send({
      jsonrpc: "2.0",
      method: "textDocument/didChange",
      params: {
        textDocument: { uri: sourceUri, version },
        contentChanges: [{ text }],
      },
    });
    client.send({
      jsonrpc: "2.0",
      method: "textDocument/didSave",
      params: { textDocument: { uri: sourceUri } },
    });
  };

  change(2, source("two"));
  await new Promise((finish) => setTimeout(finish, 25));
  change(3, source("three"));
  const finalStatus = await waitForRevision(client, first.activeRevision.sequence, 3);
  const finalRevision = finalStatus.activeRevision;

  const inspection = await client.request("rustowl/inspectRange", {
    uri: sourceUri,
    range: {
      start: { line: 0, character: 0 },
      end: { line: 80, character: 0 },
    },
    documentVersion: 3,
    limits: { max_nodes: 500, max_edges: 1000, max_depth: 8 },
  });
  if (!inspection.result?.fresh || inspection.result.analyzed_document_version !== 3) {
    throw new Error(`final editor graph is stale: ${JSON.stringify(inspection)}`);
  }
  if (inspection.result.revision_id !== finalRevision.id) {
    throw new Error("editor range query did not use the active revision artifact");
  }

  const map = (
    await client.request("rustowl/workspaceMap", {
      includePlaces: false,
      limits: { max_nodes: 500, max_edges: 1000, max_depth: 12 },
    })
  ).result;
  const labels = map.nodes.map((node) => node.label);
  if (!labels.some((label) => label.includes("revision_three_marker"))) {
    throw new Error("final graph omitted revision_three_marker");
  }
  if (labels.some((label) => label.includes("revision_two_marker"))) {
    throw new Error("cancelled revision_two_marker became visible in the final graph");
  }

  // Persistence is intentionally off the editor's critical path. Give the
  // immutable publication task time to finish, then close it cleanly before a
  // separate process opens the exact artifact.
  await new Promise((finish) => setTimeout(finish, 500));
  await client.request("shutdown");
  client.send({ jsonrpc: "2.0", method: "exit", params: {} });
  lsp.stdin.end();
  if (lsp.exitCode === null) await once(lsp, "exit");

  const server = spawn(mcp, ["--workspace", workspace], {
    env: environment,
    stdio: ["pipe", "pipe", "inherit"],
  });
  const agent = mcpClient(server);
  await agent.request("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "rustowl-rapid-edit", version: "0.1.0" },
  });
  server.stdin.write(
    `${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" })}\n`,
  );
  const summary = toolResult(
    await agent.request("tools/call", {
      name: "rustowl_workspace_summary",
      arguments: {},
    }),
  );
  const loaded = summary.workspaces?.[0];
  if (loaded?.revisionId !== finalRevision.id) {
    throw new Error(
      `agent loaded ${loaded?.revisionId ?? "no revision"} sequence ${loaded?.revisionSequence ?? "?"} from ${loaded?.storageBackend ?? "?"}, expected ${finalRevision.id} sequence ${finalRevision.sequence}`,
    );
  }
  const latest = toolResult(
    await agent.request("tools/call", {
      name: "rustowl_search",
      arguments: { query: "revision_three_marker", limit: 20 },
    }),
  );
  const cancelled = toolResult(
    await agent.request("tools/call", {
      name: "rustowl_search",
      arguments: { query: "revision_two_marker", limit: 20 },
    }),
  );
  const latestMatches = latest.completeness?.totalMatches;
  const cancelledMatches = cancelled.completeness?.totalMatches;
  if (!latestMatches || cancelledMatches !== 0) {
    throw new Error(
      `agent artifact mismatch: latest=${latestMatches}, cancelled=${cancelledMatches}`,
    );
  }
  server.stdin.end();
  server.kill();
  agent.lines.close();

  console.log(
    `RustOwl rapid-edit smoke passed (document v3, revision ${finalRevision.sequence}; cancelled v2 absent from editor, Helix, and agent artifact).`,
  );
}

try {
  await main();
} finally {
  rmSync(testDirectory, { recursive: true, force: true });
}
