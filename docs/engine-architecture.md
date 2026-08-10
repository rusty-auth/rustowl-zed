# RustOwl engine architecture

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

The first protocol additions should be:

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

[HelixDB](https://github.com/HelixDB/helix-db) is a strong candidate for the
persisted workspace graph because it provides a labeled property graph,
traversals, typed properties, and an embedded Rust execution mode. It is
Apache-2.0 licensed.

HelixDB should sit behind an engine-owned storage trait instead of becoming a
hard dependency of the ownership analysis pipeline:

```rust
trait OwnershipGraphStore {
    async fn replace_function(&self, graph: FunctionGraph) -> Result<()>;
    async fn flow_from(&self, node: NodeId, limits: FlowLimits) -> Result<OwnershipFlow>;
    async fn invalidate_revision(&self, revision: Revision) -> Result<()>;
}
```

The initial implementation should be an in-memory adjacency graph used by
hover and inline requests. An optional embedded HelixDB implementation can
persist cross-file and historical graphs for side-pane exploration and MCP
queries. This keeps the hot editor path independent from database startup,
durability, or object-storage work.

Before adopting HelixDB in release binaries, benchmark both implementations
on cold startup, single-function replacement, six-hop traversal, full-crate
replacement, database size, and shutdown flush time. The editor experience
must continue working when persistence is disabled or unavailable.
