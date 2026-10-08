# mygo 原生事件与终端修复

- 来源：`github.com/egoist/mygo`，MIT；原版权和许可见 `LICENSE.mygo`。
- 基线：`1ff0c41aeea303df5f948dfdfc4e6bbeb6a46df0`。
- 范围：`internal/darwin/surface.go` 的 resize 父类调用与输入法组合态按键分派，以及 `plugins/terminal/internal/pty/pty_darwin.go` 的 Darwin PTY master 打开方式；没有复制框架到 KeenCode。

`mygo-native-input-and-resize.patch` 修复：

1. AppKit/KVO 派生类下，动态 `SendSuper` 会再次调用本重写方法；改为带 NSSize 类型的 `objc_msgSendSuper`，从定义类的父类 NSView 开始分派。
2. 存在 marked text 时，回车、方向和删除键应归输入法；先投给编辑器会额外换行或破坏组合态。Command/Control 快捷键继续正常分派。

`mygo-darwin-pty.patch` 修复：`os.OpenFile` 在尚未 grant 的 PTY master 上初始化 kqueue，会出现 `EAGAIN` 读取错误；终端 reader 结束并关闭 master，导致 shell 收到 SIGHUP。改用 `syscall.Open` 后接 `os.NewFile`，保留原始阻塞 fd 模式。实际 shell 命令执行、关闭面板释放 shell，以及 terminal/PTY 包测试均已通过，见 `../workbench-2026-10-06.md`。此前全仓终端失败记录是修复前状态，不代表本次验收结果；本次未重跑 mygo 全仓测试。

当前本机 mygo 工作树已应用，尚未提交到该依赖仓库。换机器先取得上述基线，再执行：

```sh
git -C /path/to/mygo apply --check /path/to/jian-desktop/docs/go-migration/patches/mygo-native-input-and-resize.patch
git -C /path/to/mygo apply /path/to/jian-desktop/docs/go-migration/patches/mygo-native-input-and-resize.patch
git -C /path/to/mygo apply --check /path/to/jian-desktop/docs/go-migration/patches/mygo-darwin-pty.patch
git -C /path/to/mygo apply /path/to/jian-desktop/docs/go-migration/patches/mygo-darwin-pty.patch
go mod edit -replace github.com/egoist/mygo=/path/to/mygo
# 在 mygo 目录执行：
cd /path/to/mygo
go test ./plugins/terminal ./plugins/terminal/internal/pty -count=1
```

若已应用，检查反向补丁即可确认，不重复执行。后续应使用包含修复的可下载依赖版本，不能仅靠本机文件宣布发布闭环。

验收：macOS Sonoma Intel 实际窗口缩放和搜狗拼音提交，见 `../acceptance-2026-10-06.md`；本次三栏和终端见 `../workbench-2026-10-06.md`。Windows 路径不受这些 Darwin 补丁影响，但尚未原生验收。
