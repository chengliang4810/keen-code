# 添加项目面板 Design QA

## 对比目标

- 设计真值：`/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-c1dea8dd-9a2e-4633-9d84-7b8655eacaa9.png`
- 最终实现截图：`/Users/chengliang/Documents/jian-desktop/output/design-qa/add-project-550-after.png`
- 并排对比：`/Users/chengliang/Documents/jian-desktop/output/design-qa/add-project-comparison.png`
- 状态：浅色界面；创建项目面板已打开；名称与源文件夹为空；名称输入框聚焦；创建按钮禁用。

## 视口与密度归一化

- 源图：1099 × 645 px，来自 macOS Retina 截图；按 2 倍密度归一化为 550 × 323 px。
- 实现：550 × 323 CSS px，`deviceScaleFactor = 1`，截图为 550 × 323 px。
- 对比时使用归一化源图 `output/design-qa/reference-550.png`，避免把 Retina 物理像素误判为两倍尺寸的 CSS 面板。

## 可见结果

- 信息层级与参考一致：标题、项目名称、源文件夹、文件夹选择/拖入区域、取消、创建项目。
- 面板、输入框、拖入区域和底部操作区在归一化视口中的位置与比例一致，没有裁切、溢出或控件遮挡。
- 项目名称默认获得焦点，焦点边界可见；创建按钮在源文件夹为空时保持禁用。
- 图标使用项目现有 Tabler 图标；没有用 CSS 图形、文本符号或占位素材代替。
- `Codex` 改为 `KeenCode`，并保留 KeenCode 的玻璃表面、圆角、颜色与按钮令牌，属于明确的产品语义和设计系统差异。

完整面板在 550 × 323 对比图中已能清楚判断字体、间距、颜色、图标和文案，因此不需要额外局部裁切。

## 交互与运行检查

- 侧栏“添加项目”、搜索面板“添加项目”、对话项目菜单“添加项目”均打开同一面板。
- Escape 可关闭面板；每次打开后名称输入框重新获得焦点，关闭后焦点归还侧栏、搜索或对话项目菜单的稳定入口。
- 左侧栏折叠后通过 `Cmd/Ctrl+K` 打开搜索再进入添加面板，关闭后焦点回到可见的对话输入框，不会落到隐藏侧栏。
- 项目可拖到目标行前后排序，也可用 `Alt+↑/↓` 排序；键盘排序结果通过现有状态提示区播报。
- 创建进入忙碌态时，输入框改为只读、提交按钮保留焦点并暴露 `aria-disabled`，焦点不会逃出面板。
- Web/Vite 环境点击源文件夹会显示“需要在 Tauri 窗口中选择目录”，不会误提交；创建按钮仍禁用。
- 控制台 error/warn：0。
- 原生目录选择、Tauri 系统文件夹拖入、项目排序重启持久化未操作原生窗口，属于桌面验收边界。

## 对比历史

1. 初次实现：`output/design-qa/add-project-550-before.png`。发现模态框焦点被关闭按钮抢走，关闭按钮出现不符合参考的焦点圈；空状态还多了一行提示文本。
2. 修正：`GlassModal` 使用显式初始焦点标记聚焦项目名称，并在关闭后把焦点归还入口；输入框增加可见焦点边界，同时移除空状态的重复提示行。
3. 复验：`output/design-qa/add-project-550-after.png`。上述差异已消除；随后补齐键盘排序播报和忙碌态焦点保持，没有剩余 P0、P1、P2 问题。

## 剩余 P3

- KeenCode 现有皮肤比参考图偏粉、标题字重略轻；这是复用当前产品令牌的有意差异，不阻塞交付。

final result: passed

---

# 新建对话欢迎区 Design QA

## 对比目标

- 设计真值：`/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-e8e85a5c-a6b9-41c1-84d4-f7c939f186a2.png` 下半部分的新建对话状态。
- 实现全景：`output/design-qa/new-chat-welcome-after.jpg`。
- 聚焦同屏对比：`output/design-qa/new-chat-welcome-comparison.jpg`。
- 状态：浅色 Dream 皮肤；新建对话；无项目、无可用模型；输入为空；发送按钮禁用。

## 视口与归一化

- 源图：2071 × 1331 px；源图同时包含两个产品，本次只采用下半部分的“欢迎语 + 项目选择 + 输入区”作为结构参考。
- 实现：1280 × 720 CSS px；浏览器报告 `devicePixelRatio = 2`，截图输出已归一化为 1280 × 720 px。
- 聚焦对比将源图 `(180, 700)–(1680, 1331)` 与实现 `(140, 150)–(1140, 510)` 分别等比缩放并留白到 1280 × 600 px，再左右并排；没有把两个产品的整屏外壳误作同一视口。

## Findings

- 没有剩余 P0、P1、P2 问题。新建对话现在使用居中欢迎语，项目选择与 Composer 位于同一外层卡片，层级和垂直节奏与参考一致。
- 字体与文案：欢迎语使用当前产品字体与字重，并采用 KeenCode 自有任务导向文案；参考中的按时段问候不作为功能要求复制。
- 间距与布局：外层卡片实测 864 × 164 px；项目栏 40 px、Composer 850 × 110 px；欢迎语底部到卡片顶部约 50 px。现有对话仍走底部浮动 Composer，不使用欢迎卡片样式。
- 颜色与令牌：边框、表面、阴影和圆角全部复用 `--border-subtle`、`--bg-elevated`、`--shadow-pop` 与 `--radius-composer`，没有新增局部硬编码颜色。
- 图像与图标：本状态没有位图或插画；文件夹、加号、模型、思考程度和发送按钮继续使用项目现有图标组件。
- 文案与内容：项目目录、输入占位、模型和发送状态保持 KeenCode 现有产品语义，没有复制参考产品的权限或模型文案。

## 交互与运行检查

- 点击“项目目录”可打开项目菜单，空项目状态下“添加项目”入口可见。
- 输入内容后欢迎语隐藏；清空输入后欢迎语恢复，避免长草稿与标题争抢空间。
- 浏览器控制台 error/warn：0。
- 29 项聚焦测试、TypeScript 类型检查、CSS 检查和 `git diff --check` 均通过。

## 对比历史

1. 初次实现同时给外层欢迎卡片和内层 Composer 使用浮层阴影，视觉上存在重复层级。
2. 修正后只保留外层卡片阴影，内层 Composer 通过边框区分输入区域。
3. 复验截图与聚焦同屏对比中没有剩余 P0、P1、P2 差异。

## Follow-up Polish

- P3：参考产品的欢迎标题更大、输入区更高；KeenCode 保留已确认的 110 px Composer 高度和当前信息密度，避免本次适配反向改变已有对话输入体验。

final result: passed

---

# Composer 浮层卡片 Design QA

## 对比目标

- 设计真值：`/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-5940a880-af30-41a0-b0a5-c5de73db2cd5.png` 中间产品的输入卡片。
- 实现全景：`output/design-qa/composer-card-after.png`。
- 实现局部：`output/design-qa/composer-card-after-focus.png`。
- 同屏对比：`output/design-qa/composer-reference-vs-after.png`。
- 浏览器视口：`680 × 620` CSS px，截图密度为 1。
- 参考原图：`2317 × 852` px；参考区裁切为 `(330, 270)–(1680, 510)`，归一化为 `675 × 120` px。
- 状态：浅色 Dream 皮肤；新对话；无项目、无可用模型；输入为空；发送按钮禁用。

## 可见结果

- Composer 实测 `648 × 110px`，圆角 `24px`，内边距 `12px`；相较调整前的 `12px` 圆角与弱阴影，窄栏中的卡片比例和悬浮层次已接近参考。
- 加号保留 `32 × 32px` 命中区域，但静止态背景改为透明；发送按钮继续承担右侧主操作，不改变现有功能和图标资产。
- 阴影与边框直接复用 `--shadow-pop`、`--glass-border`，浅色和暗色皮肤均由现有令牌控制，没有新增局部颜色。
- 字体、字号、行高和文案保持项目现有 Composer 体系；参考中的“完全访问”不属于 KeenCode 当前产品权限模型，因此不复制。
- 本组件没有位图、插画或品牌图像；可见图标继续使用项目现有图标库，没有使用 CSS 图形或文本符号代替。

## 交互与运行检查

- “添加”按钮可打开现有 Composer 菜单，Escape 可关闭。
- 浏览器控制台 error/warn：0。
- 30 项聚焦测试、类型检查与 CSS 检查通过。

## 对比历史

1. 调整前：`output/playwright/composer-height-102.png`。输入高度已够用，但 `12px` 圆角、几乎不可见的浅色阴影和常驻灰底加号仍使卡片显得扁平。
2. 修正：新增 Composer 语义圆角令牌；改用现有浮层阴影和玻璃边框；纵向空间增至 `110px`；移除加号静止态底色。
3. 复验：`output/design-qa/composer-reference-vs-after.png`。没有剩余 P0、P1、P2 问题。

## 剩余 P3

- 参考产品使用上箭头发送图标，KeenCode 保留现有纸飞机图标；这是产品图标体系差异，不阻塞本次比例和层次调整。
- 无模型状态下右侧控件按既有禁用语义呈现较低对比度；配置模型后的正常态需由用户在原生桌面环境复核。

final result: passed

---

# 思考程度与 Ultra 面板 Design QA

## 对比目标

- 结构参考：`/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-20f92a4d-bccf-41f2-b06a-7fc40af3bc41.png`
- 实现截图：`output/design-qa/reasoning-ultra-1287x550.png`
- 像素差异：`output/design-qa/reasoning-ultra-reference-diff.png`
- 参考图与实现均为 1287 × 550 px；参考图用于确认“标题/当前值/滑轨”的内部结构，不作为 KeenCode 工作台整体皮肤。

## 可见结果

- 模型选择器之后出现独立思考程度触发器，面板标题、当前值和单值离散 Slider 保持与参考图一致的信息结构。
- Ultra 位于同一面板下半区，使用现有 shadcn/ui Switch；开启后面板保持打开，状态为 `checked`，不会改变 Slider 的值。
- 面板宽度为 320 px，Ultra 说明在当前简体中文下不再孤立换行；菜单未裁切、未遮挡发送按钮。
- Web 验证环境没有 Tauri 供应商目录，因此 Slider 正确显示“不支持”禁用态；支持模型的档位映射由组件测试覆盖。
- 控制台 error/warn：0。

## 差异说明

- 参考图是脱离产品上下文的双值 Temperature 示例；当前需求是模型元数据给出的单个离散思考档位，因此实现使用单 Thumb，不复制 Temperature、双值范围或白底示例页面。
- 实现直接复用 KeenCode 当前菜单表面、文字、Slider 和 Switch 令牌；像素差异图主要反映产品工作台上下文与参考示例页面的有意差异。

final result: passed

---

# Ultra Switch 可见性 Design QA

## 证据

- 缺陷截图：`/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-dcae1c73-fb5e-4f30-8844-cb423a659e9f.png`（558 × 264 px，浅色 Dream 皮肤，Ultra 关闭）。
- 修复后关闭态：`output/design-qa/ultra-switch-visible-off.png`。
- 修复后开启态：`output/design-qa/ultra-switch-visible-on.png`。
- 同屏对比：`output/design-qa/ultra-switch-visibility-comparison.png`。
- 浏览器视口：1287 × 550 CSS px；实现聚焦截图原始输出为 312 × 177 px，对比图移除浏览器裁切留白后统一归一化到 264 px 高。

## 发现与修复

- [P1 已修复] `app.css` 的全局透明 `button` 重置覆盖了 shadcn Switch 的状态背景，导致关闭态轨道和 Thumb 都接近白色；开启态也只有极淡轮廓。
- 公共 Switch 现在使用皮肤语义令牌：关闭态为 `--bg-active` 轨道、`--text-tertiary` 轨道与 Thumb 边界；开启态为实色 `--accent` 轨道和 `--accent-fg` Thumb。
- 修复后的实际渲染值：关闭态轨道 `rgba(196, 61, 85, 0.1)`、边界 `rgba(90, 50, 60, 0.4)`；开启态轨道与边界均为 `rgb(196, 61, 85)`。颜色、位置与形状共同区分状态。

## 必要表面检查

- 字体和文案：未改变；Ultra 标题和说明保持原层级与换行。
- 间距和布局：Switch 尺寸、位置、Thumb 位移及面板尺寸未改变。
- 颜色和令牌：只复用当前皮肤的状态令牌，没有引入原始颜色或面板局部覆盖。
- 图像与图标：本次没有图像或图标资产。
- 交互：关闭和开启均已实际点击；`data-state` 分别为 `unchecked`、`checked`，面板保持打开。
- 控制台 error/warn：0。

## 对比结论

- 缺陷截图中的白底白轨道已消失；关闭态存在可辨识的有色轨道和双边界，开启态为明显实色轨道。
- 没有剩余 P0、P1、P2 问题。

final result: passed

---

# 新建对话单卡片结构 Design QA

## 对比目标

- 用户指出的错误实现：`/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-cbf0f334-82ea-40cd-a40f-f517027530ac.png`。
- 设计真值：`/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-44a7f2eb-b191-4a18-99e3-6f6604f58b2a.png`。
- 修正后实现：`output/design-qa/new-chat-single-card-after.jpg`。
- 聚焦同屏对比：`output/design-qa/new-chat-single-card-comparison.jpg`。

## 视口与归一化

- 设计真值：1526 × 347 px。
- 实现：1280 × 720 CSS px，浏览器报告 `devicePixelRatio = 2`，截图输出为 1280 × 720 px。
- 同屏对比将设计真值缩放到 1280 px 宽；实现裁切 `(190, 285)–(1090, 485)` 后缩放到 1280 px 宽，两侧均留白到 1280 × 360 px，再左右并排。
- 状态：浅色界面；新建对话；未选择项目；无可用模型；输入为空；发送按钮禁用。

## Findings

- [P1 已修复] 原实现同时给组合容器和 Composer 设置边框、白色表面与阴影，形成“卡片套卡片”。修正后组合容器只保留无边框、无阴影的中性顶部背景，唯一悬浮卡片是白色 Composer。
- 字体与文案：保持 KeenCode 当前字号、字重和产品文案；结构参考不要求复制另一产品的品牌、权限或模型文本。
- 间距与布局：顶部项目栏 48 px，Composer 110 px；两者同宽、无外层留白和间隙，形成连续的“顶部上下文 + 下方输入区”结构。
- 颜色与令牌：浅色顶部背景使用 `--surface-tint-light` 与 `--bg-main` 混合；暗色回落为 `--bg-main` 与 `--text-primary` 的语义混合。没有引入主题专属硬编码色。
- 图像与图标：本状态没有位图或插画；继续使用项目现有文件夹、模型、思考程度和发送图标。
- 内容与交互：项目菜单可以正常打开，“添加项目”入口可见；已有对话 Composer 不使用该新建对话组合样式。

## 对比历史

1. 用户截图确认首轮实现存在双层白色卡片和重复阴影。
2. 移除组合容器的边框、阴影和内边距，把悬浮层级还给 Composer；顶部改为中性背景区。
3. 修正后同屏对比中，顶部项目栏与单一输入卡片的层级关系已与设计真值一致；没有剩余 P0、P1、P2 问题。

## 运行检查

- 项目菜单交互正常；控制台 error/warn：0。
- 29 项聚焦测试、TypeScript 类型检查、CSS 检查和 `git diff --check` 均通过。

final result: passed

---

# 新建对话卡片比例协调 Design QA

## 对比目标

- 用户对比截图：`/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-ce5eaf27-c915-4dc3-a49b-5c0fe0fcc06b.png`。
- 独立设计真值：`/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-44a7f2eb-b191-4a18-99e3-6f6604f58b2a.png`。
- 修正后实现：`output/design-qa/new-chat-balanced-after.jpg`。
- 聚焦同屏对比：`output/design-qa/new-chat-balanced-comparison.jpg`。

## 视口与归一化

- 设计真值：1526 × 347 px。
- 实现：1280 × 720 CSS px，浏览器报告 `devicePixelRatio = 2`，截图输出为 1280 × 720 px。
- 同屏对比将设计真值缩放至 1280 px 宽；实现裁切 `(280, 285)–(1000, 475)` 后等比缩放至 1280 px 宽，两侧留白到 1280 × 360 px，再左右并排。
- 状态：浅色界面；新建对话；未选择项目；无可用模型；输入为空；发送按钮禁用。

## Findings

- [P2 已修复] 当前卡片 864 px 的宽度明显大于参考约 672 px 的主体比例，顶部 48 px 高度和双层浮层阴影进一步放大了松散感。
- 修正后新建对话卡片宽度为 672 px，顶部栏为 40 px，Composer 保持用户已确认的 110 px 高度；整体宽高节奏与参考一致。
- 阴影由通用浮层的两段 28/48 px 阴影收敛为单段 10/28 px 阴影，只表达 Composer 高于顶部背景的层级。
- 字体与文案：项目栏调整到 14 px 主文字色；Composer 和产品文案保持 KeenCode 现状。
- 颜色与令牌：继续使用现有主题令牌和语义颜色混合，没有新增硬编码主题色。
- 图像与图标：没有新增图像资产；所有可见图标继续使用项目现有图标组件。
- 响应式：672 px 只作为新建对话的最大宽度；窄面板仍按可用宽度收缩，不改变既有对话内容与底部 Composer 的宽度规则。

## 对比历史

1. 用户确认单卡片结构正确，但同屏对比显示卡片过宽、顶部偏高、阴影偏重。
2. 仅收紧新建对话组合容器和欢迎态阴影，未改动 DOM、菜单或已有对话样式。
3. 修正后同屏对比没有剩余 P0、P1、P2 问题。

## 运行检查

- 项目菜单可正常打开，“添加项目”入口可见；控制台 error/warn：0。
- 29 项聚焦测试、TypeScript 类型检查、CSS 检查和 `git diff --check` 均通过。

final result: passed

---

# 项目选择文案与创建项目输入框 Design QA

## 对比目标

- 项目选择文案参考：`/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-3fa7b128-ca9f-4a0e-8c6b-a221dcc982bd.png`。
- 输入框缺陷参考：`/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-9b86b4fd-4126-4945-923e-522214a10e06.png`。
- 修复后项目选择：`output/design-qa/2026-08-26-select-project.png`。
- 修复后输入框：`output/design-qa/2026-08-26-add-project-focus.png`。
- 输入框同屏对比：`output/design-qa/2026-08-26-add-project-comparison.png`。

## Findings

- [P2 已修复] 无项目时的默认文案由“项目目录”改为动作导向的“选择项目”；英文和繁体中文同步更新。
- [P2 已修复] 项目名称输入框的外扩焦点轮廓会被面板内容边界裁切，形成底部蓝线和零散角标。焦点反馈改为输入框自身边框色，不再改变盒模型或侵入相邻区域。
- 输入框实测高度 42 px，聚焦时 `outline: none`、`box-shadow: none`；与“源文件夹”标签之间保留 16 px 间距。
- 创建项目面板的尺寸、字段顺序、文件夹选择和提交逻辑均未改动。

## 运行检查

- 新建对话显示“选择项目”；项目菜单和“添加项目”入口可正常打开。
- 创建项目面板自动聚焦项目名称输入框，标签与输入框无重叠；控制台 error/warn：0。
- 29 项聚焦测试、TypeScript 类型检查、CSS 检查和 `git diff --check` 均通过。

final result: passed
