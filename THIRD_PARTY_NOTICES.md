# Third-party notices

## RustOwl

This project is an unofficial editor client for
[RustOwl](https://github.com/cordx56/rustowl), created and maintained by
[Kota Mori (`cordx56`)](https://github.com/cordx56).

RustOwl is licensed under the Mozilla Public License 2.0. Its source is pinned
as the `engine` Git submodule from the
[RustyAuth-maintained fork](https://github.com/rusty-auth/rustowl-engine),
which retains the original project as its GitHub upstream. Upstream-derived
source files and our modifications remain MPL-2.0 and preserve their existing
notices. The original source and history remain available from the
[upstream repository](https://github.com/cordx56/rustowl).

Runtime archives include the maintained RustOwl engine fork and its `rustowlc`
compiler wrapper. Corresponding source remains available through the pinned
`engine` revision and its public fork. These MPL-covered binaries and source
are not relicensed under the outer repository's MIT license.

The adapter in this repository is an independently written client of
RustOwl's documented and maintained LSP methods. No RustOwl source code is
incorporated into the adapter.

## HelixDB

[HelixDB](https://github.com/HelixDB/helix-db) is the embedded persistence
engine linked into the maintained RustOwl and MCP binaries. It is used at the
immutable Git revision `ce7392958f466d118328864d7e514e58ad01204f` and is
licensed under Apache-2.0. Runtime archives ship its Apache-2.0 license. The
same revision is mirrored in `references/helix-db` for review; the Cargo
dependency and lockfile, rather than that checkout, are the production source
pin.

## Research-only submodules

The `references/` submodules are pinned upstream research projects used for
architecture, semantics, UX, and interoperability studies. They are not linked
into the extension, adapter, or RustOwl runtime and are excluded from release
archives. Their source remains governed solely by each upstream project's
license. Projects without clear license text are study-only and their source
must not be copied or adapted. Exact revisions and license boundaries are
recorded in [`references/README.md`](references/README.md).

HelixDB is the sole exception to the research-only dependency rule above. Its
production use is explicitly pinned and audited as described in the preceding
section. The RustOwl graph schema, certainty model, revision protocol, privacy
policy, and query limits remain project-owned contracts rather than HelixDB
semantics.
