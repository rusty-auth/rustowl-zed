# RustOwl engine architecture

The production delivery sequence, acceptance gates, Zed Agent integration,
and packaging plan are defined in the
[`ownership cockpit roadmap`](ownership-cockpit-roadmap.md).

The RustyAuth-maintained engine fork turns compiler-grounded RustOwl results
into a reusable ownership graph for editor visuals, Mermaid diagrams, and
agent tooling. It remains an MPL-2.0 project derived from
[`cordx56/rustowl`](https://github.com/cordx56/rustowl).

## Responsibilities

- `rustc` MIR, borrow checking, and Polonius remain the source of truth.
- The engine emits structured graph data rather than presentation-specific
  Markdown or Mermaid.
- Editor clients choose compact inline, hover, or side-pane presentations.
- Every result carries a document version and a certainty level so clients do
  not display stale or inferred information as compiler-proven fact.

The indexed protocol exposes:

- `rustowl/inspectRange` — all ownership events for a visible source range in
  one request;
- `rustowl/ownershipGraph` — nodes, typed edges, spans, and certainty for a
  selected binding or function; and
- `rustowl/analysisUpdated` — a versioned notification that lets clients
  refresh without polling.

## Graph model

Nodes represent workspace, crate, module, function, binding, place, call,
suspension, return, and drop events. Edges represent ownership and control
relationships such as `owns`, `moves_to`, `borrows_shared`, `borrows_mut`,
`mutates_through`, `returns_as`, `live_across_await`, and `drops_at`.

Compiler identity and source spans belong in the graph. Mermaid node labels do
not: Mermaid is one renderer over the structured result, not the storage
format.

## HelixDB

[HelixDB](https://github.com/HelixDB/helix-db) is the embedded persisted
workspace graph. It provides a labeled property graph, traversals, typed
properties, and an embedded Rust execution mode and is pinned immutably under
Apache-2.0.

HelixDB should sit behind an engine-owned storage trait instead of becoming a
hard dependency of the ownership analysis pipeline:

```rust
trait GraphStore {
    async fn stage(&self, graph: WorkspaceGraph) -> Result<StageReceipt>;
    async fn activate(&self, workspace: &str, revision: &StableId) -> Result<ActivationReceipt>;
    async fn load_active(&self, workspace: &str) -> Result<Option<WorkspaceGraph>>;
}
```

The in-memory adjacency graph serves hover and inline requests. Every valid
revision is also atomically written to a bounded immutable portable snapshot
before Helix publication begins. Helix persists native indexed nodes/edges plus
the canonical revision snapshot in two bounded A/B generations for cross-file
exploration and read-only MCP queries. A separate MCP process validates both
durable sources and selects the higher monotonic revision sequence. The new
Helix generation is validated before an atomic slot-pointer switch; the other
is the rollback generation. This keeps the hot editor path independent from
database startup and durability work without leaving agents Helix-dependent.

The parity, rotation, reader, regression, and recovery tests enforce that the
editor experience remains available when persistence is disabled or
unavailable. Benchmarks track cold startup, six-hop traversal, full revision
replacement, database size, and shutdown behavior.
