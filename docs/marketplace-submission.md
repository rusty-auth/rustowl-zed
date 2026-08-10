# Zed Extension Marketplace release

RustOwl uses the extension ID `rustowl`. The ID is unique, describes the
tooling, and deliberately omits the reserved words “Zed” and “extension”. The
root MIT license covers the extension and adapter; the downloaded MPL-2.0 and
Apache-2.0 tools retain their own licenses and notices.

The canonical publishing requirements are in Zed's
[Developing Extensions](https://zed.dev/docs/extensions/developing-extensions)
guide. Before opening the registry pull request:

1. Merge a clean commit to this repository's public default branch.
2. Tag the same version as `extension.toml` (`v0.1.3` for this candidate).
3. Wait for the release workflow to publish all six
   `rustowl-zed-runtime-<target>` archives.
4. Verify each archive contains four binaries, the compatibility manifest,
   CycloneDX SBOM, checksums, licenses, and notices.
5. Perform clean-install LSP and MCP smoke tests from the published archive.

Zed publishes community extensions from
[`zed-industries/extensions`](https://github.com/zed-industries/extensions).
From a personal fork of that repository:

```sh
git submodule add https://github.com/rusty-auth/rustowl-zed.git extensions/rustowl
```

Add this top-level registry entry:

```toml
[rustowl]
submodule = "extensions/rustowl"
version = "0.1.3"
```

Then run `pnpm sort-extensions`, commit the submodule and manifest changes, and
open a pull request to `zed-industries/extensions`. The submodule URL must use
HTTPS, the pinned commit must be reachable from a branch, and the version must
match `extension.toml` at that commit.

The pull-request description should state that RustOwl:

- complements Zed's native rust-analyzer rather than replacing it;
- downloads a target-specific native language/MCP runtime through the Zed
  extension API rather than embedding it in the extension WASM;
- analyzes and persists compiler evidence locally;
- exposes only bounded, read-only MCP tools;
- attributes Kota Mori's original RustOwl work and preserves MPL-2.0; and
- has passed the linked clean-install and live-editor evidence matrix.

For an update, advance the `extensions/rustowl` submodule, update the registry
version to match `extension.toml`, rerun `pnpm sort-extensions`, and open a new
registry pull request.
