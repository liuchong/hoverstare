#!/usr/bin/env bash
# M3 验收 fixture：构造"行为变更破坏调用方"场景
#   base 分支：make_id 返回稳定 ID
#   main 分支：make_id 追加时间戳（不稳定）——diff 只看得到 ids.rs，bug 藏在 store.rs 的调用方
#
# 用法: tests/fixtures/setup_caller_repo.sh [目录]
# 输出: <目录>（git 仓库）与 <目录>.diff（base..main 的 diff）

set -euo pipefail

DIR="${1:-/tmp/bugbot-caller-repo}"
SRC="$(cd "$(dirname "$0")" && pwd)/caller_repo"

rm -rf "$DIR" "$DIR.diff"
mkdir -p "$DIR/src"

cp "$SRC/lib.rs"   "$DIR/src/lib.rs"
cp "$SRC/store.rs" "$DIR/src/store.rs"
cp "$SRC/ids_old.rs" "$DIR/src/ids.rs"

cd "$DIR"
git init -q -b main
git add -A
git -c user.email=fixture@bugbot -c user.name=fixture commit -qm "base: stable make_id"
git branch base

cp "$SRC/ids_new.rs" src/ids.rs
git add -A
git -c user.email=fixture@bugbot -c user.name=fixture commit -qm "feat: append timestamp to make_id"

git diff base main > "$DIR.diff"
echo "fixture repo: $DIR"
echo "diff:         $DIR.diff"
