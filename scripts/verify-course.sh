#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)

"$repo_root/scripts/check-isolation.sh"

(
  cd "$repo_root/rust-backend"
  cargo fmt --check
  cargo clippy --all-targets --all-features -- -D warnings
  NO_PROXY=127.0.0.1,localhost,::1 \
    no_proxy=127.0.0.1,localhost,::1 \
    cargo test --all-targets
)

(
  cd "$repo_root/web"
  npm ci
  npm test
  npm run build
)

"$repo_root/scripts/check-isolation.sh"
