#!/usr/bin/env bash
# 插件边界（第 182 轮）：面板仓里不许有插件产物，两条部署路径不许混。
#
# 规则见 docs/plugin-contract.md。这里只查「机器能查的」：
#   ① 面板仓 plugins/ 只有白名单里的东西（磁盘 + git 索引都查）
#   ② plugins/ 下没有构建产物 / 工程文件（dist、node_modules、package.json…）
#   ③ 部署脚本不搬插件（deploy-daily.sh 里不许出现同步 plugins/ 的命令）
#   ④ 契约文档在、索引里有它、plugins/README.md 指向它
#   ⑤ .gitignore 挡住会话产物目录（.zcode/、plugins-wip/）
#
# 用法：bash scripts/check-plugin-boundary.sh
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

FAIL=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL+1)); }

ALLOW='README.md hello.js.bak webui-demo.js webui-demo.webui'

# ① 白名单：磁盘上的 plugins/ 顶层条目
stray=()
for p in plugins/* ; do
  [ -e "$p" ] || continue
  name=$(basename "$p")
  case " $ALLOW " in
    *" $name "*) ;;
    *) stray+=("$name") ;;
  esac
done
if [ ${#stray[@]} -eq 0 ]; then
  ok "plugins/ 只有面板自己的东西（$ALLOW）"
else
  bad "plugins/ 里混进了不属于面板仓的东西：$(printf '%s ' "${stray[@]}")"
fi

# ①b 白名单：git 索引里跟踪的 plugins/ 文件（防「删了文件没删索引」）
tracked=($(git ls-files plugins | sed 's|^plugins/||' | cut -d/ -f1 | sort -u))
tstray=()
for name in "${tracked[@]}"; do
  [ -n "$name" ] || continue
  case " $ALLOW " in
    *" $name "*) ;;
    *) tstray+=("$name") ;;
  esac
done
if [ ${#tstray[@]} -eq 0 ]; then
  ok "git 索引里的 plugins/ 也只有那些"
else
  bad "git 索引里还有：$(printf '%s ' "${tstray[@]}")"
fi

# ② 构建产物 / 工程文件
junk=$(cd plugins && ls -d node_modules dist package.json tsconfig.json package-lock.json 2>/dev/null; find . -maxdepth 2 -name "*.ts" -print 2>/dev/null)
if [ -z "${junk:-}" ]; then
  ok "plugins/ 下没有构建产物 / 工程文件（dist、node_modules、*.ts…）"
else
  bad "plugins/ 下有构建痕迹：$(echo "$junk" | tr '\\n' ' ')"
fi


# ③ 部署脚本不许搬插件
if grep -qE 'cp -R? .*ROOT./plugins|rsync .*plugins|git archive .*plugins' scripts/deploy-daily.sh; then
  bad "deploy-daily.sh 还在搬 plugins/（部署路径混了，见 R2/R3）"
else
  ok "deploy-daily.sh 不搬 plugins/（面板与插件分开部署）"
fi

# ④ 契约文档 + 索引
if [ -f docs/plugin-contract.md ]; then
  ok "docs/plugin-contract.md 在"
else
  bad "docs/plugin-contract.md 没了（契约是这个文件，别删）"
fi
if grep -q 'plugin-contract.md' docs/README.md; then
  ok "docs/README.md 指向契约文档"
else
  bad "docs/README.md 没有指向 plugin-contract.md"
fi
if grep -q 'plugin-contract.md' plugins/README.md; then
  ok "plugins/README.md 指向契约文档"
else
  bad "plugins/README.md 没有指向 plugin-contract.md"
fi

# ⑤ .gitignore 挡会话产物
for pat in '.zcode/' 'plugins-wip/'; do
  if grep -qxF "$pat" .gitignore 2>/dev/null; then
    ok ".gitignore 挡住 $pat"
  else
    bad ".gitignore 缺 $pat"
  fi
done

echo ''
if [ "$FAIL" = "0" ]; then echo "ok: 插件边界的 9 条检查全过"; else echo "红: $FAIL 条没过"; fi
exit "$FAIL"
