#!/usr/bin/env node

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const fixture = join(repository, "engine", "tests", "fixtures", "ownership-cockpit");
const sourcePath = join(fixture, "src", "main.rs");
const rustowl = process.argv[2] ?? "rustowl";
const child = spawn(rustowl, [], {
  env: { ...process.env, RUSTOWL_AUTO_SETUP: process.env.RUSTOWL_AUTO_SETUP ?? "0" },
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
  const [{ finish, timer }] = waiters.splice(index, 1);
  clearTimeout(timer);
  finish(message);
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
  return new Promise((finish, reject) => {
    const waiter = {
      predicate,
      finish,
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
  if (response.error) throw new Error(`${method}: ${JSON.stringify(response.error)}`);
  return response.result;
}

const delay = (milliseconds) =>
  new Promise((finish) => setTimeout(finish, milliseconds));

async function main() {
  const rootUri = pathToFileURL(fixture).href;
  const sourceUri = pathToFileURL(sourcePath).href;
  await request("initialize", {
    processId: process.pid,
    rootUri,
    workspaceFolders: [{ uri: rootUri, name: "ownership-cockpit" }],
    capabilities: {},
  });
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

  const deadline = Date.now() + 120_000;
  let status;
  while (Date.now() < deadline) {
    status = await request("rustowl/analysisStatus", {});
    if (status.status === "finished" && status.activeRevision) break;
    if (status.status === "error") {
      throw new Error(`compiler graph analysis failed: ${status.lastError ?? "unknown error"}`);
    }
    await delay(200);
  }
  if (!status?.activeRevision) throw new Error("compiler graph analysis produced no revision");

  const evidence = await request("rustowl/inspectRange", {
    uri: sourceUri,
    range: {
      start: { line: 0, character: 0 },
      end: { line: 200, character: 0 },
    },
    documentVersion: 1,
    limits: { max_nodes: 500, max_edges: 1000, max_depth: 5 },
  });
  const graph = evidence?.result;
  if (!graph) throw new Error("compiler graph fixture was absent");
  const workspaceMap = await request("rustowl/workspaceMap", {
    includePlaces: false,
    limits: { max_nodes: 500, max_edges: 1000, max_depth: 12 },
  });
  const map = workspaceMap?.result;
  if (!map) throw new Error("compiler workspace call map was absent");
  const callSites = map.nodes.filter((node) => node.kind === "call_site");
  const callEdges = map.edges.filter((edge) => edge.kind === "calls");
  const labels = callSites.map((node) => node.label);
  for (const expected of ["stage_two", "generic_identity", "transform"]) {
    if (!labels.some((label) => label.includes(expected))) {
      throw new Error(`compiler graph omitted ${expected}; call labels: ${labels.join(", ")}`);
    }
  }
  const targetById = new Map(map.nodes.map((node) => [node.id, node]));
  const callTargets = new Map(
    callEdges.map((edge) => [edge.source, targetById.get(edge.target)?.label]),
  );
  const closureCall = callSites.find((node) => node.label.includes("{closure#0}"));
  if (!closureCall || !callTargets.get(closureCall.id)?.includes("{closure#0}")) {
    throw new Error(
      `rustc closure dispatch did not resolve to the indexed closure body (call: ${closureCall?.label ?? "missing"}; target: ${closureCall ? callTargets.get(closureCall.id) : "missing"})`,
    );
  }
  const traitCall = callSites.find((node) => node.label.includes("Transform>::transform"));
  if (
    !traitCall ||
    callTargets.get(traitCall.id) !== "<AddTraitStage as Transform>::transform"
  ) {
    throw new Error("static trait dispatch did not resolve to the concrete impl body");
  }
  const genericCall = callSites.find((node) => node.label === "generic_identity");
  if (!genericCall?.properties?.generic_arguments?.some((argument) => argument.includes("String"))) {
    throw new Error("generic call evidence omitted rustc's concrete type arguments");
  }
  if (callEdges.length < 6 || labels.includes("unresolved call")) {
    throw new Error(
      `compiler graph did not resolve the six-stage flow (${callEdges.length} call edges; ${labels.join(", ")})`,
    );
  }
  const stageTwoCall = callSites.find((node) => node.label.includes("stage_two"));
  const stageTwoTrace = await request("rustowl/ownershipGraph", {
    start: stageTwoCall.id,
    direction: "both",
    edgeKinds: ["calls", "declares", "owns", "moves_to", "passes_to", "returns_as"],
    limits: { max_nodes: 500, max_edges: 1000, max_depth: 5 },
  });
  const explanations = stageTwoTrace.result.edges
    .map((edge) => edge.explanation)
    .filter(Boolean);
  if (!explanations.some((text) => text.includes("binds to") && text.includes("stage_two"))) {
    throw new Error("compiler graph omitted caller-to-parameter ownership flow");
  }
  if (!explanations.some((text) => text.includes("stage_two") && text.includes("return place"))) {
    throw new Error(
      `compiler graph omitted callee-to-caller return flow; trace explanations: ${explanations.filter((text) => text.includes("return") || text.includes("stage_two")).join(" | ") || "none"}`,
    );
  }
  const closure = map.nodes.find(
    (node) => node.kind === "function" && node.label.includes("{closure"),
  );
  if (!closure) {
    throw new Error(
      `compiler graph omitted the closure body; functions: ${map.nodes.filter((node) => node.kind === "function").map((node) => node.label).join(", ")}`,
    );
  }
  const closureTrace = await request("rustowl/ownershipGraph", {
    start: closure.id,
    direction: "both",
    edgeKinds: ["declares", "owns", "packs_into", "passes_to", "moves_to"],
    limits: { max_nodes: 500, max_edges: 1000, max_depth: 6 },
  });
  if (
    !closureTrace.result.edges.some((edge) =>
      edge.explanation?.includes("captured closure environment"),
    )
  ) {
    throw new Error("compiler graph omitted closure capture-to-body flow");
  }
  const asyncFunctionNames = [
    "non_send_across_await",
    "borrowed_across_await",
    "send_static_across_await",
  ];
  const asyncSlices = [];
  for (const functionName of asyncFunctionNames) {
    const body = map.nodes.find(
      (node) =>
        node.kind === "function" &&
        node.label.includes(functionName) &&
        node.label.includes("{closure"),
    );
    if (!body) throw new Error(`compiler workspace map omitted async body ${functionName}`);
    const trace = await request("rustowl/ownershipGraph", {
      start: body.id,
      // Traversing incoming declaration/container edges walks from this async
      // body back into the entire workspace and can legitimately exhaust the
      // bounded response before reaching its retained-field blocker edges.
      // The exact causality under test is emitted outward from the body.
      direction: "outgoing",
      limits: { max_nodes: 500, max_edges: 1000, max_depth: 4 },
    });
    asyncSlices.push(trace.result);
  }
  const asyncConstraints = asyncSlices.flatMap((slice) =>
    slice.nodes.filter((node) => node.kind === "async_constraint"),
  );
  const futureFields = asyncSlices.flatMap((slice) =>
    slice.nodes.filter((node) => node.kind === "future_field"),
  );
  const asyncEdges = asyncSlices.flatMap((slice) => slice.edges);
  const sendBlockers = asyncEdges.filter((edge) => edge.kind === "blocks_send");
  const staticBlockers = asyncEdges.filter((edge) => edge.kind === "blocks_static");
  if (
    !asyncConstraints.some(
      (node) => node.properties?.type_name?.includes("Rc<") && node.properties?.send === "rejected",
    ) || sendBlockers.length === 0
  ) {
    throw new Error(
      `compiler graph omitted Rc-based !Send future causality; constraints: ${JSON.stringify(asyncConstraints.map((node) => node.properties))}; blocker edges: ${sendBlockers.length}`,
    );
  }
  if (
    !asyncConstraints.some(
      (node) => node.properties?.type_name?.startsWith("&") && node.properties?.static_lifetime === "rejected",
    ) || staticBlockers.length === 0
  ) {
    throw new Error(
      `compiler graph omitted borrowed non-'static future causality; constraints: ${JSON.stringify(asyncConstraints.map((node) => node.properties))}; blocker edges: ${staticBlockers.length}`,
    );
  }
  if (
    !asyncConstraints.some(
      (node) =>
        node.properties?.type_name?.includes("Arc<") &&
        node.properties?.send === "proven" &&
        node.properties?.static_lifetime === "proven",
    )
  ) {
    throw new Error("compiler graph omitted positive Send + 'static retained-state evidence");
  }
  if (futureFields.length === 0) {
    throw new Error("compiler graph omitted rustc coroutine-layout fields");
  }
  const futureFieldById = new Map(futureFields.map((node) => [node.id, node]));
  for (const edge of [...sendBlockers, ...staticBlockers]) {
    const field = futureFieldById.get(edge.source);
    if (!field) {
      throw new Error(`${edge.kind} is not sourced by an exact coroutine-layout field`);
    }
    const typeName = field.properties?.type_name ?? "";
    if (typeName.includes("Context<") || typeName.includes("Poll<")) {
      throw new Error(
        `compiler bookkeeping falsely blocks async compatibility: ${field.label}: ${typeName}`,
      );
    }
  }
  const positiveAsync = asyncSlices[2];
  if (
    positiveAsync.edges.some(
      (edge) => edge.kind === "blocks_send" || edge.kind === "blocks_static",
    )
  ) {
    throw new Error("Send + 'static Arc future received a false async blocker");
  }
  console.log(
    `RustOwl compiler graph smoke passed (${callSites.length} call sites, ${callEdges.length} resolved calls, parameter/return/capture and ${futureFields.length} exact coroutine-layout fields with async Send/'static causality present).`,
  );
}

try {
  await main();
} finally {
  child.stdin.end();
  child.kill();
}
