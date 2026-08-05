# KeenCode 发布说明

## 自动发布

推送到 `main` 后，`.github/workflows/release.yml` 会：

1. 运行类型检查、测试和前端生产构建。
2. 根据提交时间（中国标准时间）与提交短哈希生成对外标签。
3. 为 macOS Apple Silicon、macOS Intel 和 Windows x64 原生构建安装包。
4. 生成并上传签名更新包与 `latest.json`。
5. 校验更新清单包含三个平台，并写入用户可见的日期 Release 标签。
6. 仅在所有平台成功后公开 Release，并标记为 Latest。

同一个提交重复运行工作流时复用同一个标签和安装包内部版本，不会生成第二个版本号。

## 版本规则

- 对外标签：`vYYYYMMDD-abcdef0`
- 安装包内部版本：由 `main` 第一父提交序号编码成三段数字，例如 `1.0.2`

对外标签继续使用中国标准时间下的提交日期。内部版本只用于原生安装器和更新比较，同一提交重复运行时保持不变，并满足 Windows MSI 对主版本、次版本和修订号的数值限制。不要手工修改发布构建的版本；唯一来源是 `scripts/release-version.mjs`。

## 更新签名

应用内更新必须通过发布签名验证，该校验不可关闭。仓库需要以下 Actions Secrets：

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

公钥已经提交到 `src-tauri/tauri.conf.json`。私钥不得提交、粘贴到 Issue、日志或 Release；丢失私钥后，已经安装的版本将无法信任使用新密钥签出的更新。

维护者可将私钥备份在本机用户配置目录，例如：

```text
~/.config/keencode-release/keen-code-updater.key
```

该文件当前权限应为 `600`。正式扩大分发前，应再把私钥离线备份到受控的密码管理或加密介质中。

## 操作系统代码签名

应用内更新签名只验证更新来源，不能代替操作系统代码签名。

当前自动构建不包含 Apple Developer ID 或 Windows Authenticode 证书。安装包仍可生成并通过应用更新私钥验签，但操作系统可能向用户显示未知发布者或来源提示。接入平台证书时，应在发布工作流中显式启用对应的签名与公证步骤。

## 发布后检查

1. Release 标签符合 `vYYYYMMDD-短哈希`。
2. Release 同时包含两个 macOS 架构和 Windows x64 安装包。
3. Release 包含 `latest.json`、更新包及其 `.sig` 文件。
4. `latest.json` 的三个目标均指向当前 Release 资产，且 `release` 字段等于当前标签。
5. 在上一版本的「设置 → 关于」中检查更新，完成下载、签名校验、安装和重启。
6. 重启后界面显示新的对外 Release 标签。

客户端启动后会立即静默检查更新，随后每 30 分钟复查一次；没有新版本时不显示侧栏更新入口。
