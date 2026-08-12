#!/usr/bin/env node

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const fixture = join(repository, "adapter", "tests", "fixtures", "basic");
const sourcePath = join(fixture, "src", "lib.rs");
const adapter =
  process.env.RUSTOWL_ZED_ADAPTER ??
  join(
    repository,
    "adapter",
    "target",
    "debug",
    process.platform === "win32"
      ? "rustowl-zed-adapter.exe"
      : "rustowl-zed-adapter",
  );
const rustowl = process.argv[2] ?? "rustowl";

const child = spawn(adapter, [], {
  env: {
    ...process.env,
    RUSTOWL_BINARY: rustowl,
    RUSTOWL_AUTO_SETUP: process.env.RUSTOWL_AUTO_SETUP ?? "1",
  },
  stdio: ["pipe", "pipe", "inherit"],
});

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
  const [{ resolve: finish, timer }] = waiters.splice(index, 1);
  clearTimeout(timer);
  finish(message);
}

function parseMessages() {
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
}

child.stdout.on("data", (chunk) => {
  received = Buffer.concat([received, chunk]);
  parseMessages();
});

function waitFor(predicate, timeout = 15_000) {
  const index = queued.findIndex(predicate);
  if (index !== -1) return Promise.resolve(queued.splice(index, 1)[0]);
  return new Promise((finish, reject) => {
    const waiter = {
      predicate,
      resolve: finish,
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
  send({ jsonrpc: "2.0", id, method, params });
  const response = await waitFor((message) => message.id === id);
  if (response.error) throw new Error(JSON.stringify(response.error));
  return response.result;
}

function delay(milliseconds) {
  return new Promise((finish) => setTimeout(finish, milliseconds));
}

async function main() {
  const rootUri = pathToFileURL(fixture).href;
  const sourceUri = pathToFileURL(sourcePath).href;
  const initialize = await request("initialize", {
    processId: process.pid,
    rootUri,
    workspaceFolders: [{ uri: rootUri, name: "basic" }],
    capabilities: {
      workspace: {
        semanticTokens: { refreshSupport: true },
        inlayHint: { refreshSupport: true },
      },
    },
  });
  if (!initialize.capabilities.semanticTokensProvider) {
    throw new Error("adapter did not advertise semantic tokens");
  }
  if (!initialize.capabilities.inlayHintProvider) {
    throw new Error("adapter did not advertise inlay hints");
  }
  if (
    !initialize.capabilities.experimental?.rustowl?.methods?.includes(
      "rustowl/inspectRange",
    )
  ) {
    throw new Error("maintained RustOwl engine did not advertise indexed graph APIs");
  }

  send({ jsonrpc: "2.0", method: "initialized", params: {} });
  send({
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
  await delay(500);
  send({
    jsonrpc: "2.0",
    method: "textDocument/didSave",
    params: { textDocument: { uri: sourceUri } },
  });

  const deadline =
    Date.now() + Number(process.env.RUSTOWL_SMOKE_TIMEOUT_MS ?? "60000");
  let lastTokenCount = 0;
  let lastLabels = [];
  while (Date.now() < deadline) {
    const tokens = await request("textDocument/semanticTokens/full", {
      textDocument: { uri: sourceUri },
    });
    const hints = await request("textDocument/inlayHint", {
      textDocument: { uri: sourceUri },
      range: {
        start: { line: 0, character: 0 },
        end: { line: 100, character: 0 },
      },
    });
    const richHints = hints.filter(
      (hint) =>
        typeof hint.label === "string" &&
        hint.label.includes(" · ") &&
        hint.tooltip?.kind === "markdown" &&
        hint.tooltip.value.includes("### RustOwl ·"),
    );
    const labels = new Set(richHints.map((hint) => hint.label));
    lastTokenCount = tokens.data.length / 5;
    lastLabels = [...labels];
    const hasMultipleOwnershipEvents =
      labels.size >= 2 &&
      labels.has("← shared borrow · read-only") &&
      [...labels].some((label) => label !== "← shared borrow · read-only");
    if (tokens.data.length > 0 && hasMultipleOwnershipEvents) {
      const hover = await request("textDocument/hover", {
        textDocument: { uri: sourceUri },
        position: { line: 2, character: 22 },
      });
      if (
        hover?.contents?.kind === "markdown" &&
        hover.contents.value.includes("### RustOwl ·")
      ) {
        const hoverMarkdown = hover.contents.value;
        if (
          !hoverMarkdown.includes("read-only view") ||
          !hoverMarkdown.includes("**Why it matters**") ||
          !hoverMarkdown.includes("Verified by rustc against the current source")
        ) {
          throw new Error(
            `RustOwl hover omitted the human ownership explanation:\n${hoverMarkdown}`,
          );
        }
        if (
          /compiler temporary|\*?\(_\d+|\b_\d+\b|\bMIR\b|revision \d|schema v\d|source [0-9a-f]{6,}/i.test(
            hoverMarkdown,
          )
        ) {
          throw new Error(
            `RustOwl hover leaked compiler or storage internals: ${hoverMarkdown}`,
          );
        }
        const evidence = await request("rustowl/inspectRange", {
          uri: sourceUri,
          range: {
            start: { line: 0, character: 0 },
            end: { line: 100, character: 0 },
          },
          documentVersion: 1,
          limits: { max_nodes: 500, max_edges: 1000, max_depth: 8 },
        });
        const graph = evidence?.result;
        const callSites =
          graph?.nodes?.filter((node) => node.kind === "call_site") ?? [];
        const callEdges =
          graph?.edges?.filter((edge) => edge.kind === "calls") ?? [];
        if (
          callSites.length === 0 ||
          callEdges.length === 0 ||
          callSites.some((node) => node.label === "unresolved call")
        ) {
          throw new Error(
            `RustOwl compiler graph omitted resolved call evidence (${callSites.length} call sites, ${callEdges.length} call edges; labels: ${callSites.map((node) => node.label).join(", ") || "none"})`,
          );
        }
        console.log(
          `RustOwl indexed-graph smoke test passed without hover activation (${tokens.data.length / 5} underlines, ${hints.length} rich inline hints, ${callEdges.length} resolved call edges).`,
        );
        return;
      }
    }
    await delay(250);
  }
  const status = await request("rustowl/analysisStatus", {});
  const evidence = await request("rustowl/inspectRange", {
    uri: sourceUri,
    range: {
      start: { line: 0, character: 0 },
      end: { line: 100, character: 0 },
    },
    documentVersion: 1,
    limits: { max_nodes: 500, max_edges: 1000, max_depth: 8 },
  });
  const kinds = [
    ...new Set(evidence?.result?.nodes?.map((node) => node.kind) ?? []),
  ];
  throw new Error(
    `RustOwl automatic visuals timed out (${lastTokenCount} underlines, labels: ${lastLabels.join(", ") || "none"}; status: ${status.status}, revision: ${status.activeRevision?.sequence ?? "none"}, stale files: ${status.staleFiles?.length ?? 0}, evidence: ${kinds.join(", ") || "none"})`,
  );
}

try {
  await main();
} finally {
  child.stdin.end();
  child.kill();
}
