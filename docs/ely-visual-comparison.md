# Ely 原生与 ZCode 视觉比较证据

## 最新视觉状态：release42（2026-10-06）

release42 binary SHA-256 为
`aab17911004dfc2a2204d2f54bb92a61318682fb34d309196c143c1af4f079ee`。两个原生取证报告
记录 settings/workspace 的 24/13 actions 均通过，被测进程自然退出且 `exit_code=0`。
三份比较均使用原始 client 图像 `2560x1640`、DPI `192`、client 偏移 `x=13,y=0`，
不缩放、不配准、不掩码；metrics 的 `acceptance` 仍为 `pending`、`assessment` 为
`not_identical`。

| 页面 | 比较报告 | source | native client | 不同像素 / 总像素 | 差异比例 | 判断 |
| --- | --- | --- | --- | ---: | ---: | --- |
| General | `out/ely-native/visual-settings-42/general-comparison/comparison-metrics.json` | `out/zcode-reference-3.14.3/windows-settings-general-dark-reference2.png` | `out/ely-native/visual-settings-42/native-run/screenshots/0015-settings-general.bmp`（client crop） | `722742 / 4198400` | `17.2147008%` | pending |
| Appearance | `out/ely-native/visual-settings-42/appearance-comparison/comparison-metrics.json` | `out/zcode-reference-3.14.3/windows-settings-appearance-dark-reference2.png` | `out/ely-native/visual-settings-42/native-run/screenshots/0021-settings-appearance.bmp`（client crop） | `117388 / 4198400` | `2.7960175%` | pending |
| Workspace / Draft reference8 | `out/ely-native/visual-workspace-42/draft-reference8-comparison/comparison-metrics.json` | `out/zcode-reference-3.14.3/windows-draft-dark-reference8.png` | `out/ely-native/visual-workspace-42/native-run/screenshots/0005-startup.bmp`（client crop） | `233206 / 4198400` | `5.5546399%` | pending |

相对 release39，Appearance 减少 `7442` 个不同像素，Draft 减少 `4834` 个；General
数值不变。这些数值只是原图比较误差，不能改写为像素或产品验收通过。

### Appearance 原图局部测量（release42）

以下测量直接来自
`out/ely-native/visual-settings-42/appearance-comparison/source.png` 与
`native-client.png`，均为 `2560x1640` client 原图。

- 两张卡片的直线边界仍对齐：界面设置卡约为
  `x=697..2360,y=370..674`，代码设置卡约为 `x=697..2360,y=867..1625`。代码设置卡
  底边之后的 `y=1626..1629` 在 source/native 中均为页面底色 `(22,22,22)`，此前记录的
  native 多出 4px 底色带在 release42 原图中不再出现；两图随后在 `y=1630..1631` 和
  `y=1632` 以下的底部绘制也一致。没有证据支持继续调整卡片底边或裁切。
- 三个 Select 的 Chevron x 范围均为 `2287..2302`，外框位置没有水平漂移。source 的
  可见笔画为 `y=443..450`，主色为 `(138,138,138)`（`#8a8a8a`，另有 2 个抗锯齿像素）；
  native 为 `y=442..451`，主色为 `(144,144,144)`（`#909090`，并含 `(120,120,120)` 等
  抗锯齿像素）。native 的可见笔画高 10px、62 个阈值亮像素，source 高 8px、30 个，
  说明剩余差异是 Chevron 的笔画高度、颜色和栅格化，不是 Select 外框尺寸。

Appearance 的主体卡片底边已对齐，但 Chevron 仍是明确的绘制差异；侧栏图标、中文字体
基线和抗锯齿差异仍计入全图 metrics。

### Draft Composer 局部测量（release42）

以下测量直接来自
`out/ely-native/visual-workspace-42/draft-reference8-comparison/source.png` 与
`native-client.png`，均为 `2560x1640` client 原图；native UIA 证据为
`out/ely-native/visual-workspace-42/native-run/accessibility/0012-empty-session.json`。

- UIA 屏幕坐标扣除 client 原点 `x=768` 后，模型按钮为 client `x=1931..2111`，发送按钮为
  `x=2119..2175`，间隔为 `8` physical px，即 `4` logical px。发送按钮亮像素 bbox 在
  两图中均为 `x=2121..2172,y=1086..1137`，右侧控制簇的水平内边距、内容间距和按钮
  间距保持一致。
- 模型文字的亮像素 bbox：source 为 `x=1950..2059,y=1099..1124`，native 为
  `x=1950..2059,y=1100..1126`；Chevron source 为 `x=2075..2090,y=1108..1115`，
  native 为 `x=2075..2090,y=1107..1116`。水平槽位相同，剩余差异属于字体基线和箭头
  笔画栅格化，不能通过再次调整 horizontal padding 或 gap 修复。
- 按 `#161616` 页面底色之外的像素计，Composer 阴影在 `y=1166` 的范围 source 为
  `x=861..2196`、native 为 `x=867..2190`；source 最后可见行是 `y=1200`、约
  `x=923..2134`，native 最后可见行是 `y=1199`、约 `x=919..2138`。release42 已不再
  出现此前 native 延伸到 `y=1203` 的尾部，但顶部收缩和底部圆角轮廓仍与 source 不同，
  阴影 helper 仍需单独校准。

全图还包含状态和内容差异：native 截图显示中午问候、已打开“原生验收”项目、仅“配置”
入口和 KeenCode 文案；固定 source 显示上午问候、未打开项目的“选择项目”、包含“升级/配置/关闭”
入口和 ZCode 文案。建议项中 native 为“代码审查”，source 为“PPT 制作”。这些品牌、项目状态、
动态文案和裁剪入口均计入全图 metrics，不能当作布局回归或 PASS。

General 的 `17.2147008%` 与 release39 完全相同，主要差异仍来自页面范围和内容布局：
native 是 KeenCode 精简页面，固定 source 含 ZCode 的语言/界面模式等上方设置卡；缺失这些
卡片会使后续 source/native 卡片整体纵向错位，另有字段和文案差异。不能把 General 全部
差异归因于字体抗锯齿，也不应为对齐而伪造未实现的语言/模式控件。

## 最新视觉状态：release39（2026-10-06）

release39 binary SHA-256 为
`f5b9b9bbb8eeb750d650b23a9d47d52036f873e72eb029f57f2ecf897c3963d2`。来源清单
`out/ely-source-inputs-39.json` 和视觉批次日志 `out/ely-native-batch-visual-39.log`
记录 settings/workspace 的 24/13 actions 均通过，被测进程自然退出、`exit_code=0`。
三份比较均保持原始 client 尺寸 `2560x1640`、DPI `192`、client 偏移 `x=13,y=0`，
不缩放、不配准、不掩码；metrics 的 `acceptance` 仍为 `pending`、`assessment` 为
`not_identical`。

| 页面 | 比较报告 | source | native client | 不同像素 / 总像素 | 差异比例 | 判断 |
| --- | --- | --- | --- | ---: | ---: | --- |
| General | `out/ely-native/visual-settings-39/general-comparison/comparison-metrics.json` | `out/zcode-reference-3.14.3/windows-settings-general-dark-reference2.png` | `out/ely-native/visual-settings-39/native-run/screenshots/0015-settings-general.bmp`（client crop） | `722742 / 4198400` | `17.2147008%` | pending |
| Appearance | `out/ely-native/visual-settings-39/appearance-comparison/comparison-metrics.json` | `out/zcode-reference-3.14.3/windows-settings-appearance-dark-reference2.png` | `out/ely-native/visual-settings-39/native-run/screenshots/0021-settings-appearance.bmp`（client crop） | `124830 / 4198400` | `2.9732755%` | pending |
| Workspace / Draft reference8 | `out/ely-native/visual-workspace-39/draft-reference8-comparison/comparison-metrics.json` | `out/zcode-reference-3.14.3/windows-draft-dark-reference8.png` | `out/ely-native/visual-workspace-39/native-run/screenshots/0005-startup.bmp`（client crop） | `238040 / 4198400` | `5.6697790%` | pending |

相对 release38，General 和 Appearance 分别减少 `30468` 与 `30438` 个不同像素，Draft
减少 `1023` 个；这些数值只表示比较误差变化，不能改写为像素或产品验收通过。release39
之后尚无 release40 视觉证据，不能据此推断 release40 状态。

### Draft Composer 局部测量（release39）

以下测量直接来自
`out/ely-native/visual-workspace-39/draft-reference8-comparison/source.png` 与
`native-client.png`，均为 `2560x1640` client 原图。比较脚本生成的 side-by-side 图按
native 在左、固定 source 在右排列；native UIA 坐标来自
`out/ely-native/visual-workspace-39/native-run/accessibility/0012-empty-session.json`，
其 client 原点为 `x=768`。

- 模型按钮四个文字亮像素槽的 x 范围在 source/native 中均为
  `1950..1975`、`1978..2003`、`2006..2031`、`2034..2059`；Chevron 的 x 范围均为
  `2075..2090`。native UIA 的模型按钮 client 框为 `x=1931..2111`，发送按钮为
  `x=2119..2175`，两者间隔为 `8` physical px，即来源的 `4` logical px。
- 发送按钮填充亮像素 bbox 在两图中均为 `x=2121..2172,y=1086..1137`，右侧控制簇的
  水平内边距、内容间距和按钮间距已对齐。剩余差异集中在绘制：模型文字 native bbox 为
  `y=1100..1126`，source 为 `y=1099..1124`；Chevron native 为 `y=1107..1116`，
  source 为 `y=1108..1115`。这属于字体基线/栅格化和箭头笔画差异，不能通过再次调整
  水平 gap 修复。
- Composer 底部阴影仍有明确几何差异：source 首个阴影行 `y=1166` 约为
  `x=861..2196`，最后可见行 `y=1200` 约为 `x=923..2134`；native 首个阴影行同为
  `y=1166` 但约为 `x=879..2178`，最后可见行延伸到 `y=1203`、约为 `x=976..2081`。
  负 spread 收缩圆角的 helper 仍需单独校准。

Draft 全图还包含不可归因于 toolbar geometry 的状态和内容差异：native 截图显示中午
问候、已打开的“原生验收”项目、仅“配置”入口和 KeenCode 文案；固定 source 显示上午
问候、未打开项目的“选择项目”、包含“升级/配置/关闭”的入口和 ZCode 文案。建议项中
native 为“代码审查”，source 为“PPT 制作”。这些品牌、项目状态、动态文案和裁剪入口
差异均计入全图 metrics，不能当作布局回归。

## release38 视觉状态（2026-10-06）

release38 binary SHA-256 为
`1f60c607818e4b970a51b90fe2bc11109bf4110c1b29a69455537eff065ae602`。来源清单
`out/ely-source-inputs-38.json` 记录 1550 个输入，vendor 哈希无漂移。
`out/ely-native-batch-visual-38.log` 中 settings/workspace 的 24/13 actions 均通过，
进程自然退出、exit code `0`。全图比较保持原始 client 尺寸 `2560x1640`、DPI `192`，
不缩放、不配准、不掩码。

| 页面 | 比较报告 | 不同像素 / 总像素 | 差异比例 | 判断 |
| --- | --- | ---: | ---: | --- |
| General | `out/ely-native/visual-settings-38/general-comparison/comparison-metrics.json` | `753210 / 4198400` | `17.9404059%` | pending |
| Appearance | `out/ely-native/visual-settings-38/appearance-comparison/comparison-metrics.json` | `155268 / 4198400` | `3.6982660%` | pending |
| Workspace / Draft reference8 | `out/ely-native/visual-workspace-38/draft-reference8-comparison/comparison-metrics.json` | `239063 / 4198400` | `5.6941454%` | pending |

General/Appearance 的图像与 release37 相同；Workspace 此时显示中午问候，固定 source
reference8 为上午问候。全图比例因此受内容变化影响，不能直接将 `6.9171%` 到 `5.6941%`
解释为几何改善或通过。品牌、项目状态及已裁剪入口继续包含在比较中。

同条件原图确认 Composer 主体边界一致，但 `y=1166` 的底部阴影 source 为
`x=861..2196`，native 为 `x=879..2178`；source 最后可见阴影行是 `y=1200`，native 是
`y=1203`。GPUI 的负 spread 收缩矩形而不收缩圆角；release39 正在修正这个绘制语义，
并按固定来源校准模型按钮间距与选中态 alpha，尚未形成 release39 视觉结论。

同时间参考刷新 `zcode-windows-draft-reference-9` 在创建窗口前自然退出，启动失败证据
已保留；隔离身份后的 `9b` 取证计划完成，但实际图片仍为欢迎页，因此只保存为
`out/zcode-reference-3.14.3/windows-onboarding-dark-reference9b.png`，不替换 Draft reference8。
取证计划的 `passed` 不能证明页面状态正确或像素通过。

## release37 视觉状态（2026-10-06）

release37 目标 binary 的 SHA-256 为
`5d3f57127a24e88456cf89c9c2ec82f1b9b4080f77b566a13b61c29a76c063bd`。视觉取证批次
`out/ely-native-batch-visual-37.log` 中 visual-settings/workspace 分别为 24/13 actions，
均自然退出、`exit_code=0`。三份比较均使用固定 ZCode 3.14.3 source，metrics 的
`acceptance` 仍为 `pending`、`assessment` 为 `not_identical`。

| 页面 | 比较报告 | 不同像素 / 总像素 | 差异比例 | 当前判断 |
| --- | --- | ---: | ---: | --- |
| General | `out/ely-native/visual-settings-37/general-comparison/comparison-metrics.json` | `753210 / 4198400` | `17.9404059%` | pending |
| Appearance | `out/ely-native/visual-settings-37/appearance-comparison/comparison-metrics.json` | `155268 / 4198400` | `3.6982660%` | pending |
| Workspace37 / Draft reference8 | `out/ely-native/visual-workspace-37/draft-reference8-comparison/comparison-metrics.json` | `290408 / 4198400` | `6.9171113%` | pending |

release37 的三份比较仍是差异证据，不能把整图差异改善概括为整体 PASS；Workspace 差异比例
较 release36 上升。General 与 Appearance 继续绑定 settings reference2，Workspace 继续绑定
Draft reference8；完整产品和性能验收仍需独立证据。

### Appearance 原图分区测量（release37）

以下数值直接来自 `appearance-comparison/source.png` 和 `native-client.png` 的
`2560x1640` 原图，未缩放。native 截图裁切偏移为 `x=13,y=0`，DPI 为 `192`。

- 两张卡片的主体边界与分隔线基本一致：界面设置卡为
  `x=697..2360,y=370..674`（`1664x305`），代码设置卡的 source 为
  `x=697..2360,y=867..1625`（`1664x759`）；边框为 `#414141`，卡片内底色为
  `#2b2b2b`，页面底色为 `#161616`。第一张卡的边框差异只有几十个圆角抗锯齿像素。
  第二张卡是实际几何差异：native 在 `y=1626..1629` 仍绘制 `#2b2b2b`，比 source
  多出 4px 的底部带状区域，source 此处已经是 `#161616`；应优先检查代码设置卡的
  底部 padding/裁切。
- Select 外框尺寸完全相同，均为 `384x64`：主题 `x=1943..2326,y=415..478`，浅色
  代码主题 `y=912..975`，深色代码主题 `y=1063..1126`。数值 Input 外框也相同，均为
  `224x56`：界面字号 `y=570..625`，代码字号 `y=1522..1577`。Select 的底色和直线
  边框 token 相同；native 只是在圆角栅格化时上下实线各宽约 2px，属于绘制差异而非
  外框尺寸偏移。三处 source chevron 的 bbox 为 `x=2287..2302`、高 8px、主色
  `#8a8a8a`（28 个像素）；native bbox 同 x、约高 10px，主色为 `#5d5d5d/#515151`
  （62 个像素），这是 Select 最明确的颜色/形状校准点。
- Appearance 页面当前没有 Slider；截图里的字号控件是 Input。两枚 Switch 外框均为
  `64x36`：开启 `x=2263..2326,y=1229..1264`，关闭 `y=1380..1415`。开启态 source
  与 native 几乎一致，只差 28 个边缘抗锯齿像素；关闭态几何也一致，但 source 的
  主 track 色为 `(107,106,107)`、native 为 `(106,106,106)`，该颜色对占 1112 像素，
  说明差异来自底色合成/通道而非尺寸。
- Input 外框相同，但内容的基线和右侧内边距不同：界面字号数字 glyph source bbox
  `x=2234..2259,y=589..608`，native 为 `x=2229..2254,y=590..609`；代码字号 source
  为 `x=2234..2259,y=1540..1559`，native 为 `x=2229..2254,y=1542..1561`。这是文字
  内部定位和字体栅格化差异，不能通过改变 Input 外框尺寸修复。
- 侧栏 entry 的几何没有整体漂移：native UIA 的按钮均为 `504x64`，Appearance 选中
  行 raw bbox 为 `x=16..519,y=348..411`，source/native 相同。选中背景 source 为
  `#414141`、native 为 `#404040`，对应 `30411` 个像素；选中行前景阈值 `>=200` 的
  像素为 source `501`、native `563`，bbox 均约 `x=38..138`，native 底部少 1px。
  icon 区 source/native 为 `121/196` 个像素，label 区为 `380/367` 个像素，说明主要
  是图标实现和字体粗细/抗锯齿差异，entry 高度本身无需调整。

因此 Appearance 的修复优先级是：先处理代码卡底部 4px 几何带，再校准 Select chevron
的颜色和笔画高度、选中侧栏背景 token及 icon/字体栅格化；Switch/Input 外框尺寸没有证据
支持改动。release37 同一批次的 `settings`、`settings-light`、`font-settings` 分别为
102/102/72 actions passed；`unsupported-hooks-diagnostics` 在 ordinal `18` 因 UIA
未观察到“当前 Native 执行入口未实现”而 failed，证据见
`out/ely-native-batch-settings-rest-37.log` 和对应 native report。

## 最新视觉状态：release36（2026-10-06）

release36 目标 binary 的 SHA-256 为
`14894efe6301874e91e6a2b34e9af3dfc5847fc71e62e8ea02dd7c54fcd121eb`。
`out/ely-native/visual-settings-36/native-run/report.json` 的 24 个取证动作通过，
`out/ely-native/visual-workspace-36/native-run/report.json` 的 13 个取证动作通过，两个被测
进程均自然退出、`exit_code=0`。取证通过不等于像素通过；三份比较 metrics 均为
`acceptance=pending`、`assessment=not_identical`。

| 页面 | 比较报告 | 不同像素 / 总像素 | 差异比例 | 当前判断 |
| --- | --- | ---: | ---: | --- |
| General | `out/ely-native/visual-settings-36/general-comparison/comparison-metrics.json` | `754208 / 4198400` | `17.9641768%` | pending |
| Appearance | `out/ely-native/visual-settings-36/appearance-comparison/comparison-metrics.json` | `156375 / 4198400` | `3.7246332%` | pending |
| Workspace36 / Draft reference8 | `out/ely-native/visual-workspace-36/draft-reference8-comparison/comparison-metrics.json` | `271756 / 4198400` | `6.4728468%` | pending |

release36 的三份比较均只作为差异证据，不能由较低差异比例改写为 PASS。General 和
Appearance 仍绑定固定 ZCode 3.14.3 settings reference2；Workspace 使用 Draft reference8。
release37 的独立比较结果见本文顶部；本 release36 历史 metrics 和判断保持不变。

同一 release36 binary 的功能批次同步记录为：provider-model 在 ordinal `16` 因完成提示
等待超时而 failed（网络已收到 HTTP `200` 和 `MessageEnd`，但 complete 可能等待 EOF）；
provider-settings `119` actions passed；code-settings 在 ordinal `43` 的 `Space` 持久化
断言失败且尚未到 `Enter`；keybindings `61` actions 与 cold-resume `35` actions passed。
这些功能结果不改变本节三份视觉 metrics 的 `pending` 状态。

## 最新视觉状态：release35（2026-10-06）

release35 目标 binary 的 SHA-256 为
`e66a3872a2219821722dbb0e6b6facc4b65c05f2e883c17f10aa43949efb506d`。视觉取证报告
`out/ely-native/visual-settings-35/native-run/report.json` 和
`out/ely-native/visual-workspace-35b/native-run/report.json` 均为 `passed`，被测进程均
自然退出、`exit_code=0`。这些是 runner/取证结果，不等于像素验收通过；下列比较报告均为
`acceptance=pending`、`assessment=not_identical`。

| 页面 | 比较报告 | 不同像素 / 总像素 | 差异比例 | 当前判断 |
| --- | --- | ---: | ---: | --- |
| General | `out/ely-native/visual-settings-35/general-comparison/comparison-metrics.json` | `779347 / 4198400` | `18.5629526%` | pending；差异同时包含内容和布局 |
| Appearance | `out/ely-native/visual-settings-35/appearance-comparison/comparison-metrics.json` | `237230 / 4198400` | `5.6504859%` | pending；总体仍非 identical |
| Workspace35b / reference6 | `out/ely-native/visual-workspace-35b/workspace-comparison/comparison-metrics.json` | `668250 / 4198400` | `15.9167778%` | pending；存在 state mismatch，不能归类为纯 geometry regression 或 PASS |
| Workspace35b / morning draft reference8 | `out/ely-native/visual-workspace-35b/draft-comparison-reference8/comparison-metrics.json` | `323274 / 4198400` | `7.6999333%` | pending；状态更接近，但仍有产品裁剪差异 |

Workspace35b 与固定 source reference6 的比较必须显著标记为状态不匹配：source 没有打开
项目且没有 Session，native35b 已创建 Session 并注册了隔离的原生验收项目。比较截图
`workspace-comparison/source.png` 显示“夜深啦，别忘了照顾好自己哦”，而
`workspace-comparison/native-client.png` 显示“上午好呀，有什么想让我帮忙的吗”；因此
source reference6 是夜间问候、native35b 是上午问候。`15.9167778%` 不能作为纯几何回归，
也不能写成 PASS。

本轮新增官方 ZCode 3.14.3 source reference8 是本日上午 draft，报告
`out/ely-native/zcode-windows-draft-reference-8/native-run/report.json` 为 `passed`、自然
退出 `0`；裁切 PNG 为
`out/zcode-reference-3.14.3/windows-draft-dark-reference8.png`。reference8 与 native35b
均为无模型、无任务的 draft，并使用同日上午问候语，因此
`out/ely-native/visual-workspace-35b/draft-comparison-reference8/comparison-metrics.json`
提供了更接近的比较。但 source 未打开项目，native 仍注册了隔离原生验收项目；产品品牌、
升级按钮和 PPT 入口也存在裁剪差异，所以 `7.6999333%` 仍保持 `pending`。

reference7 (`out/ely-native/zcode-windows-draft-reference-7/native-run/report.json`) 实际是
onboarding 过程，不是 draft reference；其 close 等待超时并被 runner 强杀，报告为
`killed-after-timeout`。该报告不得作为 draft 来源或 PASS 证据。

Appearance35 的局部检查显示，卡片所有分隔线的 y 坐标均与来源一致，按 `151/152` 交替；
四个文字 ROI 的彩边计数为 `0`。这只能支持局部几何和文字 ROI 结论，不能抵销整图
`5.6504859%` 差异或提升总体像素状态。build36 正在准备 Switch、dropdown、chip 和边界
alpha 修订；在新 binary 和新比较报告产生前，不把这些源码调整写成已验收结果。

## 最新视觉状态：release33（2026-10-06）

本轮目标 binary SHA-256 为
`b3841d8a26e286110396bb6bae43fdb5985f5b36fde44b03391de7fd0d592215`。
`out/ely-native/visual-settings-33/native-run/report.json` 的 24 个取证动作全部通过，
进程自然退出、exit code 为 `0`；四个原生截图的 client 均为 `2560x1640`，DPI 为
`192`，比较没有缩放、自动配准、掩码或删除内容区域。

| 页面 | 比较报告 | 不同像素 / 总像素 | 差异比例 |
| --- | --- | ---: | ---: |
| General | `out/ely-native/visual-settings-33/general-comparison/comparison-metrics.json` | `769708 / 4198400` | `18.3333651%` |
| Appearance | `out/ely-native/visual-settings-33/appearance-comparison/comparison-metrics.json` | `338012 / 4198400` | `8.0509718%` |
| Workspace | `out/ely-native/visual-settings-33/workspace-comparison/comparison-metrics.json` | `315557 / 4198400` | `7.5161252%` |

三份 `comparison-metrics.json` 均为 `acceptance=pending`、`assessment=not_identical`，
因此这些数字是差异证据，不是像素验收通过。General 使用固定来源
`windows-settings-general-dark-reference2.png`，Appearance 使用
`windows-settings-appearance-dark-reference2.png`，Workspace 使用
`windows-workspace-dark-reference6.png`；三份来源均来自固定 ZCode 3.14.3 提交。

release33 的 Appearance 差异为 `8.0509718%`，高于 release32 的 `6.515625%`；本轮
24px 标题改动后该页比较恶化，release34 正在校正。该校正不改变 release33 报告的
历史事实，也不能把当前 release34 源码树的状态回填到 release33。

## 历史视觉状态：release32（2026-10-06）

当前 binary SHA-256 为 `95f3778547682d46412345a21d8261c0920512d934f036ae6d6e2d8db26ccee7`。
`out/ely-native/visual-settings-32/native-run/report.json` 的 24 个取证动作通过，进程自然退出 0。
同平台 Dark、DPI192、client `2560x1640`，没有缩放、配准、掩码或删除不同的内容区域。

| 页面 | 比较报告 | 不同像素 / 总像素 | 差异比例 |
| --- | --- | ---: | ---: |
| General | `out/ely-native/visual-settings-32/general-comparison/comparison-metrics.json` | 786637 / 4198400 | 18.7365901% |
| Appearance | `out/ely-native/visual-settings-32/appearance-comparison/comparison-metrics.json` | 273552 / 4198400 | 6.515625% |

两份报告仍为 `acceptance=pending`、`assessment=not_identical`。Appearance 已补齐代码设置
卡片并校正输入框和文字色，但仍有字体抗锯齿、Switch、滚动条、品牌和导航内容差异。
General 的产品设置项与固定来源不同，整图差异同时包含内容与布局，不能当作共同控件的
纯几何误差。大面积背景相同不证明控件已对齐，也不能把 93.48% 的相同像素写成像素验收通过。

## 历史视觉状态：release31

像素验收仍为 pending。release31 设置页已生成同平台全图比较，binary SHA-256 为
`1e59a14e260a5997c0150a3332f56275e12945ae9b9671c4b68027f53d7061c0`。
固定 ZCode 3.14.3 reference2 与目标均为 Dark、DPI192、client `2560x1640`，
未缩放、自动配准或掩码。

| 页面 | 比较报告 | 不同像素 / 总像素 | 差异比例 |
| --- | --- | ---: | ---: |
| General | `out/ely-native/visual-settings-31/general-comparison/comparison-metrics.json` | 953721 / 4198400 | 22.7162967% |
| Appearance | `out/ely-native/visual-settings-31/appearance-comparison/comparison-metrics.json` | 1021214 / 4198400 | 24.3238853% |

两份报告均为 `acceptance=pending`、`assessment=not_identical`。已确认生产偏差包括
Dark foreground（目标 `#e5e5e5`，来源 ZaiDark `#d4d4d4`）、Ely Compact 下选择器
28px 与来源 32px、输入背景/圆角、字号输入对齐和缺失的代码设置卡片。
后续源码正在修复，但 release31 截图不能证明这些修改生效。GPUI 与 Chromium
文字边缘的抗锯齿方式也有差异，必须与布局偏差分别说明，不能宣称逐像素相同。

最新目标空态比较仍绑定 release28，原始不同像素为
313961 / 4198400（7.4781107%）；`assessment=not_identical`。该比例包括品牌、
数据、字体和控件差异，大片背景一致不能证明控件对齐。

本轮新增固定 ZCode 3.14.3 原生 General / Appearance 参考：
`out/ely-native/zcode-windows-settings-reference-2/native-run/report.json`，31 actions 通过，
仍绑定官方 exe SHA-256 `afb17a861ce435e1ebecb32fa99ca14daa94adadc6054b5f2116b418248cace5`。
General BMP 为 `screenshots/0021-settings-general.bmp`，Appearance BMP 为
`screenshots/0026-settings-appearance.bmp`；对应 metrics ordinal 为 22、27。
按 metrics 裁切的 client PNG 已保存到 `out/zcode-reference-3.14.3/`：
`windows-settings-general-dark-reference2.png`、`windows-settings-appearance-dark-reference2.png`，
均为 2560 × 1640，DPI 192、逻辑窗口 1280 × 820。

已确认结构差异包括 General/Appearance 职责混合、主题 Radio 与来源 Combobox 不同、
资源类别混在单页。当前正在调整；没有新 binary 的目标截图与分区 diff 之前，不提升像素状态。

## 历史基线与比较方法

本文规定如何把原生 `PrintWindow` BMP 与固定 ZCode 3.14.3 PNG 放到同一 client 区坐标
系中比较。比较脚本只生成证据和误差指标，不根据像素差异宣布产品验收通过；没有相同
主题、窗口、DPI、字体和页面状态时，结果必须保持 `pending`。

当前 dpr2 PNG 是浏览器内容参考，不是 Windows 原生窗口基线。它的报告记录为
Playwright Chromium context 加 Vite web shell，`source-appearance-dpr1.json` 的 UA 也是
`HeadlessChrome`；其中 `window-maximized` 只是 DOM class，不能证明 Win32 窗口最大化。
因此，源图中的浏览器壳层几何为 `radius=12px`、`inset=0px`，而 Windows 原生目标的
几何约束为 `radius=5px`、`inset=4px`。现在已有固定来源的同平台 Windows reference-5；
与旧浏览器源图的像素结果仍只能作为历史差异，不能宣称像素 `PASS`。

## 固定源证据

固定源提交为 `29628c9acdb81b703bbd4080c207a0e7ce5e276e`（ZCode 3.14.3）。源基线元数据
来自 `third-party/zcode/design-baseline.json` 和
`out/native-live/zcode-source-baseline-29628c9-dpr2/report.json`：

- source root 为 `packages/ui/src`；来源许可证和文件映射见 `third-party/zcode/`。
- 源 PNG 使用 CSS viewport `1280x820`、DPR `2`，物理尺寸为 `2560x1640`。
- 源页面为 Dark Zai、`zh-CN`、无 Provider 的空工作区；该状态包含“当前没有可用模型”
  提示和空 Composer。

固定来源约束 ZCode 3.14.3 的内容、主题和布局规则。当前安装的
`D:\dev\ZCode\ZCode.exe` FileVersion 为 `3.14.4.7912`，不是 fixed 3.14.3，不能将其
启动截图替换为同源原生基线；固定来源的官方 Windows reference-5 见下节。

## 平台边界

| 参考或目标 | 壳层圆角 | 外沿 inset | 证据与含义 |
| --- | ---: | ---: | --- |
| dpr2 source PNG（浏览器路径） | `12px` | `0px` | `report.json.browser`、HeadlessChrome UA；可用于内容布局参考 |
| Windows 原生目标 | `5px` | `4px` | `workspaceShellWindowChrome.ts` 的 Windows 分支、`WorkspaceShellLayout.tsx` 的桌面 `p-1`，原生 `style.rs::DESKTOP_INSET`，并由 reference-5 截图校准 |

`workspace-dpr2.json` 中的 `window-maximized` 不能填补 HWND、window/client rect、DPI 或
原生最大化状态证据。源图仍可用于校准侧栏、正文列和 Composer 的内容边界，但不能用于
证明目标实现 Windows 原生标题栏、窗口外壳、`radius=5px` 或 `inset=4px` 已通过像素验收。

固定提交中的源 CSS/TSX 给出以下可复核的内容布局量值；这些数值可继续用于校准边界，
不能与上表平台壳层几何混写为像素一致：

| 来源文件与行 | 源规则 | 物理/逻辑换算 |
| --- | --- | --- |
| `packages/ui/src/app-shell/WorkspaceShellLayout.tsx:99-101` | 侧栏默认和最小宽度 `264`，最大宽度为容器 `50%` | CSS 逻辑像素；DPR2 时默认约 `528` 物理像素 |
| `packages/ui/src/app-shell/WorkspaceShellLayout.tsx:1345-1349` | 桌面侧栏/面板分隔条使用 `w-1` | Tailwind 默认 spacing 基数 `.25rem`，即 `4px` 逻辑像素 |
| `packages/ui/src/v4/conversationLayout.ts:3-9` | 草稿 `max-w-2xl`；普通列在 `864px` 断点为 `max-w-4xl`，在 `1280px` 断点为 `max-w-6xl` | `672px`、`896px`、`1152px` 逻辑像素 |
| `packages/ui/src/v4/ConversationComposer.tsx:2191-2205` | `centered` 草稿 Composer 使用 `max-w-2xl` | `672px` 逻辑像素 |

源普通会话列不是单一固定宽度：`<864px` 使用全部可用宽度，`>=864px` 使用
`calc(100% - 6rem)` 并受 `max-w-4xl=896px` 限制，`>=1280px` 切换到
`calc(100% - 24rem)` 并受 `max-w-6xl=1152px` 限制，同时可能伴随状态面板偏移。
原生实现应通过 `apps/desktop/src/native_ui/style.rs::conversation_content_width` 对应
这三段规则；固定常量仍包括 `SIDEBAR_WIDTH=264`、`DESKTOP_INSET=4`、
`TITLEBAR_HEIGHT=48`、设置页 `CONTENT_MAX_WIDTH=896` 和空草稿
`EMPTY_COMPOSER_MAX_WIDTH=672`。比较报告必须同时记录源规则和原生规则，不能把两者
混写为已经像素一致。

## 固定来源 Windows 参考

官方 Windows reference-5 已在固定来源提交上完成真实窗口采集，报告为
`out/ely-native/zcode-windows-reference-5/native-run/report.json`。它使用
`out/zcode-reference-3.14.3/ZCode.exe`，SHA-256 为
`afb17a861ce435e1ebecb32fa99ca14daa94adadc6054b5f2116b418248cace5`；官方 v3.14.3
annotated tag peeled 到 `29628c9acdb81b703bbd4080c207a0e7ce5e276e`。参考计划固定 Dark、
DPI `192`、逻辑窗口 `1280x820` 和物理 client `2560x1640`，最终截图与 metrics 为：

| 项目 | 证据 |
| --- | --- |
| 原生截图 | `out/ely-native/zcode-windows-reference-5/native-run/screenshots/0017-workspace-final.bmp` |
| metrics | `out/ely-native/zcode-windows-reference-5/native-run/metrics/0018-workspace-final.json` |
| client / DPI | `2560x1640` / `192`（DPR `2`） |
| 进程状态 | `pid=3312`，HWND `0x2d108c8`，自然退出 `exit_code=0` |

该 reference-5 是同平台、同提交的源窗口参照，可用于校准 Windows `radius=5px`、
`inset=4px` 和标题栏 `48px`。它不等于 KeenCode 目标功能验收；目标实现仍需在同一页面
状态下生成自己的截图或 diff。旧的 `23.3496%` 来自 `zcode-capture-1` 的浏览器源图
比较；旧的 `18.6916%` 来自 `out/ely-native/release19-calibration/visual-comparison/
comparison-metrics.json` 中 release19 目标原生 PNG 与真实 Windows reference-5 PNG
的原生对原生比较。release19 与当前目标的 greeting 时间和内容不同，因此该历史比例
不能宣称像素 `PASS`。

release21 已生成同平台首屏比较，输出指标位于
`out/ely-native/release21-calibration/visual-comparison/comparison-metrics.json`；该文件
记录 `acceptance=pending`、`assessment=not_identical` 和差异比例 `8.4274%`。这是当前
此前的原生首屏误差证据，不是像素验收通过；页面状态和内容仍需与固定 reference-5 严格对齐。

## 历史比较输入证据（zcode-capture-1）

本次原生输入为 `out/ely-native/zcode-capture-1/native-run/`：

| 项目 | 事实 |
| --- | --- |
| 原生 BMP | `screenshots/0001-startup.bmp`，整窗 `2586x1653` |
| 原生 metrics | `metrics/0002-startup.json` |
| client 区 | `2560x1640`，DPI `192`，逻辑尺寸对应 `1280x820` |
| windowRect | 左上 `(755,406)`，尺寸 `2586x1653` |
| clientOriginScreen | `(768,406)` |
| 裁切偏移 | `x=13`、`y=0`；裁切框为 `(13,0)-(2573,1640)` |
| 源 PNG | `out/native-live/zcode-source-baseline-29628c9-dpr2/chat-workspace-dpr2.png`，`2560x1640` |
| 原生报告 | 截图和 metrics action 成功，但进程最终 `killed-after-timeout`；不构成功能验收 |

脚本只使用上述 offset 和 client 尺寸裁切，不缩放、不拉伸、不把整窗标题栏与源 client
区直接比较。输出目录应与输入报告分开，避免覆盖原始截图和 metrics。

## 状态差异

源 PNG 和原生 BMP 的尺寸可以对齐，但页面状态不能直接视为同一状态：

- 源页面是无 Provider 的 ZCode 空工作区，显示源侧栏、空 Composer、无可用模型提示和
  建议 Prompt。
- 原生截图显示本地项目/原生验收侧栏，正文为“从侧栏打开一个会话”，没有源 PNG 中的
  Composer 内容。
- 源 `workspace-dpr2.json` 的 body text 还包含命令面板、自动化、插件市场、账号/升级、
  浏览器控制等来源产品入口。它们是固定源页面状态的记录，不代表目标产品必须复制；
  产品边界以 `DESIGN.md` 和 `docs/frontend-zcode-source.md` 为准。
- 因此，脚本输出的误差只能说明两张当前图在几何裁切后有多少像素不同，不能说明原生 UI
  已经通过 ZCode 像素验收。

在相对稳定的背景行上，当前截图还可观察到一个几何信号：原生整窗侧栏边界约在
`x=539/541`，减去 client 偏移后约为 `x=526/528`；源图边界约为 `x=536/538`。这约
`10` 个物理像素的差异只是图像观察线索，不替代 DOM/布局证据，也不能排除页面状态造成
的边缘差异。

## 比较命令

从仓库根目录运行：

```text
python tooling/scripts/compare_native_zcode_visual.py \
  --native-bmp out/ely-native/zcode-capture-1/native-run/screenshots/0001-startup.bmp \
  --source-png out/native-live/zcode-source-baseline-29628c9-dpr2/chat-workspace-dpr2.png \
  --metrics out/ely-native/zcode-capture-1/native-run/metrics/0002-startup.json \
  --output-dir out/ely-native/zcode-capture-1/visual-comparison
```

脚本生成 `native-client.png`、`source.png`、`side-by-side.png`、`overlay.png`、
`diff.png` 和 `comparison-metrics.json`。指标包括总像素、不同像素、差异比例、每通道
平均绝对误差、均方根误差、最大通道差异以及输入文件 SHA-256。`diff.png` 只做可视化
放大，不改变误差统计。

`comparison-metrics.json` 的 `acceptance` 固定为 `pending`；即使不同像素为零，也必须
结合同主题、同窗口尺寸、同 DPI、同字体、同页面状态的原生报告，才能进入正式验收矩阵。

## 历史比较结果（zcode-capture-1）

已对当前 capture 执行上述脚本，产物位于
`out/ely-native/zcode-capture-1/visual-comparison/`。输入 SHA-256 和几何裁切记录均已
写入 `comparison-metrics.json`。结果为：

| 指标 | 结果 |
| --- | ---: |
| 比较尺寸 | `2560x1640` |
| 总像素 | `4,198,400` |
| 不同像素 | `980,308` |
| 差异比例 | `23.3496%` |
| 每通道平均绝对误差 | R `5.7975` / G `5.8001` / B `5.8153` |
| 每通道均方根误差 | R `22.4918` / G `22.4971` / B `22.5421` |
| 最大通道差异 | `233` |

该结果记录了当前两张图的差异，不能解释为布局或功能通过：源图是空 Provider Composer
状态，原生图是“从侧栏打开一个会话”状态；两者还分别走了浏览器 `radius=12px/inset=0px`
和 Windows 原生 `radius=5px/inset=4px` 的不同壳层路径。源 CSS 还包含宽屏
`max-w-6xl=1152px` 分支，而原生 helper 需要按同一三段断点计算会话列宽。这些状态和
规则差异需要后续用同一页面状态重新采集后再判断。

## release21 同平台首屏比较

本次 native 输入为 `out/ely-native/release21-calibration/native-run/`，与固定 Windows
reference-5 的 source PNG `out/zcode-reference-3.14.3/windows-workspace-dark.png`
比较。输出为 `out/ely-native/release21-calibration/visual-comparison/`；指标文件明确
保留 `acceptance=pending` 和 `assessment=not_identical`。

| 指标 | 结果 |
| --- | ---: |
| 比较尺寸 | `2560x1640` |
| 总像素 | `4,198,400` |
| 不同像素 | `353,817` |
| 相同像素 | `3,844,583` |
| 差异比例 | `8.4274%` |
| 原生输入 | `screenshots/0005-startup.bmp`，整窗 `2586x1653`，裁切偏移 `x=13,y=0` |
| 原生 metrics | `metrics/0006-startup.json` |
| DPI / client | `192` / `2560x1640` |

该结果比 release19 的 `18.6916%` 更接近，但两次比较的 greeting 时间和内容不同，且
指标文件仍为 `not_identical`/`pending`。因此它只能记录当前首屏几何和内容差异，不能提升
产品、像素或性能验收状态。

## release24 同平台首屏比较

release24 使用 binary SHA-256
`56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f`。本次 native 输入为
`out/ely-native/release24-calibration/native-run/`，与固定来源 reference6
`out/zcode-reference-3.14.3/windows-workspace-dark-reference6.png` 比较，指标文件为
`out/ely-native/release24-calibration/visual-comparison/comparison-metrics.json`。
报告明确记录 `acceptance=pending`、`assessment=not_identical`。

| 指标 | 结果 |
| --- | ---: |
| 比较尺寸 | `2560x1640` |
| 总像素 | `4,198,400` |
| 不同像素 | `315,758` |
| 相同像素 | `3,882,642` |
| 差异比例 | `7.5209%` |
| 原生输入 | `screenshots/0005-startup.bmp`，整窗 `2586x1653`，client `2560x1640` |
| 原生 metrics | `metrics/0006-startup.json` |
| DPI / awareness | `192` / `per-monitor-aware` |

该结果是 release24 对 reference6 的全图差异证据，仍受页面内容和状态一致性边界约束；它不
提升产品、像素或性能验收状态，正式像素判定继续为 `pending`。

## release25 同平台首屏比较

release25 使用 binary SHA-256
`2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452`。本次 native 输入为
`out/ely-native/release25-calibration/native-run/`，与固定来源 reference6
`out/zcode-reference-3.14.3/windows-workspace-dark-reference6.png` 比较，指标文件为
`out/ely-native/release25-calibration/visual-comparison/comparison-metrics.json`。指标明确记录
`acceptance=pending`、`assessment=not_identical`。

| 指标 | 结果 |
| --- | ---: |
| 比较尺寸 | `2560x1640` |
| 总像素 | `4,198,400` |
| 不同像素 | `314,065` |
| 相同像素 | `3,884,335` |
| 差异比例 | `7.4805878%` |
| 原生输入 | `screenshots/0005-startup.bmp`，整窗 `2586x1653`，client `2560x1640` |
| 原生 metrics | `metrics/0006-startup.json` |
| DPI / awareness | `192` / `per-monitor-aware` |

该结果只记录 release25 对 reference6 的首屏差异，仍受主题、窗口、字体和页面状态一致性
约束；`7.4805878%` 不是像素通过，正式像素判定继续为 `pending`。该证据绑定 release25
SHA，不代表 release26，也不扩展为产品功能或性能验收。

## release26 同平台首屏比较

release26 使用 binary SHA-256
`2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94`。本次 native 输入为
`out/ely-native/release26-calibration/native-run/`，与固定来源 reference6
`out/zcode-reference-3.14.3/windows-workspace-dark-reference6.png` 比较，指标文件为
`out/ely-native/release26-calibration/visual-comparison/comparison-metrics.json`。指标明确记录
`acceptance=pending`、`assessment=not_identical`。

| 指标 | 结果 |
| --- | ---: |
| 比较尺寸 | `2560x1640` |
| 总像素 | `4,198,400` |
| 不同像素 | `314,065` |
| 相同像素 | `3,884,335` |
| 差异比例 | `7.4805878%` |
| 原生输入 | `screenshots/0005-startup.bmp`，整窗 `2586x1653`，client `2560x1640` |
| 原生 metrics | `metrics/0006-startup.json` |
| DPI / awareness | `192` / `per-monitor-aware` |

该结果只记录 release26 对 reference6 的首屏差异，仍受主题、窗口、字体和页面状态一致性
约束；`7.4805878%` 不是像素通过，正式像素判定继续为 `pending`。该证据不扩展为产品
功能或性能验收，也不能由 release26 的局部设置或失败诊断推出完整产品结论。

release27 的 `permission-askuser-27`、`workbench-27` 和 `workbench-27b` 只生成了功能验收
截图/UIA/metrics，未生成独立的 `visual-comparison/comparison-metrics.json`。这些运行不能改变
本文件的像素结论；正式视觉验收仍为 `pending`，终端高度相关根因也不由像素证据确认。

## release28 同平台首屏比较

release28 使用 binary SHA-256
`a902bc39845758ecab43fc2d6a415877de277b97226edd7be19979649cb09e1b`。本次 native 输入为
`out/ely-native/release28-calibration/native-run/`，与固定来源 reference6
`out/zcode-reference-3.14.3/windows-workspace-dark-reference6.png` 比较，指标文件为
`out/ely-native/release28-calibration/visual-comparison/comparison-metrics.json`。报告生成 2 份
截图和 2 份 metrics，进程自然退出、`exit_code=0`；指标明确记录 `acceptance=pending`、
`assessment=not_identical`。

| 指标 | 结果 |
| --- | ---: |
| 比较尺寸 | `2560x1640` |
| 总像素 | `4,198,400` |
| 不同像素 | `313,961` |
| 相同像素 | `3,884,439` |
| 差异比例 | `7.4781107%` |
| 原生输入 | `screenshots/0005-startup.bmp`，整窗 `2586x1653`，client `2560x1640` |
| 原生 metrics | `metrics/0006-startup.json`、`metrics/0011-empty-session.json` |
| DPI / awareness | `192` / `per-monitor-aware` |

分区指标嵌在同一 `comparison-metrics.json` 的 `regions.comparisons` 中，没有单独的 partition
文件：

| 分区 | 不同像素 / 总像素 | 差异比例 |
| --- | ---: | ---: |
| sidebar | `36,199 / 885,600` | `4.0875113%` |
| workspace_header | `7,052 / 404,000` | `1.7455446%` |
| greeting | `135,182 / 1,010,000` | `13.3843564%` |
| composer | `108,488 / 1,010,000` | `10.7413861%` |
| quick_actions | `22,756 / 606,000` | `3.7551155%` |

该结果只记录 release28 对 reference6 的首屏和分区差异；区域仍保留数据、品牌和文案差异，
没有掩码或自动配准。`7.4781107%` 不是像素通过，正式视觉验收继续为 `pending`，也不能由
该校准结果推出 Workbench、Goal/Workflow 或完整产品验收。
