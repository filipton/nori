#!/usr/bin/env bash
# One heavy host check at a time across worktrees; test selection stays Cargo's.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here/.."
case "${1:-}" in
  rust) shift; exec python3 "$here/with-resource.py" build cargo nextest run --build-jobs 4 --test-threads 4 "$@" ;;
  rust-doc) shift; exec python3 "$here/with-resource.py" build cargo test -j4 --doc "$@" ;;
  android) shift; exec python3 "$here/with-resource.py" build ./gradlew :app:assembleDebug "$@" ;;
  bazel) shift; exec python3 "$here/with-resource.py" build bazel "$@" ;;
  *) echo "usage: check.sh rust [--workspace|-p CRATE ...]|rust-doc [...]|android [GRADLE OPTIONS]|bazel COMMAND [...]" >&2; exit 2 ;;
esac
