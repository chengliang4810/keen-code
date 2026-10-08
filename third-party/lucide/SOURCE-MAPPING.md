# Lucide 图标来源映射

这些 SVG 是应用为原生 GPUI 资源源补齐的 Lucide 图标，不属于 Ely GPUI Components
固定 revision 的内置资源。

| 应用资源 | 来源包 | 来源文件 | 来源 SHA-256 | 应用 SVG SHA-256 | 许可证 |
| --- | --- | --- | --- | --- | --- |
| `apps/desktop/assets/icons/message-circle-plus.svg` | `lucide-react` v1.17.0 | ZCode 固定提交目录下 `node_modules/lucide-react/dist/esm/icons/message-circle-plus.mjs` | `4a52698f7b463a252e62507d9acd02309d25f61fe5174a0c6981a26884fb0c29` | `144644ffc3395113372edebd302f106421b759e4928518a465194b2aa2d4b64a` | ISC |
| `apps/desktop/assets/icons/calendar-clock.svg` | `lucide-react` v1.17.0 | ZCode 固定提交目录下 `node_modules/lucide-react/dist/esm/icons/calendar-clock.mjs` | `2da82f2ba44bbaa02cb060940e781b4fae66a0efe6044b5ce397b51b8d2ae113` | `4b460858f362e2b5918cb3b0b4d68227c1ed23071118af983bd2484d659ea981` | ISC |
| `apps/desktop/assets/icons/blocks.svg` | `lucide-react` v1.17.0 | ZCode 固定提交目录下 `node_modules/lucide-react/dist/esm/icons/blocks.mjs` | `3bcc1c0a05ad9bfaeafdb31df251102be14ab81c2303316d040266b703920af5` | `522fc96b61aec06097f0d8fc87d0a8c32e9c6e38d47b104f19a4601f0c04488c` | ISC |
| `apps/desktop/assets/icons/list-filter.svg` | `lucide-react` v1.17.0 | ZCode 固定提交目录下 `node_modules/lucide-react/dist/esm/icons/list-filter.mjs` | `2dcfb15ddf6908eff2051216511da5f66538c74b544e815adfc565051f39a9da` | `236377e76d2a8ac7d57e0362ecf6f996ec3f55827aca7e9bd202dd998a4eeacf` | ISC |
| `apps/desktop/assets/icons/chevron-down.svg` | `lucide-react` v1.17.0 | ZCode 固定提交目录下 `node_modules/lucide-react/dist/esm/icons/chevron-down.mjs` | `48dfa452ca73896595819d0f6918495ed3c62919c48feabf20ebb00fe9532474` | `3de825566f9eb7cb3d9f0fdc92a7ee949919ed74c1b9976449d65243f18f62c5` | ISC |
| `apps/desktop/assets/icons/square-terminal.svg` | `lucide-react` v1.17.0 | ZCode 固定提交目录下 `node_modules/lucide-react/dist/esm/icons/square-terminal.mjs` | `05fadb8172a866009ef09a1f218cb1f5f48173fee16486757ec840db6afa9964` | `80a89f38149ea4546e3b69240e117fe524b7ea0894e934399597af1fd677511b` | ISC |
| `apps/desktop/assets/icons/panel-right-open.svg` | `lucide-react` v1.17.0 | ZCode 固定提交目录下 `node_modules/lucide-react/dist/esm/icons/panel-right-open.mjs` | `d7ac0e3857ea175b8b662dd9c8d08b2473e4b89901570f44829d324ae8b59422` | `bed9f8752ab437132619d799c02f15c796e79c4ff134fd62cf3f6ecf12645388` | ISC |
| `apps/desktop/assets/icons/settings-2.svg` | `lucide-react` v1.17.0 | ZCode 固定提交目录下 `node_modules/lucide-react/dist/esm/icons/settings-2.mjs` | `9e0f3d0676d1eb3eeaf87f577d4b977d969a0f45ab2ac713fabe6521f78ca402` | `8221f61709dcfa725e0d0c3bb7c2b7a6eeb847bd0223816a962dd3f0ebde63ee` | ISC |
| `apps/desktop/assets/icons/brain.svg` | `lucide-react` v1.17.0 | ZCode 固定提交目录下 `node_modules/lucide-react/dist/esm/icons/brain.mjs` | `e1aba356425a79c9c1b20591c446ec2719b22f5fde5afd34a72cfb2f06776f3c` | `d7b2bcc58470de19cbd4cece8ca6cf1dd249e6c6fbfd1e7bf0ef5e19b60908c4` | ISC |
| `apps/desktop/assets/icons/cable.svg` | `lucide-react` v1.17.0 | ZCode 固定提交目录下 `node_modules/lucide-react/dist/esm/icons/cable.mjs` | `8c9e80a5cd77b4e1a6cf7ee0e0baccf6c2e95fe2c468c42f5369c89b36a0188e` | `2c7890e1d1f5ff06057875a3123454ef3ad4cbe353d39e7f6b995b154686c26c` | ISC |
| `apps/desktop/assets/icons/anchor.svg` | `lucide-react` v1.17.0 | ZCode 固定提交目录下 `node_modules/lucide-react/dist/esm/icons/anchor.mjs` | `86e440e0f267c1bcf33ce91a60c4a0e5f0c59a19280c37c5a20a39d28cc7738a` | `ceb52f77ce1ccc6c0aeb10bdeaee7884904d00c65e306ddf9d92f97f0a5ce8a` | ISC |

设置导航核对使用固定来源 `packages/ui/src/settings/settingsPageConfig.ts`，其来源 SHA-256
为 `1c63280c8abd0ee50fb9c423f890fbd9c05bb31ec29753544b850d286e307575`；`general`、
`memory`、`plugin`、`mcp` 和 `hooks` 分别对应 `Settings2`、`Brain`、`Blocks`、
`Cable` 和 `Anchor`。`blocks.svg` 使用已有应用资源，不重复引入文件。

SVG 保留 Lucide 的 `24x24` viewBox、路径数据和 `currentColor` 描边语义，仅转换为
GPUI `AssetSource` 可加载的静态 SVG。许可证全文见 [`LICENSE`](LICENSE)。
