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

Current extension releases download an unmodified RustOwl release from the
upstream GitHub repository at runtime. The checked-out engine fork is for
ongoing engine and protocol development and is not relicensed under the outer
repository's MIT license.

The adapter in this repository is an independently written client of
RustOwl's documented `rustowl/cursor` and `rustowl/analyze` LSP methods. No
RustOwl source code is incorporated into the adapter.

## Research-only submodules

The `references/` submodules are pinned upstream research projects used for
architecture, semantics, UX, and interoperability studies. They are not linked
into the extension, adapter, or RustOwl runtime and are excluded from release
archives. Their source remains governed solely by each upstream project's
license. Projects without clear license text are study-only and their source
must not be copied or adapted. Exact revisions and license boundaries are
recorded in [`references/README.md`](references/README.md).

HelixDB is additionally being evaluated as the embedded persistence engine for
the ownership graph. HelixDB is licensed under Apache-2.0. If it is linked into
a release, its license and required notices will ship with that release; the
RustOwl graph schema, certainty model, revision protocol, privacy policy, and
query limits remain project-owned contracts rather than HelixDB semantics.
