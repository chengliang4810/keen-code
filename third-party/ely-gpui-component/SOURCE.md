# Ely GPUI Components 来源说明

## 固定来源

- 上游项目：`https://github.com/ZacharyZhang-NY/Ely-GPUI-Components`
- 固定 revision：`94f34c9f8e98b5f4b3078776a4c197b9021f4cdf`
- 许可证：MIT，正文来自上游根目录 `LICENSE`
- 上游工作树：固定 revision 对应的源仓库保持干净

2026-10-06 从 `7186179` 快进到上述提交。复制目录已同步该 revision 中仍属于编译
边界的 `src/documents/templates.rs` 与 `src/i18n/builtin.rs`；未复制的 Showcase/CI
及其他展示内容继续按下文裁剪。局部补丁以固定 revision 的文件内容为基准，完整文件
清单和哈希见下文。

## 本地裁剪

保留本地 path dependency 编译所需的 `Cargo.toml`、`src/`、`assets/` 和根目录 MIT
许可证。对照固定 revision 的 1661 个 tracked 路径，将上游 `Cargo.lock` 排除在复制
边界之外后，复制边界共有 1660 个路径。本地目录包含 1167 个实际复制的上游文件、
验证期间生成的 `Cargo.lock` 和本说明文件 `SOURCE.md`；上游以下 493 个复制边界路径
未复制：

- `.github/` 7 个、`examples/` 358 个、`frontend/` 104 个、`scripts/` 8 个、
  `tasks/` 9 个；
- 根级 `.gitignore`、`AGENTS.md`、`CONTRIBUTING.md`、`README.md`、`TASKS.md`、
  `gpui-components.md` 和 `logo.png`。

上游 `Cargo.lock` 不属于复制边界；本地当前同名文件是验证期间由 Cargo 生成的产物，
与 `target/` 一样不列入来源差异和哈希表。上游固定 revision 没有 `src/assets/LICENSE`；
`src/assets.rs` 的嵌入目录实际是 `assets/`，因此没有伪造缺失路径，保留上游根
`LICENSE`、`assets/icons/LICENSE` 以及随资产目录提供的字体许可证文件。

## 局部修改

以下 29 个文件在忽略 CRLF/LF 换行差异后与固定 revision 存在内容差异。这些差异说明
的是 vendor 副本的 API 和行为边界，不表示所有复制组件都已在 KeenCode 生产界面启用；
实际启用范围由应用调用方决定。

| 文件 | 局部修改 |
| --- | --- |
| `Cargo.toml` | 设置 `autoexamples = false`，移除依赖未复制 gallery/docs 文件的 web profile 与 example 声明。 |
| `src/buttons/button.rs` | 为 `Button` 增加可选文字字号、水平/右侧内边距、内容间距、固定高度、图标尺寸和独立 `aria_label` builder；未设置时保留主题默认值。 |
| `src/buttons/icon_button.rs` | 增加可选固定边长、背景内缩与禁用透明度 builder；未设置时保留 Ely density 的主题尺寸、默认背景行为和 45% 禁用透明度，供固定来源 Composer 控件对齐。 |
| `src/chat/code.rs` | 使用主题中的代码字体、字号和长行换行设置渲染代码块，替代固定界面字号与始终横向滚动。 |
| `src/editor/ime.rs` | 按视觉段过滤 inlay notes、ghost 文本和 IME 坐标；换行边界上的 note 归入后续视觉段。 |
| `src/editor/layout.rs` | 增加 `Wrapped` 行、实际显示范围和预计算软换行行构建，同时保留逻辑 buffer 行号。 |
| `src/editor/line.rs` | 让语法色、空白标记、选区、光标、折叠、鼠标按下和拖拽都使用视觉段字节范围；续行隐藏重复 gutter。 |
| `src/editor/moves.rs` | 软换行开启时按视觉代码行执行上下翻页与目标列保持，跳过 lens、diff、ghost 和 inline chat 行。 |
| `src/editor/row.rs` | 渲染 `Wrapped` 续行，并向行绘制传递对应的视觉范围与续行标记。 |
| `src/editor/state.rs` | 增加软换行配置、代码区域宽度、正文版本缓存键和 setter；正文修改、替换、撤销和重做立即失效旧帧行与换行缓存，避免输入事件先于绘制时访问已删除行；主题 `reduced_motion` 变化时重启插入符计时器。 |
| `src/editor/tests.rs` | 增加软换行视觉段、note 归属、命中测试和上下移动测试；覆盖真实首帧后连续替换、多行 Enter、IME、撤销/重做和 `set_text` 的旧帧失效。 |
| `src/editor/view.rs` | 使用 GPUI 字体 shaping 计算软换行；按正文版本、宽度、字体、字号、advance、gutter、开关和多行 ghost 截断位置缓存结果；统一失效 rows/wraps/key，并拒绝陈旧的越界或非 UTF-8 范围。 |
| `src/forms/combobox.rs` | 增加前缀元素、`InputStyle` 和可替换 indicator；按 `TextInput` Entity 身份重建事件订阅并处理焦点/文本变化，替换输入实体后仍同步选中标签，字段点击会恢复打开并定位当前选项；保留筛选、弹出列表和键盘选择行为。 |
| `src/forms/hotkey.rs` | 为快捷键录制控件增加名称、Button role 和鼠标聚焦行为，改善 UIA 读取与点击后输入。 |
| `src/forms/input.rs` | 增加 `InputStyle` 令牌覆盖接口，可设置高度、左右内边距、字号、背景、边框、圆角和间距。 |
| `src/forms/mod.rs` | 从 forms 公共 API 导出 `InputStyle` 和可选的 `SwitchStyle`。 |
| `src/forms/options.rs` | 为共享浮动选项列表及选项暴露 `ListBox`/`ListBoxOption` role、名称和已知选中状态。 |
| `src/forms/switch.rs` | 为 Switch 增加独立 `aria_label`，没有独立名称时仍回退到可见标签；提供不改变默认调用方行为的可选 `SwitchStyle` 几何、轨道/旋钮颜色和旋钮阴影覆盖；鼠标按下聚焦，并由 GPUI 将已聚焦控件的 Enter/Space `keyup` 合成为 `ClickEvent` 后执行切换，不在 `keydown` 阶段重复处理。 |
| `src/forms/tests/choices.rs` | 为长列表选择控件增加弹出列表已打开的结构回归断言；覆盖替换 `Combobox` 的 `TextInput` Entity 后选中标签同步、选择后关闭列表以及再次点击重开。 |
| `src/forms/text/element.rs` | 根据单行 `TextAlign` 计算水平偏移，并将偏移应用到文字、选区、插入符和 IME 绘制。 |
| `src/forms/text/geometry.rs` | 让光标位置、选区几何和鼠标命中反映单行对齐偏移。 |
| `src/forms/text/mod.rs` | 增加单行 `TextAlign` 与布局偏移状态，并只在 `reduced_motion` 变化时重建光标计时器。 |
| `src/layout/mod.rs` | 从 layout 公共 API 导出 `ScrollbarVisualStyle`。 |
| `src/layout/scroll.rs` | 增加可选滚动条视觉样式、轨道/thumb 几何与颜色、两端箭头按钮、鼠标拖拽和 Enter/Space 键盘滚动；默认行为保持 Ely overlay 滚动条。 |
| `src/menus/draw.rs` | 为菜单、复选和单选行设置对应 UIA role、名称、当前选择与 toggled 状态。 |
| `src/menus/hosts.rs` | 为下拉触发按钮增加独立文字字号、水平/右侧内边距、内容间距、固定高度和图标尺寸，菜单行继续使用原主题设置。 |
| `src/theme/mod.rs` | 增加 `CodeRenderSettings`，同步浅色/深色代码 token、字体、字号和自定义 palette，并在设置后刷新窗口。 |
| `src/theme/syntax.rs` | 增加 `CodeSyntaxTheme` 目录和 GitHub Light/Dark、Ely、Quiet、Paper 五组代码 token。 |
| `src/theme/tests.rs` | 覆盖代码主题目录、默认 palette 同步和代码渲染设置更新后的活动/自定义 syntax。 |

## SHA-256

哈希按内容比较统一将 CRLF/LF 换行归一化为 LF：`original` 是固定 revision 的 Git
对象内容，`local` 是当前本地副本内容。这样不会把 Windows 检出换行差异误记为局部
补丁；`LICENSE` 仅作为许可证保留证据列出，并不是局部修改。`Cargo.lock`、`target/`
和 `SOURCE.md` 不属于下表。

| 文件 | original | local |
| --- | --- | --- |
| `Cargo.toml` | `D1ACC54597F144A1F67971C965681929CCDABD1930196CE04E266188FDAE8AB6` | `EEC149F114DCA679384BB9470D7FD98ADBCC952077440D41BCFC4AA45C897396` |
| `src/buttons/button.rs` | `7DFC4B578AEF410DE055FEA5E140193A8FCD8052EC7B549EFA52A652AC5684EB` | `AF9B6A788BD4CF00B654DF19F9D13F99D25507E52117E6980634107C337CE5DD` |
| `src/buttons/icon_button.rs` | `48BAE2CEBCAB833F04D01C0E7FF626796CB009E8499E4B851C772C617C2A3CC4` | `F7C385C7B3FB018D07BE08B5F02E7B5886A11BE9973B500BF419A297F820BCE6` |
| `src/chat/code.rs` | `C5534CD77322CBA9E629BBE7075ADAC274325FC17BE78984F8B6966573A9FBEF` | `C489892F3055FC7FA75A7AA61E9AD68F2B9DE7FB7945D45042DA04B825017B8F` |
| `src/editor/ime.rs` | `0ABBE392CBFF8179028E20F4DE34732A27086ACE7E3877214260D464662CBFDC` | `8E5A2A1F27A5B3633036596EBF20EE8D30CC1AD65A277D4BB0FC1E436D74EFD9` |
| `src/editor/layout.rs` | `7CBDDB8F796F7A779235A92FE6F788F31272C418C6B962F0B8025270C5265F90` | `F8557833B551EED4F8E34C37C2E85876B2162DCB8E68EB225F1F646681D50198` |
| `src/editor/line.rs` | `BD0E86D1E566967F683155C70BBD643CB56585AE3AAF55C19D8BDAEF29486529` | `254695282FDA1360CB837DACFADC22BEF47A3BA8CF4FDB5CDB6E103488619A85` |
| `src/editor/moves.rs` | `CB48DF603C5C4A4D1DDD83A37F92ACE7E73748D3DB5343FC0CDFD3CE380EE7F8` | `BBA92681EB5362F2F0AA279755D5286F68264DB2F1C1F8A5425F300D03DFD619` |
| `src/editor/row.rs` | `029FF8B7C92E4ED91BBA43F085B2AC166D6F6C59CA14934EF1BC0EFBE71C801D` | `DF5A85389461F7E0059DE0AAB717CB7B55DD537347BB64E37EB9C55536559C98` |
| `src/editor/state.rs` | `DFBA0DBB75C82AF86752D7BD1B53C05C3558E7A1D116B0D80724F84299EFBEA2` | `C4BB0CD3002649A3CC2B5B41E37052A00121B6C9FD0A3904DE8C51DE5FB8ADC1` |
| `src/editor/tests.rs` | `25B7945EAB2D30B30070D3D806BB09C45934D6D65AB694530B1E5B88B7211A21` | `88E6FE2508C3969219293EC55513665ECECC95DDAC436CBC8C375FB4A688A904` |
| `src/editor/view.rs` | `D51ACDDDA922F0EEE1FC387351B1E26ED3B7DD933A0BC1180DB54C4EEAA09B6E` | `DD741F7CBA746951E1046F7F108F314DF667988636C5867EF826EB97A0820E7E` |
| `src/forms/combobox.rs` | `D3AC700B58AB7EB65F50E1D2A175A7E4846F624A751761100FF26AED79F788B6` | `56425AFF9C76722E07F59132D5D0D21461421767E817F4E59CB73FF08C552B62` |
| `src/forms/hotkey.rs` | `915056CA75ECCB264F6E9C520D8A3E6BC748DA81BACD82B2854504F5985A877A` | `01D232E1C5CF8174E7CC57443A35250066F45BB9AFE623489AE89B350DC8DB17` |
| `src/forms/input.rs` | `CDCBED9226FCA9FFE1D49BC38203EAED54DFC2D35957D55626ECA461A78503A1` | `AA8EBB8785F7F31AD7AAADC9F35561FC22826D1FEED870BAC3F72A61051211B0` |
| `src/forms/mod.rs` | `DF209629F492ACC0602F3293002B48FC3EF216F1614C86201FEC64C038DAD20B` | `623AE8489C3C4AA3BB639201AD30D3CB0B7966A8E81C1EF8EFFF9AEE30279A6F` |
| `src/forms/options.rs` | `94569706EF7A3DC0558A8B31C774AAF2788254205CB797ECBB41C5A8DFC621FB` | `F46CEBBA1DB4A113CB8EBCA28F94E9194D946BF9AAE9FAE9043F3C5F0C10AF53` |
| `src/forms/switch.rs` | `7A54F505971388B01F53B49BE99114273AFCEB50120C44C3C5BFD4DE904F953F` | `BA68EB583063CA5227453059196E7E1CF291A6862745C7D25702EB7D1896B4AF` |
| `src/forms/tests/choices.rs` | `73DB654F22DB701544DA062A2FDDB80287C1D1690C746F1808A9038B7C9C3E5A` | `0BCAFF547C2FEE400E533D302EE00F4CB49FE068646961F70D369937C3D43517` |
| `src/forms/text/element.rs` | `D0142351374C8F42335DE5D2EA8E382B809F3373A1E221182A3CA2D365E7E269` | `C407D3945FB3F5D4F46DE085A8C39AE86D271AA18AF3A70FEF13D6E139E5D16C` |
| `src/forms/text/geometry.rs` | `94B31A44767A4B2FD538D46AB13D6D235B208AB63C1A1EB45D0FDBD0152292A3` | `E083D16C256F385EF4FDB9554B270547E9CA9A1D4A74F44BC567FBE01730E754` |
| `src/forms/text/mod.rs` | `681464EB0A0849FD43B71CA4F2DB7AC9F78DFC00BBC999869D67F3E3CC38C448` | `ACC8B241E16B977A0E8BFABFCC32B751C123A61DBA669FC283F3B8CBEBF68CF7` |
| `src/layout/mod.rs` | `401921BB2908446855D31141DBF3618FEA6DA1D884F887A918069A7F47E980A7` | `6A23640F956552739A3B67E24D7FFA665BFE10469FBE5B3502973F4208D9C749` |
| `src/layout/scroll.rs` | `089E2D05A5711CFD03B3F2F8A8584BF786D534C7181631DCCD957763D777E592` | `56897C4185AB845025CBE411158D22A4BF265CA060AD9A5F8FE6337AE85E7439` |
| `src/menus/draw.rs` | `DBEDEFC888FE9DBDEBAEE2211DE15E440A6F0CCBABC13EF7C74CB6E54879DC01` | `8353EA6EED90075A09DD6B0A09CBC3840C71E199C6101516230909D4C1AE4F88` |
| `src/menus/hosts.rs` | `34CE51E4AC2BF0E8C3E2C0E12D8A75B873D1ADDE4D6C084837E40832B6D19810` | `16D6E3FD087E95F6A3176095A382E5D74A0B2B29A342290D18E405BE9FA7D5F9` |
| `src/theme/mod.rs` | `76D48E74AE59A31FAD5A6E0DB5024B3883623BF92EAB68317F5F75FCE8E6606C` | `E03E8676A406BF69415A911DFEE4DB12B68E7C245F569AA266CB52F975AC00D7` |
| `src/theme/syntax.rs` | `DC26812CF3F0459155E98FEC61D58B399A0EE7F340F466414A979C254D2E9EE8` | `391C6CFEEB2C6EDB498F923EBFE4658BB03CA75ADAE6DED31D74EFA96CE09515` |
| `src/theme/tests.rs` | `CB5F1D6A57977467EA16BDD250693B8CBFC97E72DB8A251C492A8D9DC74ED8D7` | `D591FB0FE9F73709DAF54814D7E98AF1D874E8D341CF014FF9E61A2CBED5ECDB` |
| `LICENSE` | `C69028B5957F4DED637DC55A4A412B209C014414ECE49EFEFA60FA811A4C0312` | `C69028B5957F4DED637DC55A4A412B209C014414ECE49EFEFA60FA811A4C0312` |

## Patch 统计与验证

按上述换行归一化内容对 30 个表项逐一比较：29 个局部修改文件均保持与固定 revision 的差异，未发现局部补丁漂移；`LICENSE` 保持预期的零差异。复制边界内缺失文件为 0 个，`original` 与 `local` 哈希表均已与当前固定 revision 和本地副本重算结果一致。

统一 `git diff --no-index --ignore-cr-at-eol --numstat` 统计 29 个局部修改文件，排除上游 Windows checkout 的 CRLF 与本地 rustfmt 的 LF 差异，得到新增 1928 行、删除 275 行，共 2203 行变更。
