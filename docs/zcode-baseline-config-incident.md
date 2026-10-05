# ZCode 基线配置越界取证

记录时间：2026-10-03（Asia/Shanghai）。本记录只保留路径、进程、哈希和键名，
不包含个人配置值、账号信息或凭据。

## 已确认事实

- 固定来源为 ZCode SHA `29628c9acdb81b703bbd4080c207a0e7ce5e276e`；来源目录
  `D:/projects/ZCode` 未修改。
- 错误参考宿主写入真实文件
  `C:/Users/chengliang/.zcode/v2/setting.json`：PID `36976` 在本地时间
  `01:46:59`--`01:51:02` 写入，PID `34628` 在 `01:54:31`--`01:58:10` 写入。
- 已停止相关进程 PID `31072`、`34628`、`36976`、`33504`。当前没有参考宿主进程，
  TCP `3030` 没有监听。
- 证据日志：`out/native-live/source-server.stdout.log`、
  `out/native-live/source-server-clean.stdout.log`、
  `out/native-live/source-server-isolated.stdout.log`。

## 精确恢复

- 没有发现可靠的原始 preimage、atomic backup、File History 或有效 sidecar，因而
  没有猜测默认值，也没有整文件覆盖。
- 仅删除已确认由本轮基线引入的路径
  `D:/projects/ZCode-baseline-29628c9`，涉及键名：`recentProjects`、
  `lastWorkspaceSession`。
- `memoryEnabled`、`onboardingOccupation`、`providerFamilyDomainUpdatedAt` 的
  原始值无法从现有证据确定，保持现状，不作恢复猜测。
- 当前文件 SHA-256：
  `9FF419414CDAC49253C33F178D35ECC454B0EE16B1907AA3328EA54379A48230`；上述测试
  路径在两个数组中的出现次数均为 `0`，没有 `setting.json.*` sidecar。

## 隔离基线验证

- 在启动参考宿主前确认独立根为
  `D:/projects/keen-code/out/native-live/zcode-reference-runtime-29628c9`，其预期
  `setting.json` 不在真实用户目录；仅设置 `ZCODE_DESKTOP_HOME_DIR` 与
  `ZCODE_DATA_BASE_DIR`，未改写 `HOME`、`USERPROFILE`、`CODEX_HOME` 或系统配置。
- 隔离宿主 PID `37408` 于本地 `03:25`--`03:36` 运行，所有设置写入独立根；截图完成后已停止。
  当前 TCP `3030` 无监听（仅有已关闭连接的 `TIME_WAIT`）。
- 启动前后真实用户文件 SHA-256 均为
  `9FF419414CDAC49253C33F178D35ECC454B0EE16B1907AA3328EA54379A48230`，未生成
  `setting.json.*` sidecar。DPR2 截图与报告位于
  `out/native-live/zcode-source-baseline-29628c9-dpr2/`；它们是源 UI 视觉参考，
  不作为原生功能验收证据。
