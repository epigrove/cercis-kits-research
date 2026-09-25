#!/usr/bin/env bash
# S7 现场演示：装科研模板 → 写数据 → 换术语包 → 证明数据一字未变
#
# 用法：bash verify/s7_demo.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"                      # kits/research
CERCIS_ROOT="${CERCIS_ROOT:-$(cd "$ROOT/../.." && pwd)}"
KERNEL="${CERCIS_BIN:-$CERCIS_ROOT/kernel/target/debug/cercis}"
PLUGIN="$ROOT/target/debug/cercis-plugin-research-template"
WORK="${TMPDIR:-/tmp}/cercis-s7-demo"
PROJECT="$WORK/proj"

export CERCIS_BIN="$KERNEL"
export CERCIS_RESEARCH_DIR="$ROOT"

for f in "$KERNEL" "$PLUGIN"; do
  [ -x "$f" ] || { echo "缺少可执行文件：$f（先 cargo build）"; exit 1; }
done

rm -rf "$WORK"; mkdir -p "$WORK"

echo "── 1. 建项目 ──"
"$KERNEL" init "$PROJECT" --name "S7 演示（合成数据）" --user demo --json >/dev/null

echo "── 2. 装田间试验模板 + 农业术语包 ──"
"$PLUGIN" install --project "$PROJECT" --template field-trial --terms agriculture --allow-write

echo
echo "── 3. 写两条数据（研究 → 小区）──"
"$KERNEL" append "$PROJECT" --table studies --set study_id=ST-001 --set name=合成试验 --json >/dev/null
"$KERNEL" append "$PROJECT" --table units --set unit_id=U-001 --set study_id=ST-001 --set block=B1 --json >/dev/null

biz_digest() {
  python3 - "$PROJECT" <<'PY'
import hashlib, pathlib, sys
root = pathlib.Path(sys.argv[1]) / "tables"
tables = ["studies","factors","treatments","units","events","samples","observations","methods","media"]
h = hashlib.sha256()
for t in tables:
    h.update((root / f"{t}.csv").read_bytes())
print(h.hexdigest()[:16])
PY
}

label() {
  "$KERNEL" template "$PROJECT" --json | python3 -c "
import json, sys
d = json.load(sys.stdin)
units = next(t for t in d['template']['tables'] if t['name'] == 'units')
print(f\"{d['terms_profile']}: {units['label_cn']}\")
"
}

echo
echo "── 4. 农业术语下的显示 ──"
BEFORE=$(biz_digest)
echo "  $(label)   业务数据指纹 $BEFORE"

echo
echo "── 5. 切换成医学术语包 ──"
"$PLUGIN" terms --project "$PROJECT" --pack medicine --allow-write
AFTER=$(biz_digest)
echo "  $(label)   业务数据指纹 $AFTER"

echo
if [ "$BEFORE" = "$AFTER" ]; then
  echo "✅ 结论：显示名换了、数据一个字节没变（列名也始终是 unit_id / treatment_id）"
else
  echo "❌ 结论：数据被改动了，不该发生"
  exit 1
fi

echo
echo "── 6. 方法文档登记与检索 ──"
printf '# SPAD 测定 SOP（合成示例）\n1. 每小区随机 5 株\n2. 每叶测 3 点取平均\n' > "$WORK/sop.md"
"$PLUGIN" method add --project "$PROJECT" --id M-001 --title "SPAD 测定 SOP" \
  --category SOP --version v1.1 --source "$WORK/sop.md" --allow-write
"$PLUGIN" method list --project "$PROJECT"

echo
echo "产物在 $WORK"
