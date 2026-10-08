//! Turn 与工具共享的可取消生命周期。

use tokio_util::sync::CancellationToken;

/// 一个可克隆、幂等且能被异步等待的 Turn 取消令牌。
#[derive(Clone, Debug, Default)]
pub struct TurnCancellation {
    /// 由 Tokio 提供并负责向全部子令牌传播取消的底层令牌。
    inner: CancellationToken,
}

impl TurnCancellation {
    /// 创建尚未取消的新令牌。
    pub fn new() -> Self {
        Self::default()
    }

    /// 幂等地取消当前 Turn 及持有其克隆的工具。
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// 返回令牌是否已经取消。
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// 异步等待令牌进入取消状态。
    pub async fn cancelled(&self) {
        self.inner.cancelled().await;
    }

    /// 创建只影响当前 Turn 子工作的子令牌。
    pub fn child_token(&self) -> Self {
        Self {
            inner: self.inner.child_token(),
        }
    }
}
