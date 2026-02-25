#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found in PATH" >&2
  exit 1
fi

echo "==> fmt check"
cargo fmt --all -- --check

echo "==> clippy"
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "==> docs (warnings denied)"
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps

echo "==> tests"
cargo test --workspace

if command -v unsafe-budget >/dev/null 2>&1 || cargo unsafe-budget --help >/dev/null 2>&1; then
  echo "==> unsafe gate"
  cargo unsafe-budget check --workspace-only
else
  echo "warning: unsafe-budget not found; skipping unsafe gate" >&2
  echo "install with: cargo install unsafe-budget" >&2
fi

if command -v cargo-llvm-cov >/dev/null 2>&1 || cargo llvm-cov --version >/dev/null 2>&1; then
  echo "==> coverage (lepiter-core lines >= 75%)"
  cargo llvm-cov --package lepiter-core --all-features --fail-under-lines 75 --summary-only
else
  echo "warning: cargo-llvm-cov not found; skipping coverage check" >&2
  echo "install with: cargo install cargo-llvm-cov" >&2
fi

echo "==> all checks passed"
