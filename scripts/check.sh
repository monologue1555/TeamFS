#!/usr/bin/env bash
# 全套回归：每个套件都会运行并记录退出码，前面失败也不跳过后续套件。
# 最后生成 artifacts/regression-summary.json/.md；任一套件失败、产物缺失或未更新时退出非零。
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"
mkdir -p -- "$TEAMFS_ROOT/artifacts"
runs="$(mktemp)"
trap 'rm -f -- "$runs"' EXIT
# 记录运行前的源码状态；artifacts 是本次运行的输出，不算源码修改。
git_query() { git -C "$TEAMFS_ROOT" "$@" 2>/dev/null || true; }
commit="$(git_query rev-parse HEAD)"
branch="$(git_query rev-parse --abbrev-ref HEAD)"
dirty="$(git_query status --porcelain -- . ':(exclude)artifacts')"
python3 -c 'import json,sys; print(json.dumps({"source":{"commit":sys.argv[1] or None,"branch":sys.argv[2] or None,"dirty":bool(sys.argv[3])}}))' \
    "$commit" "$branch" "$dirty" > "$runs"
# Wall clock for artifact freshness; monotonic clock for elapsed time, because
# WSL can step the wall clock while a long suite is running.
monotonic_ns() {
    local uptime idle whole fraction
    read -r uptime idle < /proc/uptime
    whole=${uptime%.*}
    fraction=${uptime#*.}
    printf '%d\n' "$((10#$whole * 1000000000 + 10#$fraction * 10000000))"
}
run_suite() {
    local name=$1 display=$2 started mono_started code
    shift 2
    started=$(date +%s%N)
    mono_started=$(monotonic_ns)
    set +e
    "$@"
    code=$?
    set -e
    printf '{"suite":"%s","command":"%s","exit_code":%d,"started_ns":%d,"elapsed_ns":%d}\n' \
        "$name" "$display" "$code" "$started" "$(( $(monotonic_ns) - mono_started ))" >> "$runs"
}
unit_tests() { (cd -- "$TEAMFS_ROOT" && cargo test --locked) 2>&1 | tee "$TEAMFS_ROOT/artifacts/model-tests.txt"; }
run_suite unit "cargo test --locked" unit_tests
run_suite smoke "bash scripts/smoke.sh" bash "$TEAMFS_ROOT/scripts/smoke.sh"
run_suite v2 "bash scripts/test-v2.sh" bash "$TEAMFS_ROOT/scripts/test-v2.sh"
run_suite v3 "bash scripts/test-v3.sh" bash "$TEAMFS_ROOT/scripts/test-v3.sh"
run_suite v4 "bash scripts/test-v4.sh" bash "$TEAMFS_ROOT/scripts/test-v4.sh"
run_suite v5 "bash scripts/test-v5.sh" bash "$TEAMFS_ROOT/scripts/test-v5.sh"
run_suite v6 "bash scripts/test-v6.sh" bash "$TEAMFS_ROOT/scripts/test-v6.sh"
run_suite blocks "bash scripts/test-blocks.sh" bash "$TEAMFS_ROOT/scripts/test-blocks.sh"
run_suite experiments "bash scripts/test-experiments.sh" bash "$TEAMFS_ROOT/scripts/test-experiments.sh"
run_suite attribution "bash scripts/test-attribution.sh" bash "$TEAMFS_ROOT/scripts/test-attribution.sh"
run_suite member3 "bash scripts/test-member3.sh" bash "$TEAMFS_ROOT/scripts/test-member3.sh"
run_suite member4 "bash scripts/test-member4.sh" bash "$TEAMFS_ROOT/scripts/test-member4.sh"
python3 "$TEAMFS_ROOT/tests/regression_summary.py" "$runs"
