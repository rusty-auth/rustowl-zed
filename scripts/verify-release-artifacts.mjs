#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const expectedTargets = new Set([
  "aarch64-apple-darwin",
  "aarch64-pc-windows-msvc",
  "aarch64-unknown-linux-gnu",
  "x86_64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "x86_64-unknown-linux-gnu",
]);

function walk(path) {
  const stat = statSync(path);
  if (stat.isFile()) return [path];
  return readdirSync(path, { withFileTypes: true }).flatMap((entry) =>
    walk(join(path, entry.name)),
  );
}

function run(command, arguments_, cwd) {
  const result = spawnSync(command, arguments_, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.status !== 0) {
    throw new Error(
      `${command} ${arguments_.join(" ")} failed:\n${result.stderr || result.stdout}`,
    );
  }
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function expectedFiles(windows) {
  const suffix = windows ? ".exe" : "";
  return [
    "ENGINE_THIRD_PARTY_NOTICES.md",
    "LICENSE-APACHE-2.0",
    "LICENSE-MIT",
    "LICENSE-MPL-2.0",
    "THIRD_PARTY_NOTICES.md",
    "checksums.sha256",
    "manifest.json",
    `rustowl${suffix}`,
    `rustowl-mcp${suffix}`,
    `rustowl-zed-adapter${suffix}`,
    `rustowlc${suffix}`,
    "sbom.cdx.json",
  ].sort();
}

function verifyBinaryFormat(path, target) {
  const magic = readFileSync(path).subarray(0, 4).toString("hex");
  const expected = target.includes("windows")
    ? "4d5a"
    : target.includes("apple")
      ? "cffaedfe"
      : "7f454c46";
  if (!magic.startsWith(expected)) {
    throw new Error(`${basename(path)} has ${magic}, expected ${expected} for ${target}`);
  }
}

function verifyArchive(archive, expectedVersion) {
  const name = basename(archive);
  const match = /^rustowl-zed-runtime-(.+)\.(tar\.gz|zip)$/.exec(name);
  if (!match) throw new Error(`unexpected release archive ${name}`);
  const [, target, format] = match;
  if (!expectedTargets.has(target)) throw new Error(`unsupported target ${target}`);

  const extraction = mkdtempSync(join(tmpdir(), "rustowl-release-verify-"));
  try {
    if (format === "zip") {
      run("unzip", ["-q", archive, "-d", extraction], repository);
    } else {
      run("tar", ["-xzf", archive, "-C", extraction], repository);
    }

    const actualFiles = readdirSync(extraction).sort();
    const requiredFiles = expectedFiles(format === "zip");
    if (JSON.stringify(actualFiles) !== JSON.stringify(requiredFiles)) {
      throw new Error(
        `${target} archive contents differ:\nactual ${actualFiles.join(", ")}\nexpected ${requiredFiles.join(", ")}`,
      );
    }

    const checksumText = readFileSync(join(extraction, "checksums.sha256"), "utf8");
    if (checksumText.startsWith("\uFEFF") || checksumText.includes("\r")) {
      throw new Error(`${target} checksum manifest is not LF-only UTF-8 without BOM`);
    }
    if (!checksumText.endsWith("\n")) {
      throw new Error(`${target} checksum manifest lacks a final newline`);
    }
    const checksumLines = checksumText.trimEnd().split("\n");
    if (checksumLines.length !== requiredFiles.length - 1) {
      throw new Error(`${target} checksum manifest has ${checksumLines.length} entries`);
    }
    for (const line of checksumLines) {
      const checksum = /^([a-f0-9]{64})  ([^/\\]+)$/.exec(line);
      if (!checksum) throw new Error(`${target} has malformed checksum line: ${line}`);
      const [, expectedHash, filename] = checksum;
      const actualHash = sha256(join(extraction, filename));
      if (actualHash !== expectedHash) {
        throw new Error(`${target} checksum mismatch for ${filename}`);
      }
    }

    const manifest = JSON.parse(readFileSync(join(extraction, "manifest.json"), "utf8"));
    if (
      manifest.formatVersion !== 1 ||
      manifest.extensionVersion !== expectedVersion ||
      manifest.ownershipGraphSchemaVersion !== 1 ||
      manifest.persistence?.backend !== "embedded-helixdb" ||
      manifest.evidencePolicy?.agentAccess !== "read-only" ||
      JSON.stringify([...manifest.binaries].sort()) !==
        JSON.stringify(["rustowl", "rustowl-mcp", "rustowl-zed-adapter", "rustowlc"])
    ) {
      throw new Error(`${target} compatibility manifest is inconsistent`);
    }

    const sbom = JSON.parse(readFileSync(join(extraction, "sbom.cdx.json"), "utf8"));
    const componentNames = new Set(sbom.components?.map((component) => component.name));
    for (const component of ["helix-db", "rustowl", "rustowl-zed-adapter"]) {
      if (!componentNames.has(component)) {
        throw new Error(`${target} SBOM omits ${component}`);
      }
    }

    const suffix = format === "zip" ? ".exe" : "";
    for (const binary of ["rustowl", "rustowl-mcp", "rustowl-zed-adapter", "rustowlc"]) {
      verifyBinaryFormat(join(extraction, `${binary}${suffix}`), target);
    }
    console.log(`${target}: archive, checksums, manifest, SBOM, and binaries verified`);
    return target;
  } finally {
    rmSync(extraction, { recursive: true, force: true });
  }
}

const extensionToml = readFileSync(join(repository, "extension.toml"), "utf8");
const version = /^version\s*=\s*"([^"]+)"/m.exec(extensionToml)?.[1];
if (!version) throw new Error("extension.toml does not declare a version");

const roots = process.argv.slice(2).map((path) => resolve(path));
if (roots.length === 0) roots.push(join(repository, "artifacts"));
const archives = roots
  .flatMap(walk)
  .filter((path) => path.endsWith(".tar.gz") || path.endsWith(".zip"))
  .sort();
const verifiedTargets = new Set(
  archives.map((archive) => verifyArchive(archive, version)),
);
if (
  verifiedTargets.size !== expectedTargets.size ||
  [...expectedTargets].some((target) => !verifiedTargets.has(target))
) {
  throw new Error(
    `release matrix is incomplete: verified ${[...verifiedTargets].sort().join(", ")}`,
  );
}
console.log(`RustOwl release matrix verified (${verifiedTargets.size} targets).`);
