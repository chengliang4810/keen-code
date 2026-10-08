# 原输入框资源引用的数据契约

来源 UI 的输入框插件 chip 通过标准 `session/prompt` 和 `keencode/session/steer` 的扩展元数据对接 KeenCode。页面组件、DOM、样式和用户正文保持来源实现，资源身份由 Rust 宿主验证并随消息写入权威 Journal。

```json
{
  "sessionId": "session-id",
  "prompt": [{ "type": "text", "text": "@proof 原请求" }],
  "_meta": {
    "keencode/turnId": "turn-id",
    "keencode/messageReferences": [{ "name": "proof", "path": "plugin://proof@local" }]
  }
}
```

- 引用数组最多 32 项，每项只有 `name`、`path` 两个字符串字段。名称最多 128 个 UTF-8 字节，路径最多 2048 个 UTF-8 字节；禁止空白身份、控制字符和重复路径。无选择时省略该扩展。
- 当前宿主只接受准确的 `plugin://插件名@市场名`，显示名必须与插件裸名一致，且正文中存在原输入框序列化的明确引用 token。普通文件、会话引用不作为插件接受。
- 宿主在 admission 和实际启动前重新核对安装目录与启用状态。前端编辑重发预检发生在历史回退之前；预检不替代真正发送的宿主验证。
- 相同 operation/turn 标识不能用相同正文替换插件市场身份。扩展参与幂等摘要及权威消息身份计算。
- `Message.references` 与 `SessionMessage.references` 保存资源选择；原正文不拼接隐藏说明。消息块实时投递和冷回放均携带 `keencode/messageId`、`keencode/messageReferences`，原页面从该元数据恢复 chip。分叉继承同一消息身份及引用。
- 三种模型协议共用 `Message.wire_content()`，在模型请求的用户内容中附加准确资源身份，并计入上下文预算。该附加内容不进入原正文或页面投影，不提高权限；历史摘要输入仍包含权威引用字段。
- 运行中追加消息使用 `keencode/session/steer` 和同一引用扩展。宿主先验证准确身份与启用状态；`UserSteer`、operation digest、队列容量、claim/ack 和冷恢复都保存引用。模型消费信封按序号逐条绑定正文与引用，不合并不同指令的选择；信封保持内部消息属性，原页面不显示协议说明。恢复确认同时核对权威 Transcript 的完整正文和引用，不能只凭水位删除待处理指令。
- 运行中追加沿用当前 Turn 的模型，不重复写入要求会话空闲的模型配置；不同模型选择明确拒绝。插件或附件预检期间若目标回合已经结束，不把同一追加请求改投为普通新回合，原队列保留请求供用户重试。
- 消费信封以 `isMeta` 保持隐藏。已消费追加的原正文与引用随同一原子消费回执保存为 `user_inputs`；实时与冷投递共用标准用户消息块，稳定 `messageId` 为 `<turn>:steer:<sequence>`，并携带 `keencode/startsNewTurn=false`。Steer ACK 返回同一 `messageId`，供原页面绑定乐观消息身份。原文仅用于展示，不重复加入模型 Transcript，不依赖前端持久化事实源。
- 追加前后的回复投影按权威事件顺序分段，分段 ID 保持稳定；整轮开始时间与耗时仍来自宿主。最后一条追加可以作为编辑目标，回退语义仍是归档并删除整个根 Turn。宿主核对最新输入的稳定标识与完整正文，拒绝旧起点、旧追加和正文篡改；中断恢复使用同一锚点，归档保留准确引用。旧空回执不猜测缺失正文。
- 当前不支持携带插件引用的 detached operation admission 或 `/workflow` 请求，明确拒绝，不能静默丢弃资源身份。原 PluginLibrary 入口缺失和其他未接入能力不由该扩展伪装实现。

主要回归保护位于 `core/model/src/tests.rs`、`core/provider/src/tests.rs`、
`core/runtime/src/tests.rs`、`core/agent/src/collaboration_tests.rs` 和
`apps/desktop/src/agent_runtime.rs` 中的内联测试，覆盖引用编码、Journal 冷恢复、
live/replay 和桌面分叉。
