//! KeenCode 扩展协议的共享常量。
//!
//! `_meta` 键与非标准方法名是桌面 Host 与 headless Host 的共同契约，
//! 统一在此定义，避免两侧各写一份字符串后漂移；新增键时先在协议侧
//! 登记，再由各 Host 引用。

/// `_meta` 中可选的稳定创建/控制操作标识。
pub const META_OPERATION_ID: &str = "keencode/operationId";
/// `_meta` 中可选的精确 Turn 标识。
pub const META_TURN_ID: &str = "keencode/turnId";
/// `_meta` 中可选的本轮 Ultra 开关。
pub const META_ULTRA_MODE: &str = "keencode/ultraMode";
/// Prompt 是否要求客户端断开后继续由 Host 托管；CLI detach 使用该元数据。
pub const META_DETACHED: &str = "keencode/detached";
/// `_meta` 中可选的 Fork 标题。
pub const META_TITLE: &str = "keencode/title";
/// 响应 `_meta` 中的最小 Session 快照键。
pub const META_SNAPSHOT: &str = "keencode/snapshot";
/// 响应 `_meta` 中完整 `session/load` 历史恢复的最终游标事实。
pub const META_REPLAY: &str = "keencode/replay";
/// 初始化响应 `_meta` 中的默认 Session cwd。
pub const META_DEFAULT_CWD: &str = "keencode/defaultCwd";
/// `session/list` 每项 `_meta` 中的最近用户消息时间。
pub const META_LAST_USER_MESSAGE_AT: &str = "keencode/lastUserMessageAt";
/// `session/list` 每项 `_meta` 中的用户置顶标记。
pub const META_SESSION_PINNED: &str = "keencode/pinned";
/// `session/list` 每项 `_meta` 中的用户归档标记。
pub const META_SESSION_ARCHIVED: &str = "keencode/archived";
/// `session/list` 每项 `_meta` 中的标题写入来源。
pub const META_SESSION_TITLE_SOURCE: &str = "keencode/titleSource";

/// 非交互 CLI 在绑定稳定执行身份后即可断开的 admission 方法。
pub const OPERATION_ADMIT_METHOD: &str = "keencode/operation/admit";
/// 跨连接查询 Prompt operation 状态的方法。
pub const OPERATION_STATUS_METHOD: &str = "keencode/operation/status";
/// Web 控制面启动方法；Web server 仍由 Host 所有权和配置决定。
pub const WEB_START_METHOD: &str = "keencode/web/start";
/// Web 控制面停止方法。
pub const WEB_STOP_METHOD: &str = "keencode/web/stop";
/// Web 控制面状态查询方法。
pub const WEB_STATUS_METHOD: &str = "keencode/web/status";
