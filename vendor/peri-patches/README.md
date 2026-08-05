# KeenCode peri vendor patch

`0001-keencode-current.patch` 是 KeenCode 对固定上游源码的完整当前差异，不再拆分或保留历史补丁。

- 上游仓库：`KonghaYao/peri`
- 上游提交：`3dfe1185`（2026-08-03，LLM 层独立为 `peri-model` crate 的结构性重构）
- 目标目录：`vendor/peri/`（8 crate 闭包：`peri-acp` `peri-agent` `peri-middlewares`
  `peri-model` `peri-workflow` `peri-lsp` `peri-acp-types` `langfuse-client`）
- 说明：当前统一补丁包含 KeenCode 对模型适配、ThreadStore replay、
  goal/replay/recovery ACP 等能力的全部修改。

重新生成并验证（`upstream_repo` 指向包含固定提交的 peri 上游仓库）：

```bash
upstream_repo=/absolute/path/to/peri
commit=$(cat vendor/peri/COMMIT)
git -C "$upstream_repo" cat-file -e "$commit^{commit}"
base=$(mktemp -d)
verify=$(mktemp -d)
git -C "$upstream_repo" archive "$commit" | tar -x -C "$base"
repo=$(mktemp -d)
cp -a "$base"/. "$repo"/
git -C "$repo" init
git -C "$repo" config user.email keencode@local
git -C "$repo" config user.name KeenCode
git -C "$repo" add -f -A
git -C "$repo" commit -m base
rsync -a --delete --exclude target --exclude .git --exclude .DS_Store vendor/peri/ "$repo"/
git -C "$repo" add -f -A
git -C "$repo" diff --cached --binary > vendor/peri-patches/0001-keencode-current.patch

git -C "$upstream_repo" archive "$commit" | tar -x -C "$verify"
git -C "$verify" init
git -C "$verify" apply --check "$PWD/vendor/peri-patches/0001-keencode-current.patch"
git -C "$verify" apply "$PWD/vendor/peri-patches/0001-keencode-current.patch"
rsync -rcni --delete --exclude target --exclude .git --exclude .DS_Store vendor/peri/ "$verify"/
rm -rf "$base" "$repo" "$verify"
```

基线与目标两次都使用 `git add -f -A`，确保上游 `.gitignore` 命中的源码删除也登记进补丁。补丁应用到干净上游副本后，用 `rsync -rcni --delete` 比较目标树；统一补丁不记录空目录，可忽略仅包含目录的 `cd+++++++` 与 `*deleting .../` 行，其余无输出时表示文件内容与 `vendor/peri/` 完全一致。
