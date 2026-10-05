# Tao Windows 输入重入修复

- 上游：<https://github.com/tauri-apps/tao>，Apache-2.0；保留的许可证见 `LICENSE`。
- 当前 Tauri 2.11 使用 `tao ^0.35`，registry 0.35.3 尚未包含修复。
- 根 `Cargo.toml` 的 patch 固定官方提交
  `c704261c519c58cfdd0bc2d58ba24e06a0b71c92`，对应
  [上游 PR 1215](https://github.com/tauri-apps/tao/pull/1215)。
  Cargo 直接读取固定 Git 源码，本目录不复制或修改窗口实现。
- 提交的 crate 版本仍为 0.35.3；`tao-macros` 同源固定为 0.1.3。
  不跟随分支，不升级 Tauri、Wry 或既有 crate 版本。
- 相对 `tao-v0.35.3`，实现差异为四个 Windows 输入文件和一个 Linux JIS 键映射修正；
  另有两份上游 changefile。Windows 修复将可能重入的 `PeekMessageW` 移到输入锁之前。
  本次只验证 Windows；Linux JIS 改动及其他平台运行未验证。
- 宿主卡死时的重复主线程栈保存于
  `out/native-live/manual-window-unwind.json`，显示持有键盘锁期间 `PeekMessageW`
  重入同一窗口回调并再次等待该锁。没有目录通知／线程回收帧。
- 后续升级至已包含修复的 Tauri／Tao 稳定版本时，先复验 Windows 键盘、IME、焦点、
  多窗口与原生浏览器，再移除 patch；不能仅因 registry 出现新版本就删去固定源。

本次没有修改上游仓库，也没有维护私人 fork。构建需取得该固定 Git 源，与其他 Cargo
依赖一样可使用本地缓存；Tao 在产品运行时不会拉取 Git 源码，也不需要 Node。
