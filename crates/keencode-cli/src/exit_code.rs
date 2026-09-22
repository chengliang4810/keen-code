//! CLI 对外稳定退出码。

/// KeenCode 非交互 CLI 对外承诺的退出码。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ExitCode {
    /// 请求成功。
    Success = 0,
    /// Agent 回合失败，或服务端返回业务失败。
    TaskFailed = 1,
    /// 参数、配置或输入格式错误。
    InvalidArguments = 2,
    /// Host 不可发现、不可连接或协议握手失败。
    HostUnavailable = 3,
    /// 需要认证，但本地 CLI 没有有效凭据。
    AuthenticationFailed = 4,
    /// 用户主动取消当前请求。
    Cancelled = 5,
    /// 非交互请求遇到需要用户回答的交互式询问。
    UserInputRequired = 6,
}

impl ExitCode {
    /// 返回进程级退出整数。
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}
