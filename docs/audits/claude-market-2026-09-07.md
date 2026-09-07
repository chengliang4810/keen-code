# 官方插件市场逐条适配核对

核对日期：2026-09-07。结论：**无需一次实现上一份规范缺项的全部内容；应按本快照的真实使用情况排期。** 但“没有显式配置某字段”不能证明默认行为不需要支持，也不能证明插件已经可运行。

## 范围与方法

- 主仓库：[anthropics/claude-plugins-official](https://github.com/anthropics/claude-plugins-official/tree/85cce0381e7860082641b59d961a2b8c368b8b79)，锁定 `85cce0381e7860082641b59d961a2b8c368b8b79`。
- 291 个市场条目全部登记：53 个仓库内目录，238 个外部条目；外部按 220 个不同仓库与提交组合取得源码，最终全部下载成功。计数按市场条目去重，不等于独立实现数量（例如三个 data 条目共用同一实现）。
- 仓库位于 `/tmp/keencode-official-market-audit`，外部静态源码位于 `/tmp/keencode-market-sources`。未安装插件、未执行插件脚本、未调用插件服务。
- 检查实际组件声明、SKILL/command/agent frontmatter、Hook 配置、MCP/LSP 配置；不是搜索 README 关键词来决定支持需求。plugin-dev 的动态命令示例单独标为 example，不计入运行需求；普通文字中的 `!`、Rust 宏和 TypeScript 断言已排除。
- 外部文本快照不包含大于 2 MiB 的文件、二进制/构建目录和归档符号链接；脚本变量扫描限于根 hooks/scripts，所以变量使用数是已发现证据数，不能当作全源码穷尽值。按每个插件生成记录，不宣称逐个完成运行验收。
- 完整字段与 SHA 固定证据见 [JSON 明细](claude-market-2026-09-07.json)。下表“未命中”只表示没有命中这次列举的缺项；依赖安装、凭据、MCP OAuth、操作系统及宿主工具语义仍需独立验证。

## 优先结论

| 优先级 | 真实需求 | 本快照证据与决定 |
| --- | --- | --- |
| P0 | 市场条目与清单合并 | 12 个 LSP 条目内联 lspServers；3 个条目在市场层声明 skills；14 个 strict:false。不能只加载磁盘 plugin.json，先修安装/发现契约 |
| P0 | 插件身份 | 18 个条目与源码清单 name 忽略大小写后仍不一致；当前物化逻辑会拒绝。需确认官方别名规则或报告上游不一致，不能无条件改写第三方清单 |
| P0 | Agent 元数据与模型映射 | 21 个条目声明 Claude 模型名；11 个包含 color。当前只接受 providerId::modelId 且未知字段直接报错。先支持必要映射和无执行含义的展示字段，不必为所有自定义字段建立功能 |
| P1 | 动态 Shell | 7 个：claude-security, coderabbit, commit-commands, outputai, ralph-loop, rootly, ui5。直接影响 commit-commands、ralph-loop 等具体流程 |
| P1 | 异步/条件 Hook | async 6 个、asyncRewake 2 个、if 4 个；当前未支持处理器可导致同插件全部 Hook 被隔离，不能只当作缺少后台优化 |
| P1 | prompt Hook | 2 个：databases-on-aws, paypal；是实际需求，不代表还要同时实现 agent/http/mcp_tool |
| P1 | 额外生命周期 | SessionEnd 8 个、UserPromptExpansion 4 个、PostModelSwitch 3 个、FileChanged 2 个。其他大量事件集中于 dash0 观测插件；不应因此盲目扩大核心产品范围 |
| P1 | 会话上下文接口 | 已发现 transcript_path 7 个、CLAUDE_ENV_FILE 2 个。实际 JSONL 格式也需核对，只有提供一个路径不够 |
| P2 | Skill 高级运行 | fork 2 个、局部 hooks 5 个、模型覆盖 3 个、effort 1 个、paths 1 个；按 rootly、microsoft-docs、datahub-skills 等目标实施 |
| P2 | 小众组件 | settings.json 2 个（agentforce-adlc、spotify-ads-api）、outputStyles 1 个（dominodatalab）、experimental monitors 1 个（convex）。并非所有插件都需要，且 experimental 不等于稳定标准 |
| 暂缓 | 未观察到显式声明 | agent/http/mcp_tool Hook；LSP settings/workspaceFolder/shutdownTimeout/restartOnCrash/diagnostics；Skill 命名 arguments、background、shell；workflows。没有本批证据支撑一次全部实现，后续目标插件或故障证明需要时再补 |

LSP 的基础诊断、进程回收与崩溃处理仍是正常运行质量要求；上面暂缓的是这些高级配置项，并非删除基本可靠性。当前发现 14 个带 LSP 声明的条目，使用的字段只有 command、args、extensionToLanguage、startupTimeout、maxRestarts。

## 上一份缺项清单需要修正的判断

1. “完整标准缺项”不能直接作为开发待办。改为三类：市场里已有真实需求、特定插件才需要、当前没有证据。
2. “完整 YAML”也不能机械套用：pr-review-toolkit 的一个 Agent 和 unity 的四个 Skill 被严格 YAML 解析器拒绝；需核对官方实际容错行为或修正上游文件，不能承诺换一个 YAML 库就完成兼容。
3. ui-theme-designer、ui5、ui5-modernization、ui5-typescript-conversion 在根 plugin.json 放元数据，未放 .claude-plugin/plugin.json；这是上游布局问题待确认，不应偷改插件源码或无依据新增历史兼容路径。
4. receipts 与 session-report 直接读取 ~/.claude/projects 的会话数据；即使标准 Hook 和 Skill 都实现，也不自动变成 KeenCode 会话报表。它们需要数据源适配或标注 Claude 专用。
5. dash0 声明权限/团队事件，Discord/Telegram 等频道插件还涉及外部消息产品能力；这些与 KeenCode 当前非目标冲突。标明支持边界，不为“291 个全绿”引入审批/团队/频道系统。
6. 本次没有验证零差异插件可运行，也没有将统计数当作兼容率。特别是 allowed-tools 预批准含义不能直接照搬为 KeenCode 权限模式。

## 每个插件

S/C/A/H/M/L 分别为发现候选 Skills、Commands、Agents、Hook handlers、MCP servers、LSP servers；数量用于检查源码范围，不是宿主成功加载数量。差异字段有时只是展示元数据的接纳需求，不代表必须开发对应运行功能。

| 插件 | S/C/A/H/M/L | 核对结论 | 已发现差异字段 | 固定版本证据 |
| --- | --- | --- | --- | --- |
| 42crunch-api-security-testing | 5/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/42Crunch-AI/claude-plugins/blob/30287f5e3f122a646d1ac5ca3ab96e130c52a3ad/plugins/api-security-testing/skills/42crunch-api-security-testing/SKILL.md#L2) |
| adobe-for-creativity | 8/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/adobe/skills/blob/1307e2c03b9cd20c49872be8cbdfda7ee9aa8c7e/plugins/creative-cloud/adobe-for-creativity/skills/adobe/SKILL.md#L2) |
| agent-sdk-dev | 0/1/2/0/0/0 | 存在适配或产品边界差异 | agents:model=sonnet, commands:argument-hint | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/agent-sdk-dev/commands/new-sdk-app.md#L3) |
| agentforce-adlc | 3/0/4/2/0/0 | 存在适配或产品边界差异 | agents:skills, manifest:settings | [源码](https://github.com/SalesforceAIResearch/agentforce-adlc/blob/d16d14ac7f817336e21bf9392cf51b6cac6194d8/settings.json#L1) |
| ai-plugins | 1/0/11/4/0/0 | 存在适配或产品边界差异 | agents:model=sonnet | [源码](https://github.com/endorlabs/ai-plugins/blob/2de00883bd2be8b8578b46ab09baf2c8731376de/agents/ai-sast-remediation.md#L11) |
| aikido | 3/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/AikidoSec/aikido-claude-plugin/blob/1353c9d54b387f259f0c04f0ed3408842203c29e/skills/issues/SKILL.md#L2) |
| airtable | 8/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/Airtable/skills/blob/812ee67f1fd3d76fb45ff8df40afaa0448602ba8/plugins/airtable/skills/agent-activity-log/SKILL.md#L2) |
| airwallex-agentos | 5/0/0/0/2/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/airwallex/airwallex-marketplace/blob/d4593e33a9e1177a44b895ca0a1d6e015215736f/plugins/airwallex-agentos/skills/awx-best-practices/SKILL.md#L2) |
| airwallex-dev | 6/0/0/0/1/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/airwallex/airwallex-marketplace/blob/d4593e33a9e1177a44b895ca0a1d6e015215736f/plugins/airwallex-dev/skills/airwallex-ai-provider-card-mit/SKILL.md#L5) |
| aiven | 1/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/aiven/aiven-ai-plugins/blob/634f41d6852957cb9280e9245f2ea6f74645b4f1/skills/aiven-getting-started/SKILL.md#L2) |
| alloydb | 7/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/alloydb/blob/9521dbb4ea2d2236b36dc58bf58609d57a00a679/skills/alloydb-postgres-access-management/SKILL.md#L2) |
| alloydb-omni | 9/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/alloydb-omni/blob/60cd1c8d5659f2527c4f1b096ab73c490aca40a5/skills/alloydb-omni-access-control/SKILL.md#L2) |
| altimate-code | 2/2/0/1/0/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/AltimateAI/altimate-claude-plugin/blob/b7c8f68b3dfd303ab3ccf2f43934098811dfe2aa/plugins/altimate-code/commands/altimate.md#L3) |
| amazon-location-service | 1/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/awslabs/agent-plugins/blob/c65ee436b0db77bb75d380aef6fbdc9b114edf2a/plugins/amazon-location-service/skills/amazon-location-service/SKILL.md#L2) |
| amd-skills | 4/0/0/0/0/0 | 存在适配或产品边界差异 | market:skills-override, market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| amplitude | 36/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/amplitude/mcp-marketplace/blob/7dcd19575c504e6e6270edc32dae5222e3b78bac/plugins/amplitude/skills/add-analytics-instrumentation/SKILL.md#L2) |
| apollo | 4/0/0/0/1/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/apolloio/apollo-mcp-plugin/blob/2adde980e45f421b7e9383d92870455627936bce/skills/analytics/SKILL.md#L5) |
| apollo-skills | 14/0/0/0/1/1 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/apollographql/skills/blob/c288eb80629dd2309eed81f23d693f66a452d043/skills/apollo-client/SKILL.md#L2) |
| appwrite | 11/2/0/0/1/0 | 存在适配或产品边界差异 | commands:disable-model-invocation | [源码](https://github.com/appwrite/claude-plugin/blob/ab3c90b37c95b7068f0c064dc562cf21958e8e19/commands/deploy-function.md#L4) |
| asana | 0/1/0/0/0/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/asana/commands/asana-setup.md#L3) |
| astronomer-data-agents | 34/0/0/5/0/0 | 存在适配或产品边界差异 | hook:async, market:name-mismatch, skills:hooks | [源码](https://github.com/astronomer/agents/blob/dbd9bb79c4f7b7ac9c1cdd2b4b75213c81f423e5/skills/authoring-dags/SKILL.md#L4) |
| atlan | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/atlanhq/agent-toolkit/blob/86bb1ad27f80e189b328333d2271b360ae579f2b/.mcp.json#L4) |
| atlassian | 12/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/atlassian/atlassian-mcp-server/blob/d390144793913360bc0987797c1e0e2ff65ecbf5/skills/capture-tasks-from-meeting-notes/SKILL.md#L2) |
| atlassian-twg-cli | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/atlassian-labs/twg-plugins/blob/c50dc777c48fff8c96accffa5fa7597fab022db6/skills/twg-setup/SKILL.md#L2) |
| atomic-agents | 7/0/2/0/0/0 | 存在适配或产品边界差异 | agents:color, agents:model=sonnet, skills:argument-hint | [源码](https://github.com/BrainBlend-AI/atomic-agents/blob/b15ca449a81278b1c92666bdf9a2e57a817dcacd/claude-plugin/atomic-agents/skills/new-app/SKILL.md#L5) |
| auth0 | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/auth0/agent-skills/blob/d33e1b8ca86a1b74a3f457d327a065c49d7e375a/plugins/auth0/skills/auth0/SKILL.md#L2) |
| aws-agents | 9/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/aws/agent-toolkit-for-aws/blob/10b28af8aa3417eeeac6f1ebb5dd4f470a0c3594/plugins/aws-agents/skills/agents-build/SKILL.md#L2) |
| aws-agents-for-devsecops | 13/9/0/0/1/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/aws/agent-toolkit-for-aws/blob/08ad220e4e9bbc498821ce9360b3dcdf4813121d/plugins/aws-agents-for-devsecops/commands/chat.md#L3) |
| aws-amplify | 1/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/awslabs/agent-plugins/blob/c65ee436b0db77bb75d380aef6fbdc9b114edf2a/plugins/aws-amplify/skills/amplify-workflow/SKILL.md#L2) |
| aws-core | 24/0/0/2/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/aws/agent-toolkit-for-aws/blob/10b28af8aa3417eeeac6f1ebb5dd4f470a0c3594/plugins/aws-core/skills/amazon-bedrock/SKILL.md#L2) |
| aws-data-analytics | 9/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/aws/agent-toolkit-for-aws/blob/08ad220e4e9bbc498821ce9360b3dcdf4813121d/plugins/aws-data-analytics/skills/amazon-opensearch-service/SKILL.md#L2) |
| aws-serverless | 7/0/0/1/1/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/awslabs/agent-plugins/blob/34afdf5005325f17d5da2d1443b87f27a53b0a20/plugins/aws-serverless/skills/aws-lambda/SKILL.md#L4) |
| aws-startup-advisor | 9/0/6/1/4/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/awslabs/startups/blob/7ad465c4d1761d88eb3ec68e78dfcbdfaa003a85/advisor/plugins/aws-startup-advisor/skills/agent-advisor/SKILL.md#L2) |
| aws-transform | 1/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/awslabs/agent-plugins/blob/a35c295c62452468446d3a3fa7e2590cd27474ab/plugins/aws-transform/skills/aws-transform/SKILL.md#L2) |
| azure | 38/0/0/1/1/0 | 存在适配或产品边界差异 | hook-script:transcript_path, skills:argument-hint | [源码](https://github.com/microsoft/azure-skills/blob/2fe0f02665574305657c6e79a619c6b9554e53dc/skills/airunway-aks-setup/SKILL.md#L8) |
| azure-cosmos-db-assistant | 1/3/1/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/AzureCosmosDB/cosmosdb-claude-code-plugin/blob/f1e0498579a9251e5f3179b92d25d6ce3409bae5/skills/cosmosdb-best-practices/SKILL.md#L2) |
| azure-sql-developer | 17/0/0/0/0/0 | 存在适配或产品边界差异 | market:name-mismatch | [源码](https://github.com/microsoft/azure-sql-database-container/blob/2193ed98ef1e6d1005143951d44346aac3cd1bad/.claude-plugin/plugin.json) |
| base44 | 5/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/base44/skills/blob/8641b9467c852b39f6d1ae0108eec6ffe70b204c/skills/base44-cli/SKILL.md#L2) |
| bigdata-com | 27/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/Bigdata-com/bigdata-plugins-marketplace/blob/2a52a001366227e205cfa7565d78e8198dc42fa8/plugins/bigdata-com/skills/bigdata-catalyst-monitor/SKILL.md#L2) |
| bigquery-data-analytics | 3/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/bigquery-data-analytics/blob/632c3c3e6a876908b5cd432e8b018f4774f971ce/skills/bigquery-ai-ml/SKILL.md#L2) |
| boltz | 8/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/boltz-bio/boltz-api-skills/blob/70e480ebb14baecfc4456b49eb8b724611470b7c/plugins/boltz/skills/boltz-check-status/SKILL.md#L2) |
| box | 5/0/0/0/0/0 | 存在适配或产品边界差异 | market:skills-override | [源码](https://github.com/box/box-for-ai/blob/15b5c2a567f43b0c13b14b33aab964d080a444cd/skills/box/SKILL.md#L2) |
| brightdata-plugin | 21/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/brightdata/skills/blob/e825f02fbcd7a89087fd1053a57ddcd45113370f/skills/agent-onboarding/SKILL.md#L2) |
| browser-use | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/browser-use/plugins/blob/a25f0a262928bcbb62071432636d943ed9f2b6aa/browser-use/.mcp.json#L3) |
| buildkite | 6/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/buildkite/skills/blob/a4bb20c6bf9ae535335f782015f65b9975065f27/skills/buildkite-agent-runtime/SKILL.md#L2) |
| canva | 6/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/canva-sdks/canva-skills/blob/b56291ea0a36d0a941e1478b47959be5f1771dee/plugins/canva/skills/brand-check/SKILL.md#L2) |
| carbone-skill | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/carboneio/carbone-skill/blob/25207b3f52b97872312eab997ee5c62cc5c9542d/carbone/SKILL.md#L2) |
| carta-cap-table | 22/0/0/9/0/0 | 存在适配或产品边界差异 | body:CLAUDE_PLUGIN_DATA, hook:event:PostModelSwitch, skills:argument-hint, skills:model=inherit, skills:model=sonnet, skills:when_to_use | [源码](https://github.com/carta/plugins/blob/cf7e25ef8d8ff5ec9f6194eafae9a809f66ffff4/plugins/carta-cap-table/skills/carta-compensation-app/SKILL.md#L16) |
| carta-crm | 25/0/0/7/0/0 | 存在适配或产品边界差异 | hook:event:PostModelSwitch, skills:model=haiku, skills:model=inherit | [源码](https://github.com/carta/plugins/blob/cf7e25ef8d8ff5ec9f6194eafae9a809f66ffff4/plugins/carta-crm/skills/add-company/SKILL.md#L11) |
| carta-investors | 16/0/0/8/0/0 | 存在适配或产品边界差异 | hook:event:PostModelSwitch, skills:argument-hint, skills:model=haiku, skills:model=inherit, skills:model=opus, skills:model=sonnet | [源码](https://github.com/carta/plugins/blob/cf7e25ef8d8ff5ec9f6194eafae9a809f66ffff4/plugins/carta-investors/skills/carta-co-investors/SKILL.md#L13) |
| catalyst-by-zoho | 16/1/0/1/1/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/catalystbyzoho/claude-plugin/blob/9670b79d72fe4923cc845ca0414d009ef3684ccb/commands/switch-dc.md#L3) |
| cds-mcp | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/cap-js/mcp-server/blob/644bf4a132313cf506b2ae11ac51db9123198af3/.mcp.json#L4) |
| chrome-devtools-mcp | 6/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/ChromeDevTools/chrome-devtools-mcp/blob/45f187b1e3202c9f32ddba913be5d68751c3caa3/skills/a11y-debugging/SKILL.md#L2) |
| circle-skills | 18/0/0/0/1/0 | 存在适配或产品边界差异 | market:name-mismatch | [源码](https://github.com/circlefin/skills/blob/26dc09ea0746a038c969c6f197feee1267f834b5/plugins/circle/.claude-plugin/plugin.json) |
| circleback | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/circlebackai/claude-code-plugin/blob/a610634c95ab310accf20a0cabdf0fa7ab784fa3/.mcp.json#L4) |
| ckeditor | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/ckeditor/skills/blob/7fc22e0820b27c00a9207076fce4e49d959b552e/skills/ckeditor/SKILL.md#L2) |
| clangd-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| claude-code-setup | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/claude-code-setup/skills/claude-automation-recommender/SKILL.md#L2) |
| claude-md-management | 1/1/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/claude-md-management/skills/claude-md-improver/SKILL.md#L2) |
| claude-security | 1/0/8/3/0/0 | 存在适配或产品边界差异 | agents:color, agents:effort=low, agents:effort=medium, agents:effort=xhigh, agents:initialPrompt, agents:model=inherit, agents:model=sonnet, body:dynamic-shell, hook:asyncRewake, hook:event:UserPromptExpansion, hook:if | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/claude-security/skills/claude-security/SKILL.md#L32) |
| clickhouse | 2/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/ClickHouse/clickhouse-claude-code-plugin/blob/988a7a39834af9c9e14ec3650365243e2cfc6882/skills/clickhouse-best-practices/SKILL.md#L2) |
| clickhouse-best-practices | 11/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/ClickHouse/agent-skills/blob/5aec3114379671f33b1c502a51d420a0729c8172/skills/chdb-datastore/SKILL.md#L2) |
| cloud-sql-mysql | 4/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/cloud-sql-mysql/blob/066a3737218d7e54ce0b28e25584c1ed446361eb/skills/cloud-sql-mysql-admin/SKILL.md#L2) |
| cloud-sql-postgresql | 8/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/cloud-sql-postgresql/blob/69cf78726668b7b922758b963212fe46744511ac/skills/cloud-sql-postgres-admin/SKILL.md#L2) |
| cloud-sql-sqlserver | 4/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/cloud-sql-sqlserver/blob/9e4e8990a4cd99ac39aac97189f8456a7fd10446/skills/cloud-sql-sqlserver-admin/SKILL.md#L2) |
| cloudflare | 13/2/0/0/1/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/cloudflare/skills/blob/9177f9a0bafc1ab61a0dae8dca57a8eb4d9f636d/commands/build-agent.md#L3) |
| cloudinary | 3/0/0/0/5/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/cloudinary-devs/cloudinary-plugin/blob/86be53409e3fb1c465cd5ff4d1dc937a2860f45c/skills/claimable-cloud/SKILL.md#L2) |
| cockroachdb | 34/0/3/2/3/0 | 存在适配或产品边界差异 | agents:color, agents:model=sonnet | [源码](https://github.com/cockroachdb/claude-plugin/blob/6c96c6394a61f366e8ec1b7cec2281e97507cbff/agents/cockroachdb-dba.md#L4) |
| code-modernization | 0/10/8/0/0/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/code-modernization/commands/modernize-assess.md#L3) |
| code-review | 0/1/0/0/0/0 | 存在适配或产品边界差异 | commands:disable-model-invocation | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/code-review/commands/code-review.md#L4) |
| code-simplifier | 0/0/1/0/0/0 | 存在适配或产品边界差异 | agents:model=opus | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/code-simplifier/agents/code-simplifier.md#L4) |
| coderabbit | 2/1/1/0/0/0 | 存在适配或产品边界差异 | body:dynamic-shell, commands:argument-hint | [源码](https://github.com/coderabbitai/skills/blob/aa49953c4cb2590e35480637b1b6a29cf4187cfa/commands/coderabbit-review.md#L3) |
| codspeed | 2/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/CodSpeedHQ/codspeed/blob/6ecb3c0d0599b1acda9ec60d20f3af78e203f748/skills/codspeed-optimize/SKILL.md#L2) |
| commit-commands | 0/3/0/0/0/0 | 存在适配或产品边界差异 | body:dynamic-shell | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/commit-commands/commands/commit-push-pr.md#L8) |
| confidence | 14/17/0/0/2/0 | 存在适配或产品边界差异 | commands:argument-hint, skills:argument-hint | [源码](https://github.com/spotify/confidence-ai-plugins/blob/c8eb4aefefc4e095759ef0e5d1dd7e4e5ae6dae7/skills/analyze-project/SKILL.md#L4) |
| context7 | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/context7/.mcp.json#L4) |
| convex | 17/1/2/4/1/0 | 存在适配或产品边界差异 | commands:argument-hint, commands:disable-model-invocation, manifest:experimental, skills:paths, skills:when_to_use | [源码](https://github.com/get-convex/convex-backend-skill/blob/6ca54f6e2e7582812187b8a5a4783fb4dff52692/.claude-plugin/plugin.json#L21) |
| crowdsec | 2/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/crowdsecurity/crowdsec-skill/blob/517173c3899b165934f06042e3737aae94685b07/skills/crowdsec/SKILL.md#L2) |
| crowdstrike-falcon-foundry | 11/0/0/5/0/0 | 存在适配或产品边界差异 | hook-script:CLAUDE_ENV_FILE | [源码](https://github.com/CrowdStrike/foundry-skills/blob/7f7d46687c16a9a37c9b08ad58ad58999aabe497/hooks/foundry-session-start.sh#L50) |
| crowdstrike-falcon-fusion | 7/0/0/4/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/CrowdStrike/fusion-skills/blob/8df28a78be316f619a0a5227a401b395da990c40/skills/authoring/SKILL.md#L2) |
| csharp-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| cwc-makers | 2/1/0/0/0/0 | 存在适配或产品边界差异 | commands:disable-model-invocation | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/cwc-makers/commands/maker-setup.md#L3) |
| dash0 | 1/2/0/24/0/0 | 存在适配或产品边界差异 | commands:argument-hint, hook:event:ConfigChange, hook:event:CwdChanged, hook:event:Elicitation, hook:event:ElicitationResult, hook:event:FileChanged, hook:event:InstructionsLoaded, hook:event:Notification, hook:event:PermissionDenied, hook:event:PermissionRequest, hook:event:PostCompact, hook:event:PreCompact, hook:event:SessionEnd, hook:event:StopFailure, hook:event:SubagentStart, hook:event:SubagentStop, hook:event:TaskCompleted, hook:event:TaskCreated, hook:event:TeammateIdle, market:name-mismatch | [源码](https://github.com/dash0hq/dash0-agent-plugin/blob/2e2af93006a2cd39693735539cc076026b9a5776/claude/commands/audit-usage.md#L3) |
| data | 34/0/0/5/0/0 | 存在适配或产品边界差异 | hook:async, market:name-mismatch, skills:hooks | [源码](https://github.com/astronomer/agents/blob/dbd9bb79c4f7b7ac9c1cdd2b4b75213c81f423e5/skills/authoring-dags/SKILL.md#L4) |
| data-agent-kit-starter-pack | 29/0/0/1/13/0 | 存在适配或产品边界差异 | market:name-mismatch | [源码](https://github.com/gemini-cli-extensions/data-agent-kit-starter-pack/blob/d8d9081c865256f3a28212387c1a3c77a857a32d/.claude-plugin/plugin.json) |
| data-engineering | 34/0/0/5/0/0 | 存在适配或产品边界差异 | hook:async, market:name-mismatch, skills:hooks | [源码](https://github.com/astronomer/agents/blob/dbd9bb79c4f7b7ac9c1cdd2b4b75213c81f423e5/skills/authoring-dags/SKILL.md#L4) |
| databases-on-aws | 1/0/0/1/3/0 | 存在适配或产品边界差异 | hook:type:prompt | [源码](https://github.com/awslabs/agent-plugins/blob/026f03dc9a2f217ab96aa61f87f10fe8c09e067d/plugins/databases-on-aws/hooks/hooks.json#L8) |
| databricks | 31/2/0/3/0/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/databricks/databricks-agent-skills/blob/ce0599506bad5dd63dead9ab88c440ebd2d8336c/plugins/databricks/claude/commands/doctor.md#L3) |
| datadog | 5/0/0/2/1/0 | 存在适配或产品边界差异 | body:CLAUDE_PLUGIN_DATA, hook:event:SessionEnd | [源码](https://github.com/datadog-labs/claude-code-plugin/blob/195f570c002beab3d7bd342739034d0019aa4863/skills/ddsetup/SKILL.md#L46) |
| datahub-skills | 13/8/4/0/0/0 | 存在适配或产品边界差异 | agents:background, agents:color, agents:model=haiku, agents:model=sonnet, commands:argument-hint, skills:effort=high, skills:effort=low, skills:hooks | [源码](https://github.com/datahub-project/datahub-skills/blob/c6d0ded76eca4c649276e39ab376ad6c66142eb7/skills/datahub-connector-planning/SKILL.md#L6) |
| dataproc | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/dataproc/blob/d78695419d7581f818d289cd347081dd2d66badd/skills/dataproc-skills/SKILL.md#L2) |
| datarobot-agent-skills | 16/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/datarobot-oss/datarobot-agent-skills/blob/e6dddbe327dc2c83e9b7677a5577d33a1063feaf/skills/datarobot-agent-assist/SKILL.md#L2) |
| dataverse | 9/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/microsoft/Dataverse-skills/blob/001b31e0c78e63a0675078ad599e44c16078cd7f/.github/plugins/dataverse/skills/dv-admin/SKILL.md#L2) |
| deepeval | 3/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/confident-ai/deepeval/blob/c144abbce848a6dfbd35bbdaaab49a62bb3fb7b6/skills/deepeval/SKILL.md#L2) |
| deploy-on-aws | 3/0/0/1/3/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/awslabs/agent-plugins/blob/efe040ab66ed7eb4bccc0a94133181da5bc57567/plugins/deploy-on-aws/skills/aws-architecture-diagram/SKILL.md#L4) |
| desktop-commander | 6/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/wonderwhy-er/DesktopCommanderMCP/blob/1eccc8b09cc09805202a1737fd20d605356c3671/plugins/claude/skills/ai-tools-setup/SKILL.md#L2) |
| discord | 2/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/discord/skills/access/SKILL.md#L2) |
| dominodatalab | 23/4/3/0/1/0 | 存在适配或产品边界差异 | agents:model=inherit, agents:skills, manifest:outputStyles | [源码](https://github.com/dominodatalab/domino-claude-plugin/blob/d86698d74d56d3934c8f8ddccb4f6aa55eb2bba7/output-styles#L1) |
| dropbox | 6/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/dropbox/dropbox-ai-plugins/blob/4135e81caf8275b4c97caef244479e0dcb6fb823/claude/skills/clean-up-dropbox-content/SKILL.md#L2) |
| duckdb-skills | 9/0/0/0/0/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/duckdb/duckdb-skills/blob/7feda8e01e22bc0886c86123f3884947e36d8c69/skills/attach-db/SKILL.md#L7) |
| duende-skills | 24/0/2/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/DuendeSoftware/duende-skills/blob/723d77254267621aed00628667e4ee77893af2d4/skills/aspnetcore-authentication/SKILL.md#L2) |
| exa | 2/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/exa-labs/exa-mcp-server/blob/15ffb50519e719dc791cdc750ce5ed1934c0a1ed/skills/exa-agent/SKILL.md#L2) |
| explanatory-output-style | 0/0/0/1/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/explanatory-output-style/hooks/hooks.json#L4) |
| expo | 23/0/0/2/1/0 | 存在适配或产品边界差异 | hook:event:UserPromptExpansion | [源码](https://github.com/expo/skills/blob/80090ccda0ce5973c1354743d1d02602664b15be/plugins/expo/hooks/hooks.json#L15) |
| fakechat | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/fakechat/.mcp.json#L4) |
| fastly-agent-toolkit | 10/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/fastly/fastly-agent-toolkit/blob/42a6070055b2cc08e1f085107685d7eb216b72ed/skills/falco/SKILL.md#L2) |
| feature-dev | 0/1/3/0/0/0 | 存在适配或产品边界差异 | agents:color, agents:model=sonnet, commands:argument-hint | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/feature-dev/commands/feature-dev.md#L3) |
| fiftyone | 18/2/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/voxel51/fiftyone-skills/blob/4fedd6ee7aa88e6e540e31bf54602528c9ca529d/skills/fiftyone-app-playwright/SKILL.md#L2) |
| figma | 14/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/figma/mcp-server-guide/blob/ae7e5e5f80da20f1dd7445e0c6ae5ac58a5b0bce/skills/figma-code-connect/SKILL.md#L2) |
| firebase | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/firebase/.mcp.json#L3) |
| firecrawl | 12/1/0/0/0/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/firecrawl/firecrawl-claude-plugin/blob/b5978f60d8308650821918bf4476fc3b701e88b9/commands/skill-gen.md#L3) |
| firestore-native | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/firestore-native/blob/b65f184dda7aa11a52ca7540fb2e963169a322b9/skills/firestore-data/SKILL.md#L2) |
| forge-skills | 6/0/0/0/2/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/atlassian/forge-skills/blob/294b53da6b20f423be2068596425b499ed8cd730/skills/forge-app-builder/SKILL.md#L2) |
| frontend-design | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/frontend-design/skills/frontend-design/SKILL.md#L2) |
| fullstory | 3/0/1/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/fullstorydev/fullstory-skills/blob/b20614e2d08d7a7c70775bb62b5af640f60b024b/skills/comparisons/SKILL.md#L2) |
| github | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/github/.mcp.json#L3) |
| gitkraken | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gitkraken/claude-plugin/blob/f7a53b2cd138c22eccb977b9d93494ea6d423d12/mcp.json#L4) |
| gitlab | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/gitlab/.mcp.json#L3) |
| google-cloud-storage | 5/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/google-cloud-storage/blob/db08d0fdb8c133ca0cb82e742d9d54228c4a318b/skills/gcs-security-assessment/SKILL.md#L2) |
| gopls-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| grafana-assistant | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/grafana/ai-marketplace/blob/a5c72f2d74c640e9675eb0249526447968535015/plugins/grafana-assistant/skills/grafana-assistant-cli/SKILL.md#L2) |
| grafana-cloud-mcp | 1/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/grafana/ai-marketplace/blob/12be5634a492f73c189d466c5449d09b853ad7a4/plugins/grafana-cloud-mcp/skills/grafana-cloud-mcp-tools/SKILL.md#L2) |
| grafana-mcp | 1/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/grafana/ai-marketplace/blob/a5c72f2d74c640e9675eb0249526447968535015/plugins/grafana-mcp/skills/grafana-mcp-tools/SKILL.md#L2) |
| greptile | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/greptile/.mcp.json#L3) |
| growthbook | 4/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/growthbook/skills/blob/3051b2b1516cceb7eb09b494866789b8d2eb86cc/skills/analytics/SKILL.md#L2) |
| honeycomb | 11/1/2/2/1/0 | 存在适配或产品边界差异 | agents:color, agents:model=inherit | [源码](https://github.com/honeycombio/agent-skill/blob/41214b7dfb97f262adabf295fa6f0fcad85bc0f6/honeycomb/agents/honeycomb-investigator.md#L37) |
| hookify | 1/4/1/4/0/0 | 存在适配或产品边界差异 | agents:color, agents:model=inherit, commands:argument-hint | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/hookify/commands/hookify.md#L3) |
| hostinger | 7/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/hostinger/claude-plugin/blob/569880c60681a7068b3ca8ca84e2fe7ab6cfa7ff/skills/agency-hosting-deploy-php-site/SKILL.md#L2) |
| huggingface-skills | 25/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/huggingface/skills/blob/97862b0fcc89c850fdd00c82ede1e62d3c930a6d/skills/hf-cli/SKILL.md#L2) |
| hunter | 13/0/0/0/1/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/hunter-io/claude-plugin/blob/2a6de2a00f7f459a05c146c6f0712417b8d30db9/skills/build-sequences/SKILL.md#L5) |
| hyperframes | 20/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/heygen-com/hyperframes/blob/19ab83f9294485bfe8bce1eab067c7727a335b95/skills/embedded-captions/SKILL.md#L2) |
| idmp-plugin | 23/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/taosdata/agent-skills/blob/0b67242e2cd114ad9631c39aa20969fdd9780a11/plugins/idmp-plugin/skills/idmp-ai/SKILL.md#L2) |
| imessage | 2/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/imessage/skills/access/SKILL.md#L2) |
| intercom | 4/0/0/0/1/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/intercom/claude-plugin-external/blob/62773a7d4b8aac31545d6888fe6479be3bc53804/skills/customer-360/SKILL.md#L11) |
| jdtls-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| jfrog | 7/0/0/7/1/0 | 存在适配或产品边界差异 | hook:async, hook:event:FileChanged, hook:event:UserPromptExpansion, hook:statusMessage | [源码](https://github.com/jfrog/claude-plugin/blob/41a8da888c3edaa3123336c077118923cb83ad0b/hooks/hooks.json#L10) |
| knowledge-catalog | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/knowledge-catalog/blob/6c92f3d7636d1bddb2ef0bbb05bc4af446845401/skills/knowledge-catalog-discovery/SKILL.md#L2) |
| kotlin-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| langfuse-observability | 0/0/0/2/0/0 | 存在适配或产品边界差异 | hook-script:transcript_path, hook:event:SessionEnd | [源码](https://github.com/langfuse/claude-observability-plugin/blob/169ddfac42a6836f0017f1f8ff1396ff6c67e12f/hooks/hooks.json#L13) |
| laravel-boost | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/laravel-boost/.mcp.json#L3) |
| learn-with-coursera | 1/0/0/0/0/0 | 存在适配或产品边界差异 | market:skills-override, market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| learning-output-style | 0/0/0/1/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/learning-output-style/hooks/hooks.json#L4) |
| legalzoom | 1/1/0/0/1/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/legalzoom/claude-plugins/blob/f9fd8a0ca6e1421bc1aacb113a109663a7a6f6d8/plugins/legalzoom/commands/review-contract.md#L3) |
| linear | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/linear/.mcp.json#L3) |
| liquid-lsp | 0/0/0/0/0/1 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/Shopify/liquid-skills/blob/ae3e4cc3f454923e388bbd841fd931f0c7bf5be4/plugins/liquid-lsp/.lsp.json#L3) |
| liquid-skills | 3/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/Shopify/liquid-skills/blob/ae3e4cc3f454923e388bbd841fd931f0c7bf5be4/plugins/liquid-skills/skills/liquid-theme-a11y/SKILL.md#L2) |
| logfire | 6/4/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/pydantic/skills/blob/9e9390ee24d44b32cf5379c58acaebd7563f5f86/plugins/logfire/skills/logfire-evals/SKILL.md#L2) |
| logrocket | 1/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/LogRocket/logrocket-claude-plugin/blob/2e44dbb47faf9bb54e3405a8ae2714e7c5ce9791/plugins/logrocket/skills/use-logrocket/SKILL.md#L2) |
| looker | 2/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/looker/blob/769a7db04b6a8ad3d8ab3c02e33922b911b0ea2e/skills/looker/SKILL.md#L2) |
| lovable | 0/3/0/0/1/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/lovablelabs/mcp/blob/0336e6db8026b0f02cb89d1451cc48ea3f469791/commands/build.md#L3) |
| lua-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| lumen | 2/0/0/2/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/ory/lumen/blob/f60f9ecee41723b42f3758e9a9409141881ac7d5/skills/doctor/SKILL.md#L2) |
| lusha | 6/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/lusha-oss/lusha-mcp-plugin/blob/ed34947a36754411f03cdeaba91c8cdaa153ef2e/skills/enrich-contact/SKILL.md#L2) |
| mapbox | 20/0/0/0/3/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/mapbox/mapbox-agent-skills/blob/209e8c408fd65edfff45e491c941e8a00025a1a6/skills/mapbox-android-patterns/SKILL.md#L2) |
| math-olympiad | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/math-olympiad/skills/math-olympiad/SKILL.md#L2) |
| mattpocock-skills | 37/0/0/0/0/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/mattpocock/skills/blob/6654f6b60cd9d5be8b54c6fafe44346dabeb3b76/skills/in-progress/claude-handoff/SKILL.md#L4) |
| mcp-apps | 4/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/modelcontextprotocol/ext-apps/blob/fa1274490873f869c9a084e1abb9cf3031d288c7/plugins/mcp-apps/skills/add-app-to-server/SKILL.md#L2) |
| mcp-server-dev | 3/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/mcp-server-dev/skills/build-mcp-app/SKILL.md#L2) |
| mcp-tunnels | 0/1/0/0/0/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/mcp-tunnels/commands/create-docker-mcp-tunnel.md#L3) |
| mercadopago | 4/4/1/2/1/0 | 存在适配或产品边界差异 | agents:category, agents:copyright, agents:license, agents:model=sonnet, agents:tags, agents:version, body:CLAUDE_PROJECT_DIR, commands:argument-hint | [源码](https://github.com/mercadopago/mercadopago-claude-marketplace/blob/c0d06959afc90b2f3d637c35eeb51c36f512c005/plugins/mercadopago/commands/mp-integrate.md#L3) |
| mergify | 6/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/mergifyio/mergify-cli/blob/727ce50b8fb3be8a9a24025807e159d644dbba80/skills/mergify-ci/SKILL.md#L2) |
| microsoft-docs | 3/0/0/0/1/0 | 存在适配或产品边界差异 | skills:context=fork | [源码](https://github.com/MicrosoftDocs/mcp/blob/bb124f19c9304a33e507d0017f0613f8bda3a16c/skills/microsoft-code-reference/SKILL.md#L4) |
| migration-to-aws | 5/0/6/0/4/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/awslabs/startups/blob/f1cc0fb275d45ce053997c7e921b4017f8f40eda/migrate/plugins/migration-to-aws/skills/agent-advisor/SKILL.md#L2) |
| mintlify | 1/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/mintlify/mintlify-claude-plugin/blob/acd6d2e0128c4f235d55cfb8d8c91ecbdd5df8cc/skills/mintlify/SKILL.md#L2) |
| miro | 7/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/miroapp/miro-ai/blob/85c2c7347542b3ce185eb1d2793f8d79ad485c63/claude-plugins/miro/skills/miro-browse/SKILL.md#L2) |
| modern-web-guidance | 2/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/GoogleChrome/modern-web-guidance/blob/56c61c9ee79a8df1a98822309c04847a57f56000/skills/chrome-extensions/SKILL.md#L2) |
| mlflow | 12/0/0/1/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/mlflow/skills/blob/0348e36548b1113d4a17e307acb0e17774cb48c4/agent-evaluation/SKILL.md#L2) |
| monday-crm | 9/0/0/0/1/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/mondaycom/mcp/blob/5eb0835e921de42ba603d7a8315ab4a2575a46b2/plugins/monday-crm/skills/activity-insights/SKILL.md#L5) |
| mongodb | 7/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/mongodb/agent-skills/blob/8ada610346e678b8dc9f866e8166092840c6eb2f/plugins/mongodb/skills/mongodb-atlas-stream-processing/SKILL.md#L2) |
| mongodb-atlas | 6/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/mongodb/agent-skills/blob/8ada610346e678b8dc9f866e8166092840c6eb2f/plugins/mongodb-atlas/skills/mongodb-atlas-stream-processing/SKILL.md#L2) |
| neon | 7/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/neondatabase/agent-skills/blob/4d02514f89ec203ec0f603882f90d468ff8c44f8/plugins/neon-postgres/skills/neon/SKILL.md#L2) |
| netlify-skills | 15/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/netlify/context-and-tools/blob/ee5dd6e5fa0edf8204b8b5e4937fedac61ffe3bf/skills/netlify-access-control/SKILL.md#L2) |
| netsuite-ai-companion | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/oracle/netsuite-suitecloud-sdk/blob/23793a10a9bb557c684c5cb8a97f926eb3463f12/anthropic/netsuite-ai-companion/skills/netsuite-ai-connector-instructions/SKILL.md#L2) |
| netsuite-finance-analyst | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/oracle/netsuite-suitecloud-sdk/blob/23793a10a9bb557c684c5cb8a97f926eb3463f12/anthropic/netsuite-finance-analyst/skills/netsuite-finance-analyst/SKILL.md#L2) |
| netsuite-suitecloud | 8/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/oracle/netsuite-suitecloud-sdk/blob/23793a10a9bb557c684c5cb8a97f926eb3463f12/anthropic/netsuite-suitecloud/skills/netsuite-owasp-secure-coding/SKILL.md#L2) |
| newrelic | 4/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/newrelic/claude-code-plugin/blob/f8e5f8b62139072a22153ec92de1c0cc1af2f56a/skills/apm/SKILL.md#L2) |
| nightvision | 5/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/nvsecurity/nightvision-skills/blob/957db6bb934839275c0f643042101bbb675ddbf7/skills/api-discovery/SKILL.md#L2) |
| nimble | 15/1/2/0/1/0 | 存在适配或产品边界差异 | agents:memory, agents:model=haiku, agents:model=sonnet, agents:skills, commands:argument-hint | [源码](https://github.com/Nimbleway/agent-skills/blob/2890fdf94f0adfae79bf05b4de3c91667701bafb/commands/search.md#L3) |
| noibu | 7/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/Noibu/ai-plugin/blob/5e696fc88ef0ef9324776b6ae05413a296504e7c/src/skills/build-business-context/SKILL.md#L2) |
| notion | 4/10/0/0/1/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/makenotion/claude-code-notion-plugin/blob/9847f2aa1a15f25df35ed1fb7b4557dbb60cd651/commands/create-database-row.md#L3) |
| nvidia-skills | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/NVIDIA/skills/blob/2455514388e984b7e5a78a30e928f39d1401f16b/plugins/nvidia-skills/skills/nvidia-skill-finder/SKILL.md#L2) |
| oracle-ai-data-platform-workbench-databricks-migrator | 10/4/2/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/oracle-samples/oracle-aidp-samples/blob/a88bcf3a9f9acca94663a727de42d8535e869486/ai/claude-code-plugins/oracle-ai-data-platform-workbench-databricks-migrator/skills/aidp-acceptance-contract/SKILL.md#L2) |
| oracle-ai-data-platform-workbench-engineer-agent | 37/0/0/1/0/0 | 存在适配或产品边界差异 | hook:statusMessage | [源码](https://github.com/oracle-samples/oracle-aidp-samples/blob/13e7a9139b3b62172119c7fc1a63bf4a2eac919d/ai/claude-code-plugins/oracle-ai-data-platform-workbench-engineer-agent/hooks/hooks.json#L11) |
| oracle-ai-data-platform-workbench-spark-connectors | 28/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/oracle-samples/oracle-aidp-samples/blob/da0ce42f3b71cc16ee8b3a3e1ed2420c7eb180ad/ai/claude-code-plugins/oracle-ai-data-platform-workbench-spark-connectors/skills/aidp-alh/SKILL.md#L2) |
| oracledb | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/oracledb/blob/74ab5c82e0211f4a0266d1f663051a2ae990b93b/skills/oracledb/SKILL.md#L2) |
| outputai | 50/0/5/1/0/0 | 存在适配或产品边界差异 | agents:color, agents:model=haiku, agents:model=opus, agents:model=sonnet, body:dynamic-shell | [源码](https://github.com/growthxai/output/blob/69255d7fd577139a7828bd219637d067b86a42b1/coding_assistants/claude/plugins/outputai/skills/output-dev-model-selection/SKILL.md#L27) |
| pagerduty | 0/2/0/0/1/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/PagerDuty/claude-code-plugins/blob/761cba75bd50fd561405c3b173ecf36084432089/commands/pre-commit-risk-scoring.md#L3) |
| paypal | 2/5/0/1/1/0 | 存在适配或产品边界差异 | commands:argument-hint, hook:type:prompt, skills:when_to_use | [源码](https://github.com/paypal/AI-Toolkit/blob/310b4962f0e5b3d09ff1c909d96ad5e226703ecb/skills/paypal-best-practices/SKILL.md#L10) |
| php-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| pigment | 12/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gopigment/ai-plugins/blob/ccd2831f48bb9cebb214fff107278129f013bed1/skills/analyzing-pigment-data/SKILL.md#L2) |
| pinecone | 9/1/0/2/1/0 | 存在适配或产品边界差异 | commands:model=claude-haiku-4-5, hook:statusMessage, skills:argument-hint | [源码](https://github.com/pinecone-io/pinecone-claude-code-plugin/blob/db29e063ce51322e962fe477a7cebaf6729a7eed/skills/cli/SKILL.md#L4) |
| pixeltable | 1/2/2/2/0/0 | 存在适配或产品边界差异 | commands:argument-hint | [源码](https://github.com/pixeltable/pixeltable-skill/blob/31af7bdfdbe3edfa81a6797630bfb5b8bb32ac52/commands/add-provider.md#L3) |
| planetscale | 19/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/planetscale/claude-plugin/blob/95c80f9f391be9b1b6172bf82d4191e1ec11b637/database-skills/skills/mysql/SKILL.md#L2) |
| playground | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/playground/skills/playground/SKILL.md#L2) |
| playwright | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/playwright/.mcp.json#L3) |
| plugin-dev | 7/1/3/0/0/0 | 存在适配或产品边界差异 | agents:color, agents:model=inherit, agents:model=sonnet, commands:argument-hint | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/plugin-dev/commands/create-plugin.md#L3) |
| posthog | 164/3/1/2/1/0 | 存在适配或产品边界差异 | commands:argument-hint, hook:event:SessionEnd | [源码](https://github.com/PostHog/ai-plugin/blob/19e4737aa8ab3db315e43f35f796976a1ac37787/commands/llma-cc-ingest.md#L4) |
| postiz | 2/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gitroomhq/postiz-agent/blob/6d3be9c0aec768075e91b4a29b04fb624d72d942/SKILL.md#L2) |
| postman | 11/15/1/0/1/0 | 存在适配或产品边界差异 | agents:allowed-tools, agents:model=sonnet | [源码](https://github.com/Postman-Devrel/postman-claude-code-plugin/blob/27f652ea98d9ae0cec0ccc9f2b46128239a36a8f/agents/readiness-analyzer.md#L4) |
| pr-review-toolkit | 0/1/6/0/0/0 | 需人工确认上游布局/元数据 | agents:color, agents:model=inherit, agents:model=opus, commands:argument-hint；agents/silent-failure-hunter.md: frontmatter mapping values are not allowed here | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/pr-review-toolkit/commands/review-pr.md#L3) |
| preset-cli-skills | 2/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/preset-io/agent-skills/blob/8387ac0f0538271c79a2e227c7ddec084e8a5da3/plugins/preset-cli-skills/skills/preset-cli/SKILL.md#L2) |
| prisma | 0/0/0/0/2/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/prisma/claude-plugin/blob/815dbc4a045a29e3b81510ba0e3ab806f1baaf0e/.mcp.json#L4) |
| project-artifact | 1/0/0/0/0/0 | 存在适配或产品边界差异 | body:CLAUDE_PLUGIN_DATA | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/project-artifact/skills/project-artifact/SKILL.md#L26) |
| pydantic-ai | 1/0/0/0/0/0 | 存在适配或产品边界差异 | market:name-mismatch | [源码](https://github.com/pydantic/skills/blob/e5f7cb13d01561fe3735040ed37596abd9b83976/plugins/ai/.claude-plugin/plugin.json) |
| pyright-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| qdrant-skills | 30/0/0/0/0/0 | 存在适配或产品边界差异 | market:name-mismatch | [源码](https://github.com/qdrant/skills/blob/f90056b7a0c0491d164853eb1e42f952b685fb39/.claude-plugin/plugin.json) |
| qodo | 2/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/qodo-ai/qodo-skills/blob/9c17ca69cfc2f619c8541c328fe5f492f82c99a3/skills/qodo-get-rules/SKILL.md#L2) |
| qt-development-skills | 12/0/0/0/1/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/TheQtCompanyRnD/agent-skills/blob/71d6c10da78b9a764468ae11c86ab3bc4ca4921f/skills/qt-cpp-review/SKILL.md#L19) |
| quarkus-agent | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/quarkusio/quarkus-agent-mcp/blob/4cddd1ff2d49dc04a99b677614ba33014111afb5/.mcp.json#L4) |
| railway | 1/0/0/1/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/railwayapp/railway-skills/blob/5d1e97178f86c82795d6737928bd641e0552166a/plugins/railway/skills/use-railway/SKILL.md#L2) |
| ralph-loop | 0/3/0/1/0/0 | 存在适配或产品边界差异 | body:dynamic-shell, commands:argument-hint, hook-script:transcript_path | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/ralph-loop/commands/ralph-loop.md#L3) |
| rc | 18/0/0/0/1/0 | 存在适配或产品边界差异 | market:name-mismatch | [源码](https://github.com/RevenueCat/rc-claude-code-plugin/blob/b9b77b12da33213c9c2e750b06cd6270c1d8ed65/revenuecat/.claude-plugin/plugin.json) |
| receipts | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/receipts/skills/receipts/SKILL.md#L2) |
| redis-development | 8/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/redis/agent-skills/blob/172fb9effa139cd7432ac29a9ee81c45943e5a28/plugins/redis-development/skills/iris-development/SKILL.md#L2) |
| remember | 1/1/0/4/0/0 | 存在适配或产品边界差异 | hook-script:transcript_path, hook:event:SessionEnd | [源码](https://github.com/Digital-Process-Tools/claude-remember/blob/86fbfccfd0648bb06046753b5358d266374ad69f/hooks/hooks.json#L33) |
| render | 21/2/1/1/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/render-oss/render-plugin-claude-code/blob/e8f889396634dbc8c368448a7f3de993ed4a5ac1/skills/render-background-workers/SKILL.md#L2) |
| resend | 5/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/resend/resend-skills/blob/106dc3435c2bfbd7200d639e9ef6d676d03fcf0b/skills/agent-email-inbox/SKILL.md#L2) |
| revenuecat | 18/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/RevenueCat/rc-claude-code-plugin/blob/b9b77b12da33213c9c2e750b06cd6270c1d8ed65/revenuecat/skills/create-revenuecat-project/SKILL.md#L2) |
| rill | 9/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/rilldata/agent-skills/blob/e58a668dcd48a6cd6fc6d5f552ba8cd59cdfeaa1/skills/rill-analysis/SKILL.md#L2) |
| rootly | 18/0/3/3/1/0 | 存在适配或产品边界差异 | agents:model=sonnet, body:dynamic-shell, hook:if, skills:agent=rootly:deploy-guardian, skills:agent=rootly:incident-investigator, skills:agent=rootly:retro-analyst, skills:argument-hint, skills:context=fork | [源码](https://github.com/Rootly-AI-Labs/rootly-claude-plugin/blob/65832aa6ff7a7b39c6bd64899a7a64646e3948ed/skills/action/SKILL.md#L4) |
| ruby-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| runway-api | 11/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/runwayml/skills/blob/e3dffc15498e9588e7815f37b9ecf10e8bc2c902/skills/runway-dev/SKILL.md#L2) |
| rust-analyzer-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| sagemaker-ai | 19/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/awslabs/agent-plugins/blob/bab56a3b9991aa0c6857b05198a61ba14a60bce4/plugins/sagemaker-ai/skills/dataset-evaluation/SKILL.md#L2) |
| salesforce-development | 37/13/2/34/3/0 | 存在适配或产品边界差异 | commands:argument-hint, hook-script:transcript_path, hook:event:SessionEnd, hook:event:StopFailure, hook:event:UserPromptExpansion, hook:if, hook:statusMessage, manifest:dependencies | [源码](https://github.com/forcedotcom/sf-skills/blob/1a4db263678b5fe740d876e72d1b0ecec724d2f0/plugins/builder/salesforce-development/.claude-plugin/plugin.json#L11) |
| sanity | 7/4/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/sanity-io/agent-toolkit/blob/e447ef10e09f14e245fa787d59157c7ae3576744/skills/content-experimentation-best-practices/SKILL.md#L2) |
| sap-cds-mcp | 0/0/0/0/1/0 | 存在适配或产品边界差异 | market:name-mismatch | [源码](https://github.com/cap-js/mcp-server/blob/e7b9bcd77fcfe15db8705da19a1e0a2582f202a1/.claude-plugin/plugin.json) |
| sap-fiori-mcp-server | 6/0/0/0/1/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/SAP/open-ux-tools/blob/6cfd8d068955287fe9dc764d2acb5315235ec213/packages/fiori-mcp-server/skills/sap-fiori-add-visual-filter/SKILL.md#L4) |
| sap-hana-cli | 0/0/0/0/1/0 | 存在适配或产品边界差异 | market:name-mismatch | [源码](https://github.com/SAP-samples/hana-cli-claude-plugin/blob/abadd0aba32792b6378ed784e9f6d3e5b25dfc2a/.claude-plugin/plugin.json) |
| sap-mdk-server | 0/0/0/0/1/0 | 存在适配或产品边界差异 | market:name-mismatch | [源码](https://github.com/SAP/mdk-mcp-server/blob/b0a4e4f21a6f04d289d62ee72d895756f52422b9/.claude-plugin/plugin.json) |
| save-to-spotify | 2/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/spotify/save-to-spotify/blob/9d05c8cd8c8559552f84e27fe30f46b7693ce69a/plugin/skills/configure-chapter-skip/SKILL.md#L2) |
| scandit-sdk | 86/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/Scandit/skills/blob/2f0731a090eb9c23a64b6dcd9559cf113df3313f/skills/barcode-capture-android/SKILL.md#L2) |
| security-guidance | 0/0/0/9/0/0 | 存在适配或产品边界差异 | hook:asyncRewake, hook:if | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/security-guidance/hooks/hooks.json#L41) |
| semgrep | 1/0/0/4/1/0 | 存在适配或产品边界差异 | hook:async, hook:event:SessionEnd | [源码](https://github.com/semgrep/mcp-marketplace/blob/e606376e8e8c3c6af44023900799192ad70fcb4b/plugin/hooks/hooks.json#L35) |
| sentry | 8/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/getsentry/plugin-claude/blob/73e53541d7af21672e27428c7067f4264b8a3d65/skills/sentry-create-alert/SKILL.md#L2) |
| sentry-cli | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/getsentry/cli/blob/b28cec40a687c7ffb6615c0f594104fb0bac0c11/packages/cli/plugins/sentry-cli/skills/sentry-cli/SKILL.md#L2) |
| serena | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/serena/.mcp.json#L3) |
| servicenow-sdk | 1/0/0/0/0/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/ServiceNow/sdk/blob/f3e0379242b29e0039e1625d637aa1c246acbe12/providers/claude/plugin/skills/now-sdk/SKILL.md#L4) |
| session-report | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/session-report/skills/session-report/SKILL.md#L2) |
| shippo | 9/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/goshippo/ai/blob/15cbb6364fbcfd465a134f5972da9179436d0787/providers/claude/plugin/skills/address-validation/SKILL.md#L2) |
| shopify-ai-toolkit | 21/0/0/23/0/0 | 存在适配或产品边界差异 | hook-script:transcript_path, market:name-mismatch, skills:hooks | [源码](https://github.com/Shopify/Shopify-AI-Toolkit/blob/96a7e79e62cb7b7ff3af02a6f2455986ce479730/skills/shopify-admin/SKILL.md#L8) |
| skill-creator | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/plugins/skill-creator/skills/skill-creator/SKILL.md#L2) |
| slack | 7/5/0/0/1/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/slackapi/slack-mcp-plugin/blob/1251c73cb0ef34d4ce66cada7d045751fc1d1edc/skills/block-kit/SKILL.md#L4) |
| snowflake-cortex-code | 3/0/0/2/0/0 | 存在适配或产品边界差异 | hook-script:transcript_path | [源码](https://github.com/Snowflake-Labs/snowflake-ai-kit/blob/0c54225ea50b44c2ae07e1f378bf401e88b33c33/plugins/cortex-code/scripts/router/test_plugin_units.py#L1046) |
| sonarqube | 9/0/1/1/0/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/SonarSource/sonarqube-agent-plugins/blob/e596969a083cf27bfe439ee2e9459f7ef4124a70/skills/sonar-analyze/SKILL.md#L4) |
| sonatype-guide | 1/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/sonatype/sonatype-guide-claude-plugin/blob/1dae73980f591d3196f5532ac72186513563d028/skills/sonatype-guide/SKILL.md#L2) |
| sourcegraph | 1/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/sourcegraph-community/sourcegraph-claudecode-plugin/blob/332ee0ca9a409ccd791abee43c7abf2606469017/skills/searching-sourcegraph/SKILL.md#L2) |
| spanner | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/gemini-cli-extensions/spanner/blob/290f70f172bb4841a91519b140deb35f97fd4a66/skills/spanner-data/SKILL.md#L2) |
| spotify-ads-api | 19/0/1/1/0/0 | 存在适配或产品边界差异 | agents:color, agents:model=inherit, manifest:settings, skills:argument-hint | [源码](https://github.com/spotify/ads-claude-plugin/blob/aa83ccf1833003959b19ae4faa408d16170bc5cd/settings.json#L1) |
| stackhawk-hawkscan | 1/0/0/4/0/0 | 存在适配或产品边界差异 | hook:async, market:name-mismatch | [源码](https://github.com/stackhawk/agent-skills/blob/19c14577b7e0360262d02f7a232055c67e9c46a6/plugins/hawkscan/hooks/hooks.json#L10) |
| stackhawk-api | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/stackhawk/agent-skills/blob/19c14577b7e0360262d02f7a232055c67e9c46a6/plugins/api/skills/api/SKILL.md#L2) |
| streaming-skills-plugin | 15/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/confluentinc/agent-skills/blob/7babe571ccc1c30895318f4c85d3cc1c07a79cc2/skills/confluent-cloud-cdc-tableflow/SKILL.md#L2) |
| stripe | 8/2/1/0/1/0 | 存在适配或产品边界差异 | agents:allowed-tools, commands:argument-hint | [源码](https://github.com/stripe/ai/blob/453dacd48aac27d751aa3cb7e410256efb3fcfca/providers/claude/plugin/commands/explain-error.md#L3) |
| sumup | 6/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/sumup/sumup-skills/blob/cb72003b417cec5e717c6b788d63394b7e921938/skills/sumup/SKILL.md#L2) |
| supabase | 2/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/supabase-community/supabase-plugin/blob/8629243d1cd72309b533090a4c742f21747d02fa/skills/supabase/SKILL.md#L2) |
| superdesign | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/superdesigndev/superdesign-skill/blob/f9f05cd988c247dce6c072eaf9ac6b162f2ffc4b/skills/superdesign/SKILL.md#L2) |
| superpowers | 14/0/0/1/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/obra/superpowers/blob/b36e0829c6d0140e93cfef2ca599b1b07d4a7797/skills/brainstorming/SKILL.md#L2) |
| swift-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| synthflow | 2/0/0/0/2/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/SynthFlowAI/AnthropicPlugin/blob/205871ee83508502d2c982ee1bb7e65a2a29190b/plugins/synthflow/skills/call-review/SKILL.md#L2) |
| tavily | 8/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/tavily-ai/skills/blob/ea5e8201b0d3ed9c10b70b71187589bd761fe2d2/skills/tavily-best-practices/SKILL.md#L2) |
| teamcity-cli | 2/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/JetBrains/teamcity-cli/blob/0af59a58b17b5cec3750467ac588dd642c8f5e4c/skills/migrate-to-teamcity/SKILL.md#L2) |
| telegram | 2/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/telegram/skills/access/SKILL.md#L2) |
| terraform | 0/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/external_plugins/terraform/.mcp.json#L3) |
| togetherai-skills | 14/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/togethercomputer/skills/blob/644d38225bbdef0318462fe222f6f9883c8addcd/skills/together-audio/SKILL.md#L2) |
| twilio-developer-kit | 57/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/twilio/ai/blob/8aba46fb65dc8d9a20f4b301a68352064b4159a5/skills/sendgrid/twilio-sendgrid-account-setup/SKILL.md#L2) |
| typescript-lsp | 0/0/0/0/0/1 | 存在适配或产品边界差异 | market:strict-false | [源码](https://github.com/anthropics/claude-plugins-official/blob/85cce0381e7860082641b59d961a2b8c368b8b79/.claude-plugin/marketplace.json) |
| ui-theme-designer | 2/0/0/0/0/0 | 需人工确认上游布局/元数据 | 未命中列举缺项；nonstandard root plugin.json exists; not treated as .claude-plugin manifest | [源码](https://github.com/SAP/ui-theme-designer-plugins-for-coding-agents/blob/4e30f5750f760cca24a898c3a6daa8eebfa060a0/plugins/ui-theme-designer/skills/ui-theme-designer-design-tokens/SKILL.md#L2) |
| ui5 | 8/0/0/0/1/0 | 需人工确认上游布局/元数据 | body:dynamic-shell；nonstandard root plugin.json exists; not treated as .claude-plugin manifest | [源码](https://github.com/UI5/plugins-coding-agents/blob/2b8c4a944e39609214bce7aef3b05260985f0e91/plugins/ui5/skills/ui5-best-practices-accessibility/SKILL.md#L32) |
| ui5-modernization | 19/0/0/0/1/0 | 需人工确认上游布局/元数据 | 未命中列举缺项；nonstandard root plugin.json exists; not treated as .claude-plugin manifest | [源码](https://github.com/UI5/plugins-coding-agents/blob/2b8c4a944e39609214bce7aef3b05260985f0e91/plugins/ui5-modernization/skills/fix-bootstrap-params/SKILL.md#L2) |
| ui5-typescript-conversion | 1/0/0/0/0/0 | 需人工确认上游布局/元数据 | 未命中列举缺项；nonstandard root plugin.json exists; not treated as .claude-plugin manifest | [源码](https://github.com/UI5/plugins-coding-agents/blob/2b8c4a944e39609214bce7aef3b05260985f0e91/plugins/ui5-typescript-conversion/skills/ui5-typescript-conversion/SKILL.md#L2) |
| unity | 29/0/0/0/0/0 | 需人工确认上游布局/元数据 | 未命中列举缺项；skills/physics-3d-collision/SKILL.md: frontmatter mapping values are not allowed here; skills/tilemap-ruletile-createfromsegment/SKILL.md: frontmatter mapping values are not allowed here; skills/ui/SKILL.md: frontmatter mapping values are not allowed here; skills/ui-imgui/SKILL.md: frontmatter mapping values are not allowed here | [源码](https://github.com/Unity-Technologies/unity-agent-plugin/blob/1aefde12b046a37b735a856d228eb6d427e3d31e/skills/2d-pixel-perfect/SKILL.md#L2) |
| unreal-engine-skills-for-claude-code | 3/0/0/1/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/EpicGames/unreal-engine-skills-for-claude-code-plugin/blob/7e3b09bbf6d2984c155233f9d3de5fcf523d2d42/skills/create-toolset/SKILL.md#L2) |
| valtown | 11/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/val-town/plugins/blob/2d3ec654b6a7d93e209afb8f2e8848eddcb9f17b/plugin/skills/blob-storage/SKILL.md#L2) |
| vanta | 3/0/0/0/3/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/VantaInc/vanta-mcp-plugin/blob/345d86b55faa649e955b7ea5569cf52d8425c2d5/skills/fix-test/SKILL.md#L3) |
| vanta-mcp-plugin | 3/0/0/0/3/0 | 存在适配或产品边界差异 | market:name-mismatch, skills:argument-hint | [源码](https://github.com/VantaInc/vanta-mcp-plugin/blob/345d86b55faa649e955b7ea5569cf52d8425c2d5/skills/fix-test/SKILL.md#L3) |
| vercel | 44/5/3/4/1/0 | 存在适配或产品边界差异 | hook-script:CLAUDE_ENV_FILE, hook:event:SessionEnd, skills:argument-hint | [源码](https://github.com/vercel/vercel-plugin/blob/11c32588786a9d49791372657433b88d49561874/skills/next-upgrade/upstream/SKILL.md#L4) |
| vibe-prospecting | 1/1/0/0/0/0 | 存在适配或产品边界差异 | market:name-mismatch | [源码](https://github.com/explorium-ai/vibeprospecting-plugin/blob/9b4067473305dbba80be0fa9a492be39b96f455e/.claude-plugin/plugin.json) |
| vsql-extension-builder | 2/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/villagesql/villagesql-skills/blob/ba7bbb6de1b7e66862c61d5cba9c60daec07eb4b/skills/vsql-extension-builder/SKILL.md#L2) |
| windsor-ai | 1/3/1/0/1/0 | 存在适配或产品边界差异 | agents:model=sonnet | [源码](https://github.com/windsor-ai/claude-windsor-ai-plugin/blob/d7ba1cb036c7ca765536355fb85f13a3237ea3f9/agents/business-data-analyst.md#L4) |
| wix | 20/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/wix/skills/blob/238631e91705ee67b337e7d944993c8ed10f298c/skills/wix-app/SKILL.md#L2) |
| build-with-wordpress | 1/3/0/0/0/0 | 存在适配或产品边界差异 | commands:argument-hint, commands:disable-model-invocation | [源码](https://github.com/Automattic/claude-code-wordpress.com/blob/052ca970df2c577d7c651e784935186ff93e6779/commands/design-site.md#L3) |
| workos | 2/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/workos/skills/blob/de1ed17cf03fce2b53973247361a1a47719b528c/plugins/workos/skills/workos/SKILL.md#L2) |
| youdotcom-agent-skills | 5/0/0/0/0/0 | 存在适配或产品边界差异 | market:name-mismatch | [源码](https://github.com/youdotcom-oss/agent-skills/blob/78e20fd40bfdccbf91a5aaa67c2e46ba4ed2bbaa/.claude-plugin/plugin.json) |
| zapier | 4/0/1/0/1/0 | 存在适配或产品边界差异 | agents:target | [源码](https://github.com/zapier/zapier-mcp/blob/217d65a980f9b75536babf89ba64bf03ad95beea/plugins/zapier/agents/zapier-mcp.agent.md#L4) |
| zilliz | 21/2/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/zilliztech/zilliz-plugin/blob/768d3db5fdb69b74116ada2b371032a49bfb3fe1/plugins/zilliz/skills/ask-zilliz/SKILL.md#L2) |
| zoom-plugin | 61/0/0/0/7/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/zoom/zoom-plugin/blob/3c779cd462960e56e43c87be970df846754f6ce7/skills/debug-zoom/SKILL.md#L4) |
| zoominfo | 35/0/0/0/1/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/Zoominfo/zoominfo-mcp-plugin/blob/d07402feb2b9967ccc118f55bacd41a79c822d6a/skills/account-health/SKILL.md#L2) |
| zscaler | 42/20/0/0/1/0 | 存在适配或产品边界差异 | commands:argument-hint, commands:disable-model-invocation | [源码](https://github.com/zscaler/zscaler-mcp-server/blob/809f68d6c921e0829fb2e07e9b797e7e70cf720b/commands/app-health.md#L3) |
| langfuse | 1/0/0/0/0/0 | 未命中本次缺项；未验证运行 | 未命中列举缺项 | [源码](https://github.com/langfuse/skills/blob/1264dc534ffb208f731cef74b681f618c7426e09/skills/langfuse/SKILL.md#L2) |
| zyte-web-data | 15/0/0/0/0/0 | 存在适配或产品边界差异 | skills:argument-hint | [源码](https://github.com/zytedata/claude-skills/blob/8b2d640fe82fadcb69c12af26e4e38ab8ab61ed1/skills/scrape/SKILL.md#L4) |
| activecampaign | 6/5/2/0/1/0 | 存在适配或产品边界差异 | agents:model=opus, commands:argument-hint | [源码](https://github.com/ActiveCampaign/activecampaign-plugin/blob/0ff858728bc52aee335d5475b0d4eb5f3a9589b0/commands/audience-health.md#L3) |

## 本地提交验证

本次既有改动分为内置 Agent、默认网络服务、下载镜像、市场界面、来源声明、格式和 QA 文档共 7 个提交，均未推送。类型检查、1224 项 Vitest、505 项后端测试和 CSS 检查通过。已为本次协议文档、测试和审计证据增加精确路径的来源声明，11 项门禁测试通过；当前工作区来源门禁只剩未跟踪的 .claude/skills/better-ui 与 vendor/peri 两处本地残留，不能写成当前工作区 pnpm test 全通过。原生桌面视觉和 Windows 实机仍未验收。
