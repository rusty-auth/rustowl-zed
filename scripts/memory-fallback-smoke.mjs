#!/usr/bin/env node

import { mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

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
const workspace = join(repository, "adapter", "tests", "fixtures", "basic");
const graphDirectory = mkdtempSync(join(tmpdir(), "rustowl-memory-fallback-"));
const environment = {
  ...process.env,
  RUSTOWL_GRAPH_DIR: graphDirectory,
  RUSTOWL_GRAPH_BACKEND: "memory",
  RUSTOWL_AUTO_SETUP: "0",
  RUSTOWLC: rustowlc,
  RUSTOWLC_WORKSPACE_WRAPPER: rustowlc,
};

function run(script, args) {
  const result = spawnSync(process.execPath, [join(repository, "scripts", script), ...args], {
    cwd: repository,
    env: environment,
    encoding: "utf8",
    maxBuffer: 10 * 1024 * 1024,
  });
  process.stdout.write(result.stdout ?? "");
  process.stderr.write(result.stderr ?? "");
  if (result.status !== 0) {
    throw new Error(`${script} failed with status ${result.status}`);
  }
}

function filesBelow(directory) {
  const files = [];
  const stack = [directory];
  while (stack.length > 0) {
    const current = stack.pop();
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const path = join(current, entry.name);
      if (entry.isDirectory()) stack.push(path);
      else files.push(path);
    }
  }
  return files;
}

try {
  run("smoke.mjs", [rustowl]);
  const snapshots = filesBelow(graphDirectory).filter(
    (path) => path.includes(`${join("graphs", "")}`) && path.endsWith(".json"),
  );
  if (snapshots.length !== 1) {
    throw new Error(
      `memory analysis published ${snapshots.length} portable snapshots; expected exactly one`,
    );
  }
  run("mcp-smoke.mjs", ["--server", mcp, "--workspace", workspace]);
  console.log(
    "RustOwl memory-fallback smoke passed (editor visuals and separate MCP reader share a portable immutable snapshot).",
  );
} finally {
  rmSync(graphDirectory, { recursive: true, force: true });
}
