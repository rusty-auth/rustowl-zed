# Contributing

Thank you for helping bring RustOwl to Zed.

## Ground rules

- Keep the project clearly described as an unofficial RustOwl client.
- Preserve attribution to Kota Mori and the upstream RustOwl project.
- Do not copy upstream RustOwl source into this MIT-licensed repository.
- Prefer standard LSP features in the adapter so the Zed extension stays
  within Zed's supported extension surface.

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
