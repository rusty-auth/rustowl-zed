#!/usr/bin/env node

import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const fixture = join(repository, "adapter", "tests", "fixtures", "basic");
const argumentsByName = new Map();
for (let index = 2; index < process.argv.length; index += 2) {
  const name = process.argv[index];
  const value = process.argv[index + 1];
  if (!name?.startsWith("--") || !value) {
    throw new Error(`expected --name value, received ${name ?? "<nothing>"}`);
  }
  argumentsByName.set(name.slice(2), value);
}
const server = resolve(
  argumentsByName.get("server") ??
    process.env.RUSTOWL_MCP_BINARY ??
    join(repository, "engine", "target", "debug", "rustowl-mcp"),
);
const workspace = resolve(argumentsByName.get("workspace") ?? fixture);
const inspectionFile = argumentsByName.get("file") ?? "src/lib.rs";
const serverArguments = argumentsByName.has("zed-session")
  ? [
      "--zed-session",
      argumentsByName.get("zed-session"),
      "--worktree-id",
      argumentsByName.get("worktree-id") ?? "1",
    ]
  : ["--workspace", workspace];
const child = spawn(server, serverArguments, {
  stdio: ["pipe", "pipe", "inherit"],
});
const lines = createInterface({ input: child.stdout });
const queued = [];
const waiters = new Map();
let nextId = 1;

lines.on("line", (line) => {
  const message = JSON.parse(line);
  const waiter = waiters.get(message.id);
  if (waiter) {
    waiters.delete(message.id);
    clearTimeout(waiter.timer);
    waiter.resolve(message);
  } else {
    queued.push(message);
  }
});

function send(message) {
  child.stdin.write(`${JSON.stringify(message)}\n`);
}

async function request(method, params = {}) {
  const id = nextId++;
  const response = new Promise((resolveResponse, reject) => {
    const timer = setTimeout(() => {
      waiters.delete(id);
      reject(new Error(`timed out waiting for ${method}`));
    }, 15_000);
    waiters.set(id, { resolve: resolveResponse, timer });
  });
  send({ jsonrpc: "2.0", id, method, params });
  const message = await response;
  if (message.error) throw new Error(`${method}: ${JSON.stringify(message.error)}`);
  return message.result;
}

function toolResult(result) {
  if (result.isError) {
    throw new Error(`MCP tool failed: ${JSON.stringify(result.structuredContent)}`);
  }
  if (!result.structuredContent) {
    throw new Error("MCP tool omitted structuredContent");
  }
  return result.structuredContent;
}

async function main() {
  const initialized = await request("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "rustowl-smoke", version: "0.1.0" },
  });
  if (!initialized.capabilities.tools || !initialized.capabilities.prompts) {
    throw new Error("MCP server did not advertise tools and prompts");
  }
  send({ jsonrpc: "2.0", method: "notifications/initialized" });

  const tools = await request("tools/list");
  const toolNames = new Set(tools.tools.map((tool) => tool.name));
  for (const expected of [
    "rustowl_workspace_summary",
    "rustowl_inspect_range",
    "rustowl_trace_ownership",
    "rustowl_render_mermaid",
    "rustowl_search",
    "rustowl_async_state",
  ]) {
    if (!toolNames.has(expected)) throw new Error(`missing MCP tool ${expected}`);
  }

  const prompts = await request("prompts/list");
  const promptNames = new Set(prompts.prompts.map((prompt) => prompt.name));
  if (
    !promptNames.has("debug-rust-ownership") ||
    !promptNames.has("explain-rust-async-state") ||
    !promptNames.has("plan-rust-ownership-refactor")
  ) {
    throw new Error("MCP ownership prompts are missing");
  }

  const summary = toolResult(
    await request("tools/call", {
      name: "rustowl_workspace_summary",
      arguments: {},
    }),
  );
  if (summary.workspaces?.[0]?.revisionSequence === undefined) {
    throw new Error("workspace summary omitted its revision");
  }

  const inspection = toolResult(
    await request("tools/call", {
      name: "rustowl_inspect_range",
      arguments: {
        file: inspectionFile,
        start_line: Number(argumentsByName.get("start-line") ?? 1),
        end_line: Number(argumentsByName.get("end-line") ?? 30),
        max_nodes: 500,
      },
    }),
  );
  const nodes = inspection.evidence?.nodes ?? [];
  const edges = inspection.evidence?.edges ?? [];
  const borrowEdge = edges.find((edge) => edge.kind === "borrows_shared");
  const borrowNode = nodes.find((node) => node.kind === "borrow_event");
  if (nodes.length < 2 || (!borrowEdge && !borrowNode)) {
    const kinds = {
      nodes: [...new Set(nodes.map((node) => node.kind))],
      edges: [...new Set(edges.map((edge) => edge.kind))],
    };
    throw new Error(
      `range inspection omitted compiler borrow evidence: ${JSON.stringify(kinds)}`,
    );
  }

  const traceStart = borrowEdge?.source ?? borrowNode?.id ?? nodes[0].id;
  const trace = toolResult(
    await request("tools/call", {
      name: "rustowl_trace_ownership",
      arguments: { node_id: traceStart, direction: "both", max_depth: 4 },
    }),
  );
  if (!trace.evidence?.nodes?.length) {
    throw new Error("ownership trace returned no evidence");
  }

  const diagram = toolResult(
    await request("tools/call", {
      name: "rustowl_render_mermaid",
      arguments: { node_id: traceStart, direction: "both", max_depth: 3 },
    }),
  );
  if (!diagram.mermaid?.startsWith("flowchart LR")) {
    throw new Error("Mermaid ownership diagram was not rendered");
  }

  console.log(
    `RustOwl MCP smoke test passed (${tools.tools.length} tools, ${prompts.prompts.length} prompts, revision ${summary.workspaces[0].revisionSequence}).`,
  );
}

try {
  await main();
} finally {
  child.stdin.end();
  child.kill();
  lines.close();
}
