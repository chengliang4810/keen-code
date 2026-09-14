use std::collections::{BTreeMap, HashSet};
use std::ops::{
    Deref, Index, Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive,
};
use std::sync::{Arc, OnceLock};

use serde::de::Deserializer;
use serde::ser::{SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ContentBlock, Message, ModelError, StopReason, StructuredOutputEnforcement, TokenUsage,
    ToolDefinition, structured,
};

/// 请求模型投入推理计算的相对强度。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// 最小推理强度。
    Minimal,
    /// 较低推理强度。
    Low,
    /// 中等推理强度。
    Medium,
    /// 较高推理强度。
    High,
    /// 极高推理强度。
    ExtraHigh,
    /// Provider 明确支持时使用其最大推理强度。
    Maximum,
}

/// Provider 中立的推理请求配置。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningConfig {
    /// 期望的相对推理强度；让端点采用默认值时为 `None`。
    pub effort: Option<ReasoningEffort>,
    /// 期望的最大推理 Token；让端点采用默认值时为 `None`。
    pub max_tokens: Option<u32>,
    /// 是否请求端点返回可展示的推理摘要。
    pub include_summary: bool,
}

impl ReasoningConfig {
    /// 校验显式 Token 预算大于零。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.max_tokens == Some(0) {
            return Err(ModelError::InvalidRequest {
                message: "推理 Token 上限必须大于零".to_owned(),
            });
        }
        Ok(())
    }
}

/// 要求模型最终生成指定 JSON Schema 的配置。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredOutputConfig {
    /// 非空且稳定的结果类型名称。
    pub name: String,
    /// 向模型解释结构化结果用途的可选说明。
    pub description: Option<String>,
    /// 结果必须满足的 JSON Schema 对象。
    pub schema: Value,
    /// 是否请求 Adapter 启用远端原生严格模式；无论该值如何，本地始终严格校验结果。
    pub strict: bool,
}

impl StructuredOutputConfig {
    /// 创建结构化输出配置。
    pub fn new(name: impl Into<String>, schema: Value) -> Self {
        Self {
            name: name.into(),
            description: None,
            schema,
            strict: true,
        }
    }

    /// 校验名称和 Schema 形状。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.name.trim().is_empty() {
            return Err(ModelError::InvalidRequest {
                message: "结构化输出名称不能为空".to_owned(),
            });
        }
        if !self.schema.is_object() {
            return Err(ModelError::InvalidRequest {
                message: "结构化输出 Schema 必须是 JSON 对象".to_owned(),
            });
        }
        structured::validate_schema(&self.schema)
    }

    /// 校验一个已解析 JSON 值满足当前 Schema。
    pub fn validate_value(
        &self,
        value: &Value,
        enforcement: StructuredOutputEnforcement,
    ) -> Result<(), ModelError> {
        self.validate()?;
        structured::validate_value_prechecked(&self.schema, value, enforcement)
    }

    /// 从最终模型响应提取唯一 JSON 值并执行当前 Schema 校验。
    pub fn parse_response(
        &self,
        response: &ModelResponse,
        enforcement: StructuredOutputEnforcement,
    ) -> Result<Value, ModelError> {
        self.validate()?;
        structured::parse_response_prechecked(&self.schema, response, enforcement)
    }
}

/// 模型在当前请求中选择工具的策略。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    /// 由模型决定是否调用工具。
    #[default]
    Auto,
    /// 禁止模型调用任何工具。
    None,
    /// 要求模型至少调用一个工具。
    Required,
    /// 要求模型调用一个指定工具。
    Specific {
        /// 必须调用的工具名称。
        name: String,
    },
}

/// 可在模型 Round 间持久共享、按追加分段增长的消息序列。
///
/// 初始历史、每次提交的新消息段以及请求期前后缀分别持有独立分配。克隆序列只
/// 递增引用计数；追加消息只创建一个新分段，不会复制既有历史。迭代与序列化按
/// `请求前缀 + 初始历史 + 追加段 + 请求后缀` 的逻辑顺序展开。
#[derive(Debug)]
pub struct ModelMessages {
    /// 当前 Turn 开始时取得的历史快照。
    base: Arc<Vec<Message>>,
    /// 最新追加段；通过单向链持久共享此前全部段。
    tail: Option<Arc<MessageSegment>>,
    /// 不进入 Runtime Transcript 的请求期稳定前缀。
    request_prefix: Arc<Vec<Message>>,
    /// 不进入 Runtime Transcript 的请求期动态后缀。
    request_suffix: Arc<Vec<Message>>,
    /// 仅为必须取得连续切片的兼容调用按需物化；普通迭代和序列化不触发。
    contiguous: OnceLock<Vec<Message>>,
}

/// 一段已经提交的新消息；旧快照通过 `previous` 保持不可变。
#[derive(Debug)]
struct MessageSegment {
    previous: Option<Arc<MessageSegment>>,
    messages: Vec<Message>,
    /// 从首个追加段到当前段的消息总数。
    total_len: usize,
}

impl Clone for ModelMessages {
    fn clone(&self) -> Self {
        Self {
            base: Arc::clone(&self.base),
            tail: self.tail.clone(),
            request_prefix: Arc::clone(&self.request_prefix),
            request_suffix: Arc::clone(&self.request_suffix),
            // 连续缓存可能包含完整历史，不能在普通请求克隆时复制。
            contiguous: OnceLock::new(),
        }
    }
}

impl Drop for ModelMessages {
    fn drop(&mut self) {
        // `MessageSegment.previous` is a persistent one-way chain. Taking the tail and
        // unwrapping unique segments one by one keeps destruction off the call stack;
        // a shared segment can be released immediately without touching its chain.
        let mut current = self.tail.take();
        while let Some(segment) = current {
            match Arc::try_unwrap(segment) {
                Ok(segment) => current = segment.previous,
                Err(_) => break,
            }
        }
    }
}

impl Default for ModelMessages {
    fn default() -> Self {
        Self::from(Vec::new())
    }
}

impl From<Vec<Message>> for ModelMessages {
    fn from(messages: Vec<Message>) -> Self {
        Self {
            base: Arc::new(messages),
            tail: None,
            request_prefix: Arc::new(Vec::new()),
            request_suffix: Arc::new(Vec::new()),
            contiguous: OnceLock::new(),
        }
    }
}

impl From<Arc<Vec<Message>>> for ModelMessages {
    fn from(messages: Arc<Vec<Message>>) -> Self {
        Self {
            base: messages,
            tail: None,
            request_prefix: Arc::new(Vec::new()),
            request_suffix: Arc::new(Vec::new()),
            contiguous: OnceLock::new(),
        }
    }
}

impl ModelMessages {
    /// 返回包含请求期前后缀的逻辑消息数。
    pub fn len(&self) -> usize {
        self.request_prefix
            .len()
            .saturating_add(self.base.len())
            .saturating_add(self.tail.as_ref().map_or(0, |tail| tail.total_len))
            .saturating_add(self.request_suffix.len())
    }

    /// 返回逻辑消息序列是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 按模型实际观察顺序迭代消息，不物化或复制历史。
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Message> + ExactSizeIterator + '_ {
        let mut segments = Vec::new();
        if !self.request_prefix.is_empty() {
            segments.push(self.request_prefix.as_slice());
        }
        if !self.base.is_empty() {
            segments.push(self.base.as_slice());
        }
        let tail_start = segments.len();
        let mut current = self.tail.as_deref();
        while let Some(segment) = current {
            segments.push(segment.messages.as_slice());
            current = segment.previous.as_deref();
        }
        segments[tail_start..].reverse();
        if !self.request_suffix.is_empty() {
            segments.push(self.request_suffix.as_slice());
        }
        ModelMessagesIter::new(segments, self.len())
    }

    /// 返回指定逻辑位置的消息，不物化完整历史。
    pub fn get(&self, index: usize) -> Option<&Message> {
        self.iter().nth(index)
    }

    /// 返回最后一条逻辑消息，不物化完整历史。
    pub fn last(&self) -> Option<&Message> {
        self.iter().next_back()
    }

    /// 返回连续消息切片。
    ///
    /// 单一初始段直接借用原分配；存在追加段或请求期上下文时才按需物化一次。
    /// 性能敏感路径应使用 [`ModelMessages::iter`]。
    pub fn as_slice(&self) -> &[Message] {
        self.as_vec().as_slice()
    }

    /// 返回连续消息数组；仅供确实依赖 `Vec` API 的调用。
    fn as_vec(&self) -> &Vec<Message> {
        if self.tail.is_none() && self.request_prefix.is_empty() && self.request_suffix.is_empty() {
            return self.base.as_ref();
        }
        self.contiguous
            .get_or_init(|| self.iter().cloned().collect())
    }

    /// 追加一个已提交消息段；既有历史及其快照保持共享且不发生复制。
    pub fn append(&mut self, messages: Vec<Message>) {
        if messages.is_empty() {
            return;
        }
        let total_len = self
            .tail
            .as_ref()
            .map_or(0, |tail| tail.total_len)
            .saturating_add(messages.len());
        self.tail = Some(Arc::new(MessageSegment {
            previous: self.tail.take(),
            messages,
            total_len,
        }));
        self.contiguous = OnceLock::new();
    }

    /// 设置只属于本次 Provider 请求的前后缀，不修改或复制 Transcript 历史。
    fn set_request_context(&mut self, prefix: Arc<Vec<Message>>, suffix: Arc<Vec<Message>>) {
        self.request_prefix = prefix;
        self.request_suffix = suffix;
        self.contiguous = OnceLock::new();
    }

    /// 把逻辑序列转为可变连续历史。
    ///
    /// 这是任意位置修改所需的兼容慢路径；它会把请求期上下文一并物化。仅追加
    /// 消息时应使用 [`ModelRequest::append_messages`]，避免逐轮复制完整历史。
    fn make_contiguous_mut(&mut self) -> &mut Vec<Message> {
        if self.tail.is_some() || !self.request_prefix.is_empty() || !self.request_suffix.is_empty()
        {
            let flattened = self.iter().cloned().collect::<Vec<_>>();
            *self = Self::from(flattened);
        }
        self.contiguous = OnceLock::new();
        Arc::make_mut(&mut self.base)
    }

    /// 消费序列并返回一个连续共享数组；唯一持有的单段历史保持零拷贝。
    pub fn into_arc(mut self) -> Arc<Vec<Message>> {
        if self.tail.is_none() && self.request_prefix.is_empty() && self.request_suffix.is_empty() {
            return std::mem::take(&mut self.base);
        }
        let base = std::mem::take(&mut self.base);
        let tail = self.tail.take();
        let request_prefix = std::mem::take(&mut self.request_prefix);
        let request_suffix = std::mem::take(&mut self.request_suffix);
        let mut messages = if request_prefix.is_empty() {
            Arc::unwrap_or_clone(base)
        } else {
            let mut prefix = Arc::unwrap_or_clone(request_prefix);
            prefix.extend(Arc::unwrap_or_clone(base));
            prefix
        };
        let mut reversed_segments = Vec::new();
        let mut current = tail;
        while let Some(segment) = current {
            match Arc::try_unwrap(segment) {
                Ok(segment) => {
                    reversed_segments.push(segment.messages);
                    current = segment.previous;
                }
                Err(segment) => {
                    reversed_segments.push(segment.messages.clone());
                    current = segment.previous.clone();
                }
            }
        }
        for segment in reversed_segments.into_iter().rev() {
            messages.extend(segment);
        }
        messages.extend(Arc::unwrap_or_clone(request_suffix));
        Arc::new(messages)
    }
}

/// 跨多个不可变消息段的双向迭代器。
struct ModelMessagesIter<'a> {
    segments: Vec<&'a [Message]>,
    front_segment: usize,
    front_index: usize,
    back_segment: usize,
    back_index: usize,
    remaining: usize,
}

impl<'a> ModelMessagesIter<'a> {
    /// 从按逻辑顺序排列的非空或空消息段创建迭代器。
    fn new(segments: Vec<&'a [Message]>, remaining: usize) -> Self {
        let back_segment = segments.len();
        Self {
            segments,
            front_segment: 0,
            front_index: 0,
            back_segment,
            back_index: 0,
            remaining,
        }
    }
}

impl<'a> Iterator for ModelMessagesIter<'a> {
    type Item = &'a Message;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        while self.front_segment < self.segments.len() {
            let segment = self.segments[self.front_segment];
            if self.front_index < segment.len() {
                let message = &segment[self.front_index];
                self.front_index += 1;
                self.remaining -= 1;
                return Some(message);
            }
            self.front_segment += 1;
            self.front_index = 0;
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl DoubleEndedIterator for ModelMessagesIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        while self.back_segment > 0 {
            let segment_index = self.back_segment - 1;
            let segment = self.segments[segment_index];
            if self.back_index < segment.len() {
                let message = &segment[segment.len() - 1 - self.back_index];
                self.back_index += 1;
                self.remaining -= 1;
                return Some(message);
            }
            self.back_segment -= 1;
            self.back_index = 0;
        }
        None
    }
}

impl ExactSizeIterator for ModelMessagesIter<'_> {}

impl Deref for ModelMessages {
    type Target = Vec<Message>;

    fn deref(&self) -> &Self::Target {
        self.as_vec()
    }
}

impl Index<usize> for ModelMessages {
    type Output = Message;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index)
            .unwrap_or_else(|| panic!("消息下标 {index} 超出长度 {}", self.len()))
    }
}

macro_rules! impl_message_range_index {
    ($range:ty) => {
        impl Index<$range> for ModelMessages {
            type Output = [Message];

            fn index(&self, index: $range) -> &Self::Output {
                &self.as_slice()[index]
            }
        }
    };
}

impl_message_range_index!(Range<usize>);
impl_message_range_index!(RangeFrom<usize>);
impl_message_range_index!(RangeFull);
impl_message_range_index!(RangeInclusive<usize>);
impl_message_range_index!(RangeTo<usize>);
impl_message_range_index!(RangeToInclusive<usize>);

impl AsRef<[Message]> for ModelMessages {
    fn as_ref(&self) -> &[Message] {
        self.as_slice()
    }
}

impl PartialEq for ModelMessages {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl PartialEq<Vec<Message>> for ModelMessages {
    fn eq(&self, other: &Vec<Message>) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl PartialEq<ModelMessages> for Vec<Message> {
    fn eq(&self, other: &ModelMessages) -> bool {
        other == self
    }
}

impl Serialize for ModelMessages {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for message in self.iter() {
            sequence.serialize_element(message)?;
        }
        sequence.end()
    }
}

impl<'de> Deserialize<'de> for ModelMessages {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<Message>::deserialize(deserializer).map(Self::from)
    }
}

/// Agent Runtime 提交给任意模型 Provider 的统一请求。
///
/// `messages` 在 Round 间持久共享：同一 Turn 内多轮请求只共享不可变分段，追加
/// 新消息不会复制完整历史。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequest {
    /// Provider 配置中选择的模型标识。
    pub model: String,
    /// 按对话顺序排列的完整有效消息（跨 Round 持久共享）。
    pub messages: ModelMessages,
    /// 当前调用允许模型使用的工具定义。
    pub tools: Vec<ToolDefinition>,
    /// 当前调用的工具选择策略。
    pub tool_choice: ToolChoice,
    /// 是否允许模型在一个响应中请求多个工具；采用端点默认值时为 `None`。
    pub parallel_tool_calls: Option<bool>,
    /// 推理配置；未启用或未指定时为 `None`。
    pub reasoning: Option<ReasoningConfig>,
    /// 最终结构化输出要求；自由文本响应时为 `None`。
    pub structured_output: Option<StructuredOutputConfig>,
    /// 最大输出 Token；采用模型默认值时为 `None`。
    pub max_output_tokens: Option<u32>,
    /// Provider 中立的采样温度；采用模型默认值时为 `None`。
    pub temperature: Option<f32>,
    /// 模型遇到其中任意文本时应停止继续生成。
    pub stop_sequences: Vec<String>,
    /// 调用元数据；协议 Adapter 只消费显式声明的 `keencode.*` 键（如
    /// `keencode.prompt_cache_key` → Chat Completions wire 字段），其余仅用于追踪。
    pub metadata: BTreeMap<String, String>,
}

impl ModelRequest {
    /// 创建只包含模型和消息的最小请求（消息一次性移入共享存储）。
    ///
    /// `messages` 接受 `Vec<Message>`（一次性移入）或 `Arc<Vec<Message>>`
    /// （引用计数共享，零拷贝）：
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use keencode_model::{Message, MessageRole, ModelRequest};
    ///
    /// let shared: Arc<Vec<Message>> =
    ///     vec![Message::text(MessageRole::User, "你好")].into();
    /// let request = ModelRequest::new("test-model", shared);
    /// assert_eq!(request.messages.len(), 1);
    /// ```
    pub fn new(model: impl Into<String>, messages: impl Into<ModelMessages>) -> Self {
        Self {
            model: model.into(),
            messages: messages.into(),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: None,
            reasoning: None,
            structured_output: None,
            max_output_tokens: None,
            temperature: None,
            stop_sequences: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    /// 返回完整消息数组的可变引用。
    ///
    /// 这是任意位置修改所需的兼容慢路径，共享或分段历史会先物化为独立数组。
    /// 仅追加消息时应使用 [`ModelRequest::append_messages`]。
    pub fn messages_mut(&mut self) -> &mut Vec<Message> {
        self.messages.make_contiguous_mut()
    }

    /// 追加一个消息段，不复制当前请求的既有历史。
    pub fn append_messages(&mut self, messages: Vec<Message>) {
        self.messages.append(messages);
    }

    /// 设置只在本次 Provider 请求中可见的消息前后缀。
    ///
    /// 前后缀与 Transcript 历史分配独立，设置时只共享引用计数；序列化仍输出
    /// 单一 `messages` 数组，不暴露额外协议字段。
    pub fn set_request_message_context(
        &mut self,
        prefix: Arc<Vec<Message>>,
        suffix: Arc<Vec<Message>>,
    ) {
        self.messages.set_request_context(prefix, suffix);
    }

    /// 用新的完整消息列表替换当前消息；旧请求快照保持不变。
    pub fn set_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages.into();
    }

    /// 校验请求满足统一模型层的不变量。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.model.trim().is_empty() {
            return Err(ModelError::InvalidRequest {
                message: "模型标识不能为空".to_owned(),
            });
        }
        if self.messages.is_empty() {
            return Err(ModelError::InvalidRequest {
                message: "模型请求至少需要一条消息".to_owned(),
            });
        }
        for message in self.messages.iter() {
            message.validate()?;
        }

        let mut tool_names = HashSet::with_capacity(self.tools.len());
        for tool in &self.tools {
            tool.validate()?;
            if !tool_names.insert(tool.name.as_str()) {
                return Err(ModelError::InvalidRequest {
                    message: format!("工具名称 {} 在同一请求中重复", tool.name),
                });
            }
        }

        match &self.tool_choice {
            ToolChoice::Specific { name } if name.trim().is_empty() => {
                return Err(ModelError::InvalidRequest {
                    message: "指定工具名称不能为空".to_owned(),
                });
            }
            ToolChoice::Specific { name } if !tool_names.contains(name.as_str()) => {
                return Err(ModelError::InvalidRequest {
                    message: format!("指定工具 {name} 不在当前工具列表中"),
                });
            }
            ToolChoice::Required if self.tools.is_empty() => {
                return Err(ModelError::InvalidRequest {
                    message: "要求调用工具时工具列表不能为空".to_owned(),
                });
            }
            ToolChoice::Auto
            | ToolChoice::None
            | ToolChoice::Required
            | ToolChoice::Specific { .. } => {}
        }

        if let Some(reasoning) = &self.reasoning {
            reasoning.validate()?;
        }
        if let Some(structured_output) = &self.structured_output {
            structured_output.validate()?;
        }
        if self.max_output_tokens == Some(0) {
            return Err(ModelError::InvalidRequest {
                message: "最大输出 Token 必须大于零".to_owned(),
            });
        }
        if let Some(temperature) = self.temperature {
            if !temperature.is_finite() || temperature < 0.0 {
                return Err(ModelError::InvalidRequest {
                    message: "采样温度必须是大于等于零的有限数值".to_owned(),
                });
            }
        }
        if self.stop_sequences.iter().any(|item| item.is_empty()) {
            return Err(ModelError::InvalidRequest {
                message: "停止序列不能为空字符串".to_owned(),
            });
        }
        Ok(())
    }
}

/// 一次完整模型响应的 Provider 中立表示。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseMetadata {
    /// 本机观测的首段输出至响应结束耗时；非流式或未观测时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decode_duration_ms: Option<u64>,
    /// Provider 返回的响应标识；未提供时为 `None`。
    pub response_id: Option<String>,
    /// Provider 实际报告的模型标识；未提供时为 `None`。
    pub model: Option<String>,
}

impl ResponseMetadata {
    /// 校验已报告的响应标识和模型标识均不是空字符串。
    pub fn validate(&self) -> Result<(), ModelError> {
        if self
            .response_id
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ModelError::Protocol {
                message: "Provider 返回的响应标识不能为空".to_owned(),
            });
        }
        if self
            .model
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ModelError::Protocol {
                message: "Provider 返回的模型标识不能为空".to_owned(),
            });
        }
        Ok(())
    }
}

/// 一次完整模型响应的 Provider 中立表示。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelResponse {
    /// 响应级标识和实际模型信息。
    pub metadata: ResponseMetadata,
    /// 保持模型生成顺序的内容块。
    pub content: Vec<ContentBlock>,
    /// 端点报告的 Token 用量；所有字段都可能未知。
    pub usage: TokenUsage,
    /// 模型结束当前响应的原因。
    pub stop_reason: StopReason,
}

impl ModelResponse {
    /// 创建完整模型响应。
    pub fn new(
        metadata: ResponseMetadata,
        content: Vec<ContentBlock>,
        usage: TokenUsage,
        stop_reason: StopReason,
    ) -> Self {
        Self {
            metadata,
            content,
            usage,
            stop_reason,
        }
    }

    /// 校验响应中的每个内容块。
    pub fn validate(&self) -> Result<(), ModelError> {
        self.metadata.validate()?;
        for block in &self.content {
            block.validate()?;
        }
        Ok(())
    }
}
