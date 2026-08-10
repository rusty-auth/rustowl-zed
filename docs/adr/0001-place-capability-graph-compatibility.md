# ADR 0001: Place Capability Graph compatibility boundary

- Status: accepted for M0; schema freeze pending compatibility corpus
- Date: 2026-08-10

## Context

RustOwl already exposes rustc/MIR/borrow-checker facts, but its current model is
organized around locals and decoration ranges. That representation is not
sufficient for partial moves, projections, reborrowing, nested references,
branch joins, loops, or modular calls.

The published
[Place Capability Graph](https://arxiv.org/abs/2503.21691) model addresses
these cases with places, lifetime projections, access capabilities, borrow
flow, pack/unpack operations, and call/loop abstractions. Its authors report
successful models for more than 98% of functions in their evaluation corpus.
The corresponding [PCG implementation](https://github.com/prusti/pcg) is
pinned in `references/pcg` for study, but its checked-out revision does not
contain a detectable repository license.

## Decision

RustOwl's persisted graph is a project-owned interchange and evidence graph,
not an independent attempt to replace PCG's dataflow state machine.

The schema includes compatible concepts for:

- root and projected MIR places;
- lifetime projections distinct from places;
- `read`, `write`, `exclusive`, and `shallow_exclusive` capability snapshots;
- borrow, reborrow, alias, move, copy, mutation, pack, unpack, return, and drop
  relationships;
- before/after program points, branch joins, loop invariants, and function
  summaries; and
- explicit compiler-proven, source-resolved, conservative, and unresolved
  certainty.

RustOwl will not claim PCG equivalence until an independently created fixture
corpus compares partial moves, nested projections, reborrows, nested
lifetimes, joins, loops, calls, and unsupported boundaries.

We will not copy, adapt, link, or redistribute source from the pinned PCG
repository without a verified compatible license. If a licensed PCG release
becomes available, we will evaluate using it inside the compiler extraction
layer. Storage, LSP, MCP, and UI will continue consuming the project-owned
versioned contract so the runtime is not coupled to a particular analyzer.

## Consequences

- Schema v1 cannot freeze around binding-only ownership edges.
- Place projection identity and capability state are compiler-layer concerns;
  HelixDB cannot manufacture them.
- Event history and capability snapshots remain distinct: an event explains
  what happened in the static transfer model, while a snapshot explains what
  access is permitted at a program point.
- Unsupported raw pointers, FFI, dynamic dispatch, macros, and unsafe behavior
  terminate or conservatively widen a path instead of disappearing.
- PCG compatibility tests become a release gate rather than an attribution or
  marketing claim.

## Validation

M0 must provide deterministic graph snapshots for:

1. a partial field move followed by reinitialization;
2. shared and mutable reborrows through nested references;
3. a branch join with different borrowed fields;
4. a loop-carried borrow requiring an invariant/abstraction;
5. a function returning a reference tied to more than one input;
6. a composite value containing references; and
7. an unresolved unsafe/raw-pointer boundary.

Each fixture must validate endpoints, projection identity, capability language,
certainty, and stable serialization across repeated analysis.
