# Contributing

Thank you for helping bring RustOwl to Zed.

## Ground rules

- Keep the project clearly described as an unofficial RustOwl client.
- Preserve attribution to Kota Mori and the upstream RustOwl project.
- Keep upstream-derived engine code and modifications under MPL-2.0 in the
  `engine` submodule; the outer extension and adapter remain MIT-licensed.
- Prefer standard LSP features in the adapter so the Zed extension stays
  within Zed's supported extension surface.

## Engine development

The RustOwl engine is pinned as a Git submodule at `engine`. Its `origin` is
the RustyAuth-maintained fork and its `upstream` remote is the original
RustOwl repository.

```sh
git submodule update --init --recursive
./scripts/setup-engine-remotes.sh
```

To incorporate upstream work, fetch it explicitly and review the changes
before merging them into the fork:

```sh
git -C engine fetch upstream --prune
git -C engine log --oneline main..upstream/main
git -C engine merge upstream/main
git -C engine push origin main
```

Commit and push engine changes inside `engine` first. Then commit the updated
submodule pointer in this repository. Do not remove or rewrite upstream
copyright or license notices.

## Local checks

```sh
cargo fmt --all -- --check
cargo check --target wasm32-wasip2
cargo fmt --manifest-path adapter/Cargo.toml -- --check
cargo clippy --manifest-path adapter/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path adapter/Cargo.toml
```

Test changes as a Zed dev extension before opening a pull request whenever the
change affects runtime behaviour.

## Releases

Versions in `Cargo.toml`, `adapter/Cargo.toml`, and `extension.toml` must match.
Pushing a `v*` tag builds adapter artifacts and creates a GitHub release.
