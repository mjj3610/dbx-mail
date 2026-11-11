#!/usr/bin/env bash
# Build the dbx-mail plugin UI (vite via npm) and package a .dbxp for the
# current platform. Requires: node + npm on PATH, cargo for the Rust sidecar.
# The dbx-plugin CLI bundles its own SDK (no DBX source checkout needed).
set -euo pipefail
cd "$(dirname "$0")/.."

fast=0
for arg in "$@"; do
  case "$arg" in
    --fast) fast=1 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

if ! command -v npm >/dev/null 2>&1; then
  # Node 版本真源与四插件一致：工作区根 .nvmrc；精确版本缺失时回退 nvm 里最新的 v22。
  NODE_VER="$(head -n1 ../.nvmrc 2>/dev/null | tr -d 'vV\n ')"
  NODE_BIN="$(ls -d "$HOME/.nvm/versions/node/v${NODE_VER:-22.21.0}"/bin 2>/dev/null \
    || ls -d "$HOME"/.nvm/versions/node/v22*/bin 2>/dev/null | sort -V | tail -1 || true)"
  export PATH="${NODE_BIN:+$NODE_BIN:}$PATH"
fi
# Prepend only when cargo is not already resolvable（同 files）：release CI 的
# cargo→cargo-zigbuild 包装器必须在 PATH 前列，无条件 prepend 会把它遮掉。
if ! command -v cargo >/dev/null 2>&1; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi

# Release CI builds the target-independent UI once per plugin and stages ui/
# here as an artifact; DBX_PREBUILT_UI=1 packages it as-is instead of rerunning
# the frontend steps on every platform job.
if [ "${DBX_PREBUILT_UI:-0}" = "1" ]; then
  if [ ! -f ui/index.html ]; then
    echo "DBX_PREBUILT_UI=1 but ui/index.html is missing; stage the CI frontend artifact first" >&2
    exit 1
  fi
  echo "==> frontend: skipped (prebuilt ui/ staged by CI)"
else
  if [ "$fast" -eq 1 ]; then
    echo "==> frontend: install + build (--fast: skipping typecheck)"
  else
    echo "==> frontend: install + typecheck + build"
  fi
  # node_modules 新鲜度检查同 files/ldap 的取舍：lockfile 没变就跳过安装。
  if [ ! -d node_modules ] || [ package-lock.json -nt node_modules ]; then
    npm ci --no-audit --no-fund
  fi
  if [ "$fast" -eq 1 ]; then
    npm run build
  else
    npm run typecheck
    npm run build
  fi
fi

# dbx-plugin package runs its own `cargo build` for the Rust backend; without
# CARGO_TARGET_DIR it builds into a throwaway dist/.build-rust-<triple> staging
# dir (wiped afterwards) = full dep-tree rebuild on every release. Redirect it
# to the warm backend/target — verified the CLI reuses and keeps it.
export CARGO_TARGET_DIR="$PWD/backend/target"

echo "==> sidecar release build"
cargo build --release --manifest-path backend/Cargo.toml

echo "==> package .dbxp"
unset DBX_PLUGIN_SDK_ROOT
# host 子仓库现构建的 CLI 优先：它与宿主最新源码一致，对「backend 不依赖 SDK →
# 打包器注入的 patch 未被使用」场景不要求 Cargo.lock 记录 [[patch.unused]]；
# npm 全局 CLI（实测 0.1.9/0.1.10 均如此）要求该记录，而任何不带 patch 的
# unlocked cargo 构建又会把它剪掉，锁定状态无法稳定维持。无 host 子仓库时
# 回退系统 CLI。构建 host CLI 必须剥离 CARGO_TARGET_DIR（已指向本插件
# backend/target），否则 CLI 二进制会落进插件 target 而非 host 子仓库预期路径。
HOST="${DBX_HOST_WORKTREE:-$PWD/../host}"
if [ -d "$HOST/plugins/sdk/cli" ]; then
  if [ ! -x "$HOST/plugins/sdk/cli/target/release/dbx-plugin" ]; then
    (cd "$HOST/plugins/sdk/cli" && env -u CARGO_TARGET_DIR cargo build --release)
  fi
  export PATH="$HOST/plugins/sdk/cli/target/release:$PATH"
fi
NO_COLOR=1 dbx-plugin package .

echo
echo "Artifacts:"
ls -la dist/*.dbxp dist/*.artifact.json 2>/dev/null || true
