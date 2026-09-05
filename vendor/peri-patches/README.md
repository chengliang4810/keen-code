# KeenCode peri vendor patch

`0001-keencode-current.patch` 是 KeenCode 对固定上游源码的完整当前差异，不再拆分或保留历史补丁。

- 上游仓库：`KonghaYao/peri`
- 上游提交：`ef45872c`（`agent-v3.6.5`，2026-08-12）
- 目标目录：`vendor/peri/`（9 crate 闭包：`peri-acp` `peri-acp-types` `peri-agent`
  `peri-controller` `peri-middlewares` `peri-model` `peri-resources` `peri-runtime`
  `peri-lsp`）
- 说明：当前统一补丁包含 KeenCode 对模型适配、ThreadStore replay、
  goal/replay/recovery ACP、计划模式子代理沙箱（`WriteSandboxTool` 外部基目录
  `PERI_SANDBOX_WRITE_BASE`，方案/报告写入应用数据目录而非项目内 `.peri/`）
  等能力的全部修改。

重新生成并验证（`upstream_repo` 指向包含固定提交的 peri 上游仓库）：

```bash
upstream_repo=/absolute/path/to/peri
root=$(pwd)
commit=$(cat vendor/peri/COMMIT)
git -C "$upstream_repo" cat-file -e "$commit^{commit}"
repo=$(mktemp -d)
verify=$(mktemp -d)
git -C "$repo" init
git -C "$repo" config core.autocrlf false
git -C "$repo" fetch --no-tags "$upstream_repo" "$commit"
git -C "$repo" update-ref refs/heads/upstream FETCH_HEAD
git -C "$repo" fetch --no-tags "$root" HEAD
git -C "$repo" update-ref refs/heads/keencode FETCH_HEAD
base=$(git -C "$repo" rev-parse 'refs/heads/upstream^{tree}')
target=$(git -C "$repo" rev-parse 'refs/heads/keencode:vendor/peri')
git -C "$repo" diff --binary --full-index --no-renames \
  "$base" "$target" --output="$root/vendor/peri-patches/0001-keencode-current.patch"

git -C "$verify" init
git -C "$verify" config core.autocrlf false
git -C "$verify" fetch --no-tags "$upstream_repo" "$commit"
git -C "$verify" checkout --detach --force FETCH_HEAD
git -C "$verify" apply --check --index --whitespace=error-all \
  "$root/vendor/peri-patches/0001-keencode-current.patch"
git -C "$verify" apply --index --whitespace=error-all \
  "$root/vendor/peri-patches/0001-keencode-current.patch"
test "$(git -C "$verify" write-tree)" = "$target"
rm -rf "$repo" "$verify"
```

补丁直接比较固定上游提交树与 KeenCode 的 `HEAD:vendor/peri` 树；这样可以保留 Gitlink、可执行位和 `.gitignore` 命中路径，不受归档解压或 Windows 文件系统模式影响。生成前必须先提交全部 `vendor/peri/` 改动。验证仓库从真实上游提交检出，以 `--index` 严格检查工作树与索引，并要求应用后的树 ID 与目标树完全一致。
