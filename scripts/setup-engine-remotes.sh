#!/usr/bin/env bash

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
engine_dir="$repo_root/engine"
upstream_url="https://github.com/cordx56/rustowl.git"

git -C "$repo_root" submodule update --init --recursive

if git -C "$engine_dir" remote get-url upstream >/dev/null 2>&1; then
  git -C "$engine_dir" remote set-url upstream "$upstream_url"
else
  git -C "$engine_dir" remote add upstream "$upstream_url"
fi

git -C "$engine_dir" fetch upstream --prune

printf 'Engine remotes configured:\n'
git -C "$engine_dir" remote -v
