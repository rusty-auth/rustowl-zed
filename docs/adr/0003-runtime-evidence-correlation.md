# ADR 0003: Runtime evidence is separate, opt-in, and correlated

- Status: accepted for M6
- Date: 2026-08-10

## Context

Static ownership analysis describes compiler-proven and possible program
paths. Developers and agents can also benefit from knowing which paths and
values were observed during a particular failing run. Rust ownership metadata
is largely erased after compilation, and many logical moves do not correspond
to a physical byte copy, so ordinary application logs cannot reconstruct the
borrow checker's model.

Capturing arbitrary values also creates substantial privacy, security,
correctness, and performance risk.

## Decision

Runtime capture is a separate graph namespace and evidence class. It never
mutates or upgrades static certainty. Every runtime event references a build
ID, source/config fingerprint, static analysis revision, compiler-assigned
graph ID, process/thread/task/span identity, and monotonic sequence.

The initial recorder uses opt-in compiler/MIR instrumentation plus `tracing`
correlation. It supports progressively stronger policies:

1. control events only;
2. safe metadata and keyed hashes;
3. explicitly approved values through a dedicated capture trait; and
4. experimental targeted taint propagation.

Capture is started only through an explicit editor command or
`rustowl run --capture`. MCP access is read-only and cannot execute code or
change capture policy. Static and runtime queries join only after validating
the selected run's build, source, toolchain, and graph compatibility.

Arbitrary memory, reference targets, raw pointers, secrets, environment
variables, file/network contents, and general `Debug` output are excluded by
default. Value capture is redacted before persistence, encrypted locally,
bounded by bytes/events/time, and governed by explicit retention and deletion.

## Consequences

- “Observed” always means observed in one named run, not universally true.
- A logical move event indicates that the instrumented MIR operation executed;
  it does not assert a machine-level copy.
- Instrumentation must not retain references, change drop order, call
  surprising user code, or panic across its boundary.
- Disabled capture has no runtime recorder behavior.
- Runtime evidence is omitted from diagnostic bundles and agent responses
  unless the user explicitly selects it.

## Validation

M6 tests cover synchronous and Tokio flows, cross-thread task movement,
suspension/resume/cancellation, panics/unwind, redaction, secret fixtures,
backpressure, crash recovery, retention/deletion, stale-build rejection, and a
six-function observed/static correlation. Benchmarks compare instrumented and
uninstrumented release builds and enforce the roadmap's overhead budget.
