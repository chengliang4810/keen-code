# 项目按需加载与分层存储

启动只读取全局 projects.json 登记表，不检查登记的工作目录，不枚举会话。所有项目初始折叠；展开时先检查该项目目录，再按 cwd 读取其会话列表。搜索和归档视图需要完整会话列表时按需读取。

目录检查只有 NotFound 才删除项目登记并提示“项目目录已被删除”。权限错误、路径不是目录等情况保留登记并显示错误。删除登记不删除历史文件。

## 当前存储结构

```text
数据根/
  projects.json                     # 左侧项目登记表
  project-locations/<路径哈希>.json  # 工作目录到稳定项目 ID
  session-locations/<会话ID>.json    # 会话 ID 到项目 ID
  projects/<项目ID>/
    project.json                    # 单个项目描述
    <会话ID>/                       # 日志、派生索引、产物、Goal、Plan 等
      ...
    session-mutations/              # 项目内会话变更事务
```

项目重定位保留 ID。打开指定会话通过定位文件找到所属项目，不扫描全部工作目录或全部会话。定位文件和列表索引都是文件，未引入 SQLite。设置、供应商配置、全局记忆与尚未归属会话的附件导入暂存保留在全局；会话的后台任务、工具输出和 worktree 记录归入会话目录。

## 开发数据与验证

本次仅对 ~/.keencode-dev 做一次性离线整理，未添加产品兼容或迁移代码，未修改正式 ~/.keencode。备份位于 `~/.keencode-dev-before-project-storage-20260911-234208`。23 个会话分入 2 个项目数据目录，全部会话日志与备份逐字节一致，原项目登记不变。

- 前端 typecheck 通过；完整测试 140 个文件、1306 项通过。
- resources 单元与集成测试通过；runtime 最终测试 107 项通过。
- 桌面 Rust 测试 524 项通过、7 项忽略（test-threads=4）。
- 浏览器真实 hook/ProjectTree 场景覆盖启动、展开、目录缺失和权限错误；调用记录证明只有正常展开才请求对应项目会话。
- 相同折叠状态截图比较为 0 个变化像素，详见 design-qa.md；未完成原生 WebView 截图验收，未测量真实冷启动耗时。
- 开发版通过 corepack pnpm@10.14.0 dev:desktop 启动，已确认 target/debug/keencode-desktop 进程运行。

开发版运行后复核：23 份历史事件日志均完整保留为当前日志前缀；其中一个会话已在新目录追加 title_generated / session_renamed 两个事件，现有数据写入路径已生效。

浏览器基线：Git 0a8654ae7765736b6e0d56a9289895dfe5e0ab95 的 ProjectTree 保存为 output/playwright/project-storage-20260911/before-tree.tsx。macOS、Chromium、1000×800、DPR 1，before.png / after.png 比较无像素变化。运行该目录 vite.config.ts，端口 1435，基线访问 /?baseline，当前访问 /。final-state.txt 保存接口调用和权限失败保留项目的状态证据。
