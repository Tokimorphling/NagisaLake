#!/usr/bin/env bash
# 本地跑一遍 CI 会跑的检查。提交前执行，避免把可本地发现的失败推上去。
#
#   ./scripts/check.sh          # Rust 检查
#   ./scripts/check.sh --web    # 额外检查并构建前端控制台
#
set -euo pipefail

cd "$(dirname "$0")/.."

step() { printf '\n\033[1m==> %s\033[0m\n' "$1"; }

step "Clippy"
cargo clippy --workspace --all-targets -- -D warnings

step "测试"
# 未设置 NAGISALAKE_TEST_DATABASE_URL 时 PostgreSQL 集成测试会静默跳过。
if [[ -z "${NAGISALAKE_TEST_DATABASE_URL:-}" ]]; then
  printf '注意：NAGISALAKE_TEST_DATABASE_URL 未设置，PostgreSQL 集成测试将跳过\n'
fi
cargo test --workspace --all-targets

step "ComfyUI python 扩展（只做类型检查）"
# 不跑测试：该 feature 链接 libpython，运行期加载会失败。
cargo check -p nagisalake-worker --features python

if [[ "${1:-}" == "--web" ]]; then
  step "前端"
  (cd web && pnpm install --frozen-lockfile && pnpm typecheck && pnpm test && pnpm build)
fi

printf '\n\033[32m全部通过\033[0m\n'
