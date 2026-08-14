# KeenCode peri vendor patch

`0001-keencode-current.patch` 是 KeenCode 对固定上游源码的完整当前差异，不再拆分或保留历史补丁。

- 上游仓库：`KonghaYao/peri`
- 上游提交：`ef45872c`（`agent-v3.6.5`，2026-08-12）
- 目标目录：`vendor/peri/`（11 crate 闭包：`peri-acp` `peri-acp-types` `peri-agent`
  `peri-controller` `peri-middlewares` `peri-model` `peri-resources` `peri-runtime`
  `peri-workflow` `peri-lsp` `langfuse-client`）
- 说明：当前统一补丁包含 KeenCode 对模型适配、ThreadStore replay、
  goal/replay/recovery ACP 等能力的全部修改。

重新生成并验证（`upstream_repo` 指向包含固定提交的 peri 上游仓库）：

```bash
upstream_repo=/absolute/path/to/peri
commit=$(cat vendor/peri/COMMIT)
git -C "$upstream_repo" cat-file -e "$commit^{commit}"
base=$(mktemp -d)
canonical=$(mktemp -d)
verify=$(mktemp -d)
git -c core.autocrlf=false -C "$upstream_repo" archive "$commit" | tar -x -C "$base"
git -c core.autocrlf=false archive HEAD:vendor/peri | tar -x -C "$canonical"
repo=$(mktemp -d)
cp -a "$base"/. "$repo"/
git -C "$repo" init
git -C "$repo" config core.autocrlf false
git -C "$repo" config user.email keencode@local
git -C "$repo" config user.name KeenCode
git -C "$repo" add -f -A
git -C "$repo" commit -m base
rsync -a --delete --exclude target --exclude .git --exclude .DS_Store "$canonical"/ "$repo"/
git -C "$repo" add -f -A
git -C "$repo" diff --cached --binary > vendor/peri-patches/0001-keencode-current.patch

git -c core.autocrlf=false -C "$upstream_repo" archive "$commit" | tar -x -C "$verify"
git -C "$verify" init
git -C "$verify" config core.autocrlf false
git -C "$verify" apply --check --whitespace=error-all "$PWD/vendor/peri-patches/0001-keencode-current.patch"
git -C "$verify" apply --whitespace=error-all "$PWD/vendor/peri-patches/0001-keencode-current.patch"
rsync -rcni --delete --exclude target --exclude .git --exclude .DS_Store "$canonical"/ "$verify"/
rm -rf "$base" "$canonical" "$repo" "$verify"
```

基线与目标两次都使用 `git add -f -A`，确保上游 `.gitignore` 命中的源码删除也登记进补丁。目标树从 KeenCode 的 `HEAD:vendor/peri` 导出，并显式禁用 `autocrlf`，避免工作区行尾让补丁随平台漂移；生成前应先提交全部 `vendor/peri/` 改动。补丁应用到干净上游副本后，用 `rsync -rcni --delete` 比较规范目标树；统一补丁不记录空目录，可忽略仅包含目录的 `cd+++++++` 与 `*deleting .../` 行，其余无输出时表示文件内容完全一致。
