#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

KB_PATH="${1:-lepiter-core/tests/fixtures/corpus}"
OUT_FILE="docs/snippet-support-matrix.md"

cargo run -q -p lepiter-core --example probe -- --matrix-md "$KB_PATH" > "$OUT_FILE"
echo "updated $OUT_FILE from $KB_PATH"
