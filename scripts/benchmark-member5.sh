#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/teamfs-member5/target}"
profile="${TEAMFS_BENCH_PROFILE:-release}"
case "$profile" in
  release) build_args=(--release) ;;
  debug) build_args=() ;;
  *) echo 'TEAMFS_BENCH_PROFILE must be release or debug' >&2; exit 2 ;;
esac
(cd -- "$root" && cargo build --locked "${build_args[@]}")
exec python3 "$root/tests/member5_benchmark.py" \
  --binary "$CARGO_TARGET_DIR/$profile/teamfs" --profile "$profile" "$@"
