# ADR 0002: Hybrid modular and body-aware ownership summaries

- Status: accepted for prototype; precision thresholds pending benchmarks
- Date: 2026-08-10

## Context

Whole-program recursion can answer deep ownership questions when every body is
available, but it is expensive, fragile across dependencies, and unbounded for
recursive or dynamic call graphs. Signature-only analysis is fast and works
for compiled dependencies, but must conservatively approximate effects.

[Flowistry](https://github.com/willcrichton/flowistry) demonstrates a modular
information-flow analysis based on Rust ownership and types. Its published
evaluation reports that modular and whole-program flows were identical in 94%
of evaluated cases. Its implementation also exposes a useful precision switch
between signature-only and recursive local-body analysis.

Ownership lifecycle tracing is not identical to information-flow analysis, so
that result is prior art and a benchmark target—not proof that the same
precision holds for RustOwl.

## Decision

RustOwl will build one immutable `FunctionSummary` per analyzed compiler
function identity. A summary records:

- parameter, receiver, return, captured, and externally visible place shapes;
- move/copy consumption and return relationships;
- shared/mutable borrow and reborrow relationships;
- direct and indirect mutation effects;
- drop and ownership-retention effects;
- async suspension retention, cancellation/drop, `Send`, and `'static`
  evidence where available;
- unresolved/dynamic/unsafe boundaries; and
- the source revision, compiler identity, toolchain, certainty, and summary
  algorithm version.

Queries use a hybrid strategy:

1. Use compiler-proven summary edges for dependencies and unavailable bodies.
2. Expand local bodies when the question requires statement-level evidence and
   the caller's depth/event/time budget allows it.
3. Stop recursive cycles at a stable summary boundary.
4. Preserve both paths when dynamic dispatch has several bounded candidates;
   otherwise emit one explicit unresolved boundary.
5. Mark signature-derived effects conservative unless compiler identities and
   borrow facts prove a stronger relationship.

Summary cache identity includes crate disambiguation, compiler function
identity, substitutions/monomorphization where relevant, source fingerprint,
toolchain, target/features, and algorithm version.

## Consequences

- Six-function traces do not require unrestricted recursive analysis.
- Dependency code can contribute useful bounded ownership effects without
  indexing all dependency source by default.
- Agents and UI must display whether an edge came from a body, a compiler
  summary, a signature approximation, or an unresolved boundary.
- Changing a signature invalidates callers' summary links; changing only a
  body invalidates the body summary and transitive summaries whose externally
  visible effects changed.
- RustOwl keeps its own ownership semantics and does not import Flowistry's
  information-flow result as though it were ownership proof.

## Validation

The benchmark corpus compares summary-only and bounded body-aware queries for:

- owned, shared, and mutable parameters;
- returned owned values and references tied to one or several inputs;
- field projections and in-place mutation;
- generics and monomorphizations;
- closures and captures;
- recursive calls;
- trait/static dispatch and trait objects;
- dependencies without source indexing; and
- async functions and futures retained across `.await`.

The release report records equivalence, conservative extra edges, missed
compiler-proven edges, latency, memory, and invalidation fan-out. No precision
percentage is promoted until this ownership-specific corpus passes.
