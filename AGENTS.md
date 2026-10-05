# KeenCode Agent 开发指南

本文是仓库级开发约束。用中文交流，先从当前源码、配置、测试和 Git 状态
确认事实；保留与任务无关的本地修改，不重置、覆盖或提交其他 Agent 的工作。

## 当前基线

- 前端 UI 以 ZCode 3.14.3 固定提交
  `29628c9acdb81b703bbd4080c207a0e7ce5e276e` 为来源基线，映射与许可证见
  `THIRD_PARTY_NOTICES.md`、`third-party/zcode/` 和 `docs/frontend-zcode-source.md`。
- 产品 UI 根为 `packages/ui/src/`；`apps/ui/` 承载 Tauri 平台适配、构建入口
  和契约测试，不存放第二套业务界面。React 19、Vite、Tailwind v4 和 pnpm workspace 的实际版本
  以 `package.json`、各包清单和 lockfile 为准。
- Rust workspace 由根 `Cargo.toml` 管理。Tauri 桌面宿主、ACP、Agent Loop、
  Journal 和资源持久化仍由 Rust 持有权威状态；前端是可丢弃的界面投影。
- 构建可使用 Node.js 24 与 pnpm 10.14.0；桌面运行时不得依赖 Node、Electron
  或 JavaScript workflow engine。

## 开始与编辑

1. 先执行 `git status --short`，再用 `rg`/`rg --files` 定位目标文件、调用方和
   相邻测试。不要通读无关的大文件或生成输出。
2. 修改 UI 前阅读 `DESIGN.md` 与 `docs/frontend-zcode-source.md`；修改 RPC、
   snapshot、Journal 或 workflow 前阅读 `docs/protocols/` 下对应契约。
3. 保持来源 ZCode 的 DOM、className、CSS token、键盘语义和 locale 结构。只
   在已确认的产品裁剪、品牌文案和 KeenCode RPC 接线处修改。
4. 新增或修改的非直观语义、约束和取舍使用简短中文注释；JSON 等格式不写非法
   注释。优先已有依赖和组件，不为门禁引入伪造包装层。
5. 只有用户明确要求时才提交或推送；提交信息使用中文为主的中英双语。当前
   迁移阶段不要创建提交。

## UI 规则

- 使用 `packages/ui/src/styles.css` 的 `--color-*` 语义令牌和原有主题角色。
  业务 TSX/CSS 不写主题色字面量、固定视觉 inline style 或第二套字体缩放。
- 应用界面文字必须使用 `text-ui-xl`、`text-ui-lg`、`text-ui-base`、
  `text-ui-caption`、`text-ui-sm`、`text-ui-xs`。代码、Diff、终端内容可有
  独立数字字体设置，但外围控件仍使用 `text-ui-*`。
- 通用控件优先复用 `packages/ui/src/components/ui/`；locale 以 `en-US` 键形状
  和 `zh-CN` 默认中文为准。协议值、ID、路径、模型名和用户内容不翻译。
- 官方账号、支付、云端、机器人、SSH、WSL、Docker、浏览器计算机控制及其
  路由、菜单、locale 和依赖都不属于目标产品。
- 业务代码不能通过“忽略整个第三方目录”规避设计或来源门禁；固定 ZCode 源码的
  原始例外必须命中 `third-party/zcode/design-baseline.json` 的完整 SHA256 或精确
  行特征。合法来源还必须在组件级清单中注明路径、版本和许可证。

## 协议与状态

- `ChannelClient` 继续使用 VQL 100-204 的请求、取消、订阅、释放和响应语义。
  `open`、`send`、`close` 必须有明确的生命周期和幂等释放行为。
- v4 wire protocol version 为 3，projection snapshot version 为 1。每个窗口
  使用 window-bound connection；seq 缺口、重复或 connection reset 触发 resync，
  不用 optimistic overlay 冒充已确认状态。
- Journal 是会话、消息、工具和 workflow 执行事实源；snapshot 只恢复投影。
  active barrier 由 Rust host 建立和解除，前端不得凭按钮状态宣布完成。
- `WorkflowDefinitionV1` 是 Rust `serde` 负责的纯 JSON；actor 只允许单层，
  ID/版本/引用/环由 Rust 校验。UI 草稿变化不能改变 active revision。

## 验证

从仓库根按改动范围运行：

```text
corepack pnpm@10.14.0 install --frozen-lockfile
pnpm run typecheck
pnpm test
pnpm exec vitest run --config vitest.config.ts <test-file>
pnpm run lint:css
pnpm run check:design-system
pnpm run check:clean-room
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
# native-desktop-tests 只用于原生验收程序构建；普通离线单测不需要该 feature
cargo build -p keencode-desktop --features native-desktop-tests
# 原生 WebView2 验收仅在有显式 plan/provider 配置时运行；不是离线 CI 步骤
node tooling/scripts/native-live-e2e.mjs --plan <plan.json> --provider-config <provider.json>
```

前端浏览器开发服务器只能验证打包和静态交互，不能替代 Tauri 原生窗口。真实
provider/live 测试保持 opt-in，不纳入离线 CI。每个通过项必须在
`docs/frontend-acceptance-matrix.md` 记录命令、环境和截图/日志证据；没有证据的
项目保持 `pending`。

完成前检查 `git diff` 与 `git diff --check`，报告实际验证结果和未验证范围。
