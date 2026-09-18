# Provider 协议规范文件

存放 KeenCode 支持的三套模型线协议的 OpenAPI 规范快照，供 Adapter 实现、字段核对与回归参考使用。文件为下载快照，不随上游实时更新；引用时注意核对版本。

| 文件 | 协议 | 来源 | 获取方式 |
| --- | --- | --- | --- |
| `anthropic-messages-openapi.json` | Anthropic Messages (`/v1/messages`) | [laszukdawid/anthropic-openapi-spec](https://github.com/laszukdawid/anthropic-openapi-spec) 的 `hosted_spec.json`，内容取自 Anthropic 官方 TypeScript SDK 内置规范（Anthropic 官方未单独发布 OpenAPI 文件） | `curl -L https://raw.githubusercontent.com/laszukdawid/anthropic-openapi-spec/main/hosted_spec.json` |
| `openai-openapi.yaml` | OpenAI Chat Completions 与 Responses | OpenAI 官方仓库 [openai/openai-openapi](https://github.com/openai/openai-openapi)（本快照版本 2.3.0，OpenAPI 3.1） | `curl -L https://raw.githubusercontent.com/openai/openai-openapi/master/openapi.yaml` |
| `openrouter-openapi.json` | OpenRouter Chat 兼容网关 | OpenRouter 官方站点 | `curl -L https://openrouter.ai/openapi.json` |

要点备忘：

- OpenAI 规范中 `max_completion_tokens` 为推荐字段，`max_tokens` 已废弃；OpenRouter 规范对 `max_tokens` 同样标注 deprecated 并提示部分供应商最小值 16。KeenCode 的 `ChatOutputTokenField` 显式二选一，不同时发送。
- Anthropic 官方至今未发布独立 OpenAPI 文件；上述镜像直接提取自官方 SDK，是当前最接近官方的机器可读来源，字段以 Anthropic 官方文档为准。
