# 原 Profile 页的 KeenCode 数据接口

页面保持来源基线的 `ProfileSettingsPanel.tsx`、profile 组件及 CSS。原 `stats.getProfileStats` / `stats.getProfileTokenStats` 分别转发到 `ui_profile_stats` / `ui_profile_token_stats`，返回值经原共享契约解码，不增加页面事实源。

- `utcOffsetMinutes` 是客户端本次查询的东向固定 UTC 偏移，范围 -1440 至 1440；不改变日志 UTC 时间。热力图保留原 274 天窗口，按活动日排名生成 0–4 强度；昨天有活动且今天尚未活动时连续天数仍保留。
- 提示数来自已提交的根用户 `MessageAdded` 与根 `UserSteer` 消费回执。内部 meta、子 Agent 委派、模型重试、写作请求不计提示数。复制历史沿用全局消息/回执身份去重；根据持久复制事务的前缀身份保留原输入所属线程，新增分叉输入属于分叉。
- 线程数包含真实会话和用户分叉，排除编辑产生的内部历史副本。项目排名按实际提示归属统计。
- 模型轮次来自用户输入所属 Turn 的 Provider snapshot；Token 使用本地请求记录中的成功、有会话且 `usageReported` 的物理请求，累加 input/output，失败 attempt 和独立写作不计入。缓存和推理量已包含在供应商 input/output 中，不重复相加。不把当前会话模型追溯应用到旧轮次。内部投影保留供应商 ID 与模型的绑定；返回原页面的 model 字段只包含真实模型名，按原 keencode ProviderKind 合并同名模型，避免内部供应商 ID 泄漏和有/无快照请求被拆成重复用量项。模型列表沿用原页面接口的用量降序、名称升序和前八项上限；零用量不占列表位置，但明确报告的零总量仍保持 available，比例分母包含全部模型。
- 未报告 usage 保持 unknown；明确报告 0 保留 0。部分成功请求缺少 usage 时在原页面覆盖提示中保留 `unavailableProviders`。本机自定义供应商均属于原 ProviderKind 的 `keencode`，不虚构官方账户额度，quota 始终 unavailable。
- 插件使用来自消息持久 `plugin://` 引用；技能执行来自成功 `Skill` 工具终态。仅选中但未持久引用或未执行的技能，不宣称已使用。普通子 Agent 不冒充插件 Agent。
- `profile-activity.json` 是 Rust 从权威日志派生的生命周期投影，仅含 ID、时间、模型、项目和技能名，无消息正文、endpoint 或凭据。按需更新，无常驻轮询。`session/delete` 在实际删除前持久该投影，失败则拒绝删除；因此删除不减少累计工作。
- 接入前已经永久删除、且请求记录无法提供提示事实的历史无法恢复；禁止用请求数填补提示数。投影或日志损坏必须报错，不能静默清零覆盖累计值。

验证：Rust `ui_profile::` 的真实 Journal、分叉、编辑、删除与冷读取；前端 NativeApi 的时区转发、契约解码、unknown/0 与错误传播；隔离原生 Profile 页面验收。
