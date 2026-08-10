# Research references

These pinned submodules are a reproducible design library for the RustOwl
ownership cockpit. They are not production dependencies, are excluded from
release artifacts, and must not be linked into the extension merely because
they are present here.

Run `git submodule update --init --recursive --depth 1` to fetch them. Review
upstream changes deliberately; never configure these submodules to follow a
moving revision in a production build.

| Project | Pinned revision | What we study | License boundary |
| --- | --- | --- | --- |
| [Place Capability Graphs](https://github.com/prusti/pcg) | `c1c8d264dde998830abd07183438632b539ecf21` | Capabilities, places, lifetime projections, reborrowing, packing/unpacking, loop and function abstractions | No repository license detected at this revision. Study published interfaces and behavior only; do not copy or derive source without written permission or a subsequently verified license. |
| [Flowistry](https://github.com/willcrichton/flowistry) | `693ceda925bd1d39d8de413ce239cfa6a87bb665` | MIR information flow, modular function summaries, focus-mode editor UX | MIT; keep attribution and perform a separate dependency review before reuse. |
| [Aquascope](https://github.com/cognitive-engineering-lab/aquascope) | `f2d0a06034b86765a91f10b7c8e40ec31ecb87a6` | Permission vocabulary, source-linked teaching views, separation of static permissions from runtime interpretation | MIT; keep attribution and preserve the static/runtime distinction. |
| [RustViz](https://github.com/rustviz/rustviz) | `dfd4d0e7a4a3b4beddac50a85902a38af85230f0` | Ownership timelines and approachable visual grammar | MIT; keep attribution and avoid importing presentation code without review. |
| [BORIS](https://github.com/ChristianSchott/boris) | `f543db37b1211180862dcad0e1e0337f4c4a605d` | Interactive timelines, source-to-visual navigation, limitations of syntax/rust-analyzer-only ownership models | No repository license detected at this revision. Study behavior and documentation only; do not copy or derive source. |
| [OwnSight](https://github.com/dedsecrattle/OwnSight) | `98c3c788dfdc859d4a2c5d199e6087a9c816e672` | Teaching/debug modes, ownership questions, event and timeline vocabulary | Cargo metadata says `MIT OR Apache-2.0`, but no root license text is present. Treat as study-only until provenance is clarified. |
| [mind-expander](https://github.com/mbbill/mind-expander) | `1182586b80be4747284c69b2dcff27a9a79b551c` | Source-backed infinite canvas, agent-guided tours, architecture and change overlays | Apache-2.0; keep attribution and separate its structural graph from compiler ownership evidence. |
| [rust-analyzer](https://github.com/rust-lang/rust-analyzer) | `513f60bfe6641ed48072862cf4c1821696e2630d` | Incremental project model, invalidation, symbol and call identities, editor performance patterns | MIT OR Apache-2.0; RustOwl complements rather than replaces Zed's native rust-analyzer. |
| [HelixDB](https://github.com/HelixDB/helix-db) | `ce7392958f466d118328864d7e514e58ad01204f` | Embedded writer/read-only APIs, bounded local graph persistence, query ergonomics, durability and recovery behavior | Apache-2.0; an approved production dependency only through an explicit pinned Cargo dependency and release audit. The reference checkout itself is excluded from release artifacts. |

The maintained RustOwl fork is already pinned separately at [`../engine`](../engine)
because it is product source rather than a research-only reference.

## Research gates

Before graph schema version 1 is frozen:

1. Compare the RustOwl place/capability model with PCG on partial moves,
   reborrowing, nested references, branch joins, loops, and function calls.
2. Prototype modular function ownership summaries and compare their results to
   explicit interprocedural traversal, using Flowistry's published approach as
   prior art.
3. Run visual-language reviews against Aquascope, RustViz, BORIS, and OwnSight
   so ownership, permissions, borrows, liveness, and runtime state cannot be
   confused.
4. Test cockpit and agent-guided walkthroughs against mind-expander's
   source-backed navigation principles.
5. Verify that RustOwl and rust-analyzer responsibilities remain complementary
   and that neither server duplicates latency-sensitive editor work.
6. Exercise HelixDB's embedded writer/read-only clients against the project-owned
   revision contract, including atomic activation, crash recovery, bounded
   queries, schema migration, and resource ceilings before enabling it by
   default in release builds.

Record conclusions as project-owned ADRs. Reference code never becomes product
code through an undocumented copy or an implicit Cargo path dependency.
