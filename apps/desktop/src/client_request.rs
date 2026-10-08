//! Native typed interaction 的 Session 展示串行门。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// 每个 Session 只允许一个可见 typed interaction 的串行门。
pub(crate) struct ClientRequestDisplayGate {
    /// 仅保存弱引用，使没有待决请求的 Session 不会永久占用 Runtime 内存。
    sessions: Mutex<HashMap<String, Weak<Semaphore>>>,
}

impl ClientRequestDisplayGate {
    /// 创建尚未登记任何 Session 的串行门。
    pub(crate) fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// 按 Tokio FIFO 顺序取得指定 Session 的唯一可见请求许可。
    pub(crate) async fn acquire(&self, session_id: &str) -> Option<ClientRequestDisplayPermit> {
        let semaphore = {
            let mut sessions = self.sessions.lock();
            sessions.retain(|_, semaphore| semaphore.strong_count() > 0);
            match sessions.get(session_id).and_then(Weak::upgrade) {
                Some(semaphore) => semaphore,
                None => {
                    let semaphore = Arc::new(Semaphore::new(1));
                    sessions.insert(session_id.to_owned(), Arc::downgrade(&semaphore));
                    semaphore
                }
            }
        };
        semaphore
            .acquire_owned()
            .await
            .map(|permit| ClientRequestDisplayPermit { _permit: permit })
            .ok()
    }
}

impl Default for ClientRequestDisplayGate {
    /// 创建默认的空串行门。
    fn default() -> Self {
        Self::new()
    }
}

/// 一个 typed pending 在完成响应或取消前持有的 Session 独占展示许可。
pub(crate) struct ClientRequestDisplayPermit {
    /// 字段仅通过析构释放信号量许可，不暴露手动解锁能力。
    _permit: OwnedSemaphorePermit,
}
