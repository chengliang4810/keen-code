use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{Mutex, MutexGuard},
    task::{Context, Poll},
};

use futures_core::Stream;

use crate::{
    ModelError, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
    ProviderCapabilities,
};

/// 一次脚本化模型调用将按顺序返回的事件或错误。
#[derive(Clone, Debug)]
pub struct ScriptedReply {
    events: Vec<Result<ModelStreamEvent, ModelError>>,
}

impl ScriptedReply {
    /// 创建一段可以包含中途错误的脚本化事件序列。
    pub fn new(events: Vec<Result<ModelStreamEvent, ModelError>>) -> Self {
        Self { events }
    }

    /// 创建一段所有事件都成功的脚本化事件序列。
    pub fn events(events: impl IntoIterator<Item = ModelStreamEvent>) -> Self {
        Self::new(events.into_iter().map(Ok).collect())
    }
}

/// 用于 Agent Loop 与模型层单元测试的确定性 Provider。
///
/// 每次调用消费一段脚本并记录收到的统一请求，不执行网络访问。
#[derive(Debug)]
pub struct ScriptedProvider {
    capabilities: ProviderCapabilities,
    replies: Mutex<VecDeque<ScriptedReply>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedProvider {
    /// 使用能力快照和按调用顺序排列的脚本创建测试 Provider。
    pub fn new(
        capabilities: ProviderCapabilities,
        replies: impl IntoIterator<Item = ScriptedReply>,
    ) -> Self {
        Self {
            capabilities,
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// 在脚本队尾追加一次调用结果。
    pub fn push_reply(&self, reply: ScriptedReply) -> Result<(), ModelError> {
        lock(&self.replies)?.push_back(reply);
        Ok(())
    }

    /// 返回尚未被模型调用消费的脚本数量。
    pub fn remaining_replies(&self) -> Result<usize, ModelError> {
        Ok(lock(&self.replies)?.len())
    }

    /// 返回已收到请求的独立快照。
    pub fn requests(&self) -> Result<Vec<ModelRequest>, ModelError> {
        Ok(lock(&self.requests)?.clone())
    }
}

impl ModelProvider for ScriptedProvider {
    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    fn stream(&self, request: ModelRequest) -> ModelFuture<'_, Result<ModelStream, ModelError>> {
        let result = (|| {
            request.validate()?;
            lock(&self.requests)?.push(request);
            let reply = lock(&self.replies)?.pop_front().ok_or_else(|| {
                ModelError::ProviderUnavailable {
                    message: "脚本化 Provider 没有剩余响应".to_owned(),
                    status_code: None,
                    retryable: false,
                }
            })?;
            let stream: ModelStream = Box::pin(ScriptedEventStream {
                events: reply.events.into(),
            });
            Ok(stream)
        })();
        Box::pin(async move { result })
    }
}

#[derive(Debug)]
struct ScriptedEventStream {
    events: VecDeque<Result<ModelStreamEvent, ModelError>>,
}

impl Stream for ScriptedEventStream {
    type Item = Result<ModelStreamEvent, ModelError>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.events.pop_front())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, ModelError> {
    mutex.lock().map_err(|_| ModelError::Protocol {
        message: "脚本化 Provider 的测试状态已损坏".to_owned(),
    })
}
