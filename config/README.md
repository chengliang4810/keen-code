# 辅助工具配置

这里集中管理代码分析、构建体积检查和 Nix 安装包分发配置。

| 文件 | 用途 | 项目根目录下的入口 |
| --- | --- | --- |
| `knip.json` | 检查前端未使用的文件、依赖和导出，属于 Node 工具链 | `pnpm knip` |
| `size-limit.json` | 定义前端 JS 和终端 WASM 的 gzip 体积上限 | `pnpm size` |
| `size-limit.mjs` | 根据构建依赖图计算实际首屏文件，加载体积上限 | `pnpm size` |
| `nix/flake.nix` | 声明 Nix 软件包以及 NixOS、nix-darwin 安装模块 | `nix build path:./config/nix` |
| `nix/flake.lock` | 锁定 Nix 输入依赖的版本和内容哈希 | 由 Nix 管理 |
| `nix/package.nix` | 解包并安装已发布的 RCode 安装包 | 由 flake 调用 |
| `nix/sources.json` | 保存经过验证的发布版本、下载地址和哈希 | 发布时更新 |

Knip 的文件模式相对于项目根目录。Size Limit 的文件模式相对于配置文件目录，所以构建产物路径使用 `../apps/desktop/dist/`。运行体积检查前需要先运行 `pnpm build`。

Nix 配置用于分发已有安装包，没有定义开发环境，也不从源码构建。目前 `nix/sources.json` 的 `artifacts` 为空，配置真实发布产物之前，Nix 构建会明确报错。远程引用该 flake 时需要指定 `dir=config/nix`。

根目录保留 `package.json`、`pnpm-lock.yaml`、`pnpm-workspace.yaml`、`Cargo.toml`、`Cargo.lock` 和 `biome.json`，统一管理工作区和检查入口。Desktop 的 `tsconfig*.json`、`vite.config.ts` 和 `components.json` 位于 `apps/desktop/`，前端源码位于 `apps/desktop/ui/`。Git、Agent 指令、README 和许可证保留各自的标准入口。
