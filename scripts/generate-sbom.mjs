#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = resolve(process.argv[2] ?? "sbom.cdx.json");
const manifests = [
  resolve(repository, "Cargo.toml"),
  resolve(repository, "adapter", "Cargo.toml"),
  resolve(repository, "engine", "Cargo.toml"),
];

function metadata(manifest) {
  return JSON.parse(
    execFileSync(
      "cargo",
      [
        "metadata",
        "--locked",
        "--format-version",
        "1",
        "--manifest-path",
        manifest,
      ],
      { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
    ),
  );
}

function cargoPurl(pkg) {
  if (pkg.source?.startsWith("registry+")) {
    return `pkg:cargo/${encodeURIComponent(pkg.name)}@${encodeURIComponent(pkg.version)}`;
  }
  return undefined;
}

const packages = new Map();
const dependencies = new Map();
for (const graph of manifests.map(metadata)) {
  for (const pkg of graph.packages) packages.set(pkg.id, pkg);
  for (const node of graph.resolve?.nodes ?? []) {
    const refs = dependencies.get(node.id) ?? new Set();
    for (const dependency of node.dependencies) refs.add(dependency);
    dependencies.set(node.id, refs);
  }
}

const components = [...packages.values()]
  .sort((left, right) => left.id.localeCompare(right.id))
  .map((pkg) => {
    const component = {
      type: pkg.targets.some((target) => target.kind.includes("bin"))
        ? "application"
        : "library",
      "bom-ref": pkg.id,
      name: pkg.name,
      version: pkg.version,
    };
    if (pkg.license) component.licenses = [{ expression: pkg.license }];
    if (cargoPurl(pkg)) component.purl = cargoPurl(pkg);
    if (pkg.source) {
      component.properties = [
        { name: "cargo:source", value: pkg.source },
      ];
    }
    if (pkg.repository) {
      component.externalReferences = [
        { type: "vcs", url: pkg.repository },
      ];
    }
    return component;
  });

const dependencyGraph = [...packages.keys()]
  .sort()
  .map((id) => ({
    ref: id,
    dependsOn: [...(dependencies.get(id) ?? [])]
      .filter((dependency) => packages.has(dependency))
      .sort(),
  }));

const sbom = {
  bomFormat: "CycloneDX",
  specVersion: "1.6",
  serialNumber: `urn:uuid:${randomUUID()}`,
  version: 1,
  metadata: {
    timestamp: new Date().toISOString(),
    component: {
      type: "application",
      name: "rustowl-zed-runtime",
      version: "0.1.3",
    },
  },
  components,
  dependencies: dependencyGraph,
};

writeFileSync(output, `${JSON.stringify(sbom, null, 2)}\n`);
console.log(`Wrote ${components.length} components to ${output}`);
