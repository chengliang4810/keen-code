use std::{
    collections::{BTreeMap, HashSet},
    future::poll_fn,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ContentBlock, ModelError, ModelResponse, ModelStream, OpaqueReasoningState, ReasoningContent,
    ResponseMetadata, StopReason, TokenUsage, ToolCall,
};

/// Provider Adapter 向 Agent Runtime 输出的统一流事件。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelStreamEvent {
    /// 一个模型响应开始。
    MessageStart {
        /// Provider 在响应开始时报告的响应级元数据。
        metadata: ResponseMetadata,
    },
    /// 普通文本内容的增量。
    TextDelta {
        /// 响应内稳定的内容块序号。
        index: u32,
        /// 按到达顺序追加的 UTF-8 文本。
        delta: String,
    },
    /// 推理内容的增量。
    ReasoningDelta {
        /// 响应内稳定的内容块序号。
        index: u32,
        /// 按到达顺序追加的推理文本。
        delta: String,
    },
    /// 推理摘要的增量。
    ReasoningSummaryDelta {
        /// 与对应推理内容一致的内容块序号。
        index: u32,
        /// 按到达顺序追加的摘要文本。
        delta: String,
    },
    /// 一段推理内容用于后续请求连续性的最终不透明状态。
    ReasoningContinuation {
        /// 与对应推理内容一致的内容块序号。
        index: u32,
        /// Agent Runtime 只能原样持久化和回传的状态。
        continuation: OpaqueReasoningState,
    },
    /// 一次工具调用开始。
    ToolCallStart {
        /// 响应内稳定的内容块序号。
        index: u32,
        /// 响应内唯一的工具调用标识。
        id: String,
        /// 要调用的工具名称。
        name: String,
    },
    /// 工具调用 JSON 参数文本的增量。
    ToolCallArgumentsDelta {
        /// 响应内稳定的内容块序号。
        index: u32,
        /// 与开始事件一致的工具调用标识。
        id: String,
        /// 按到达顺序追加的 JSON 文本片段。
        delta: String,
    },
    /// 一次工具调用的参数传输结束。
    ToolCallEnd {
        /// 响应内稳定的内容块序号。
        index: u32,
        /// 与开始事件一致的工具调用标识。
        id: String,
    },
    /// 当前已知的 Token 用量快照。
    Usage {
        /// 各字段可独立缺失的统一用量。
        usage: TokenUsage,
    },
    /// 模型响应结束。
    MessageEnd {
        /// 模型结束当前响应的统一原因。
        stop_reason: StopReason,
    },
}

#[derive(Debug)]
enum PendingBlock {
    Text(String),
    Reasoning {
        text: String,
        summary: Option<String>,
        continuation: Option<OpaqueReasoningState>,
    },
    Tool {
        id: String,
        name: String,
        arguments: String,
        ended: bool,
    },
}

/// 消费统一模型事件流，并校验事件顺序后生成完整响应。
pub async fn collect_model_stream(mut stream: ModelStream) -> Result<ModelResponse, ModelError> {
    let mut started = false;
    let mut ended = false;
    let mut stop_reason = None;
    let mut metadata = None;
    let mut usage = TokenUsage::unknown();
    let mut blocks = BTreeMap::<u32, PendingBlock>::new();
    let mut tool_call_ids = HashSet::new();

    while let Some(item) = poll_fn(|context| stream.as_mut().poll_next(context)).await {
        let event = item?;
        if ended {
            return Err(protocol_error("响应结束后仍收到事件"));
        }

        match event {
            ModelStreamEvent::MessageStart {
                metadata: response_metadata,
            } => {
                if started {
                    return Err(protocol_error("一个响应只能包含一次开始事件"));
                }
                response_metadata.validate()?;
                metadata = Some(response_metadata);
                started = true;
            }
            ModelStreamEvent::TextDelta { index, delta } => {
                require_started(started)?;
                match blocks
                    .entry(index)
                    .or_insert_with(|| PendingBlock::Text(String::new()))
                {
                    PendingBlock::Text(text) => text.push_str(&delta),
                    PendingBlock::Reasoning { .. } | PendingBlock::Tool { .. } => {
                        return Err(index_type_error(index));
                    }
                }
            }
            ModelStreamEvent::ReasoningDelta { index, delta } => {
                require_started(started)?;
                match blocks
                    .entry(index)
                    .or_insert_with(|| PendingBlock::Reasoning {
                        text: String::new(),
                        summary: None,
                        continuation: None,
                    }) {
                    PendingBlock::Reasoning { text, .. } => text.push_str(&delta),
                    PendingBlock::Text(_) | PendingBlock::Tool { .. } => {
                        return Err(index_type_error(index));
                    }
                }
            }
            ModelStreamEvent::ReasoningSummaryDelta { index, delta } => {
                require_started(started)?;
                match blocks
                    .entry(index)
                    .or_insert_with(|| PendingBlock::Reasoning {
                        text: String::new(),
                        summary: None,
                        continuation: None,
                    }) {
                    PendingBlock::Reasoning { summary, .. } => {
                        summary.get_or_insert_default().push_str(&delta);
                    }
                    PendingBlock::Text(_) | PendingBlock::Tool { .. } => {
                        return Err(index_type_error(index));
                    }
                }
            }
            ModelStreamEvent::ReasoningContinuation {
                index,
                continuation,
            } => {
                require_started(started)?;
                continuation.validate()?;
                match blocks
                    .entry(index)
                    .or_insert_with(|| PendingBlock::Reasoning {
                        text: String::new(),
                        summary: None,
                        continuation: None,
                    }) {
                    PendingBlock::Reasoning {
                        continuation: current,
                        ..
                    } => {
                        if current.replace(continuation).is_some() {
                            return Err(protocol_error(format!(
                                "内容块 {index} 的推理续传状态重复"
                            )));
                        }
                    }
                    PendingBlock::Text(_) | PendingBlock::Tool { .. } => {
                        return Err(index_type_error(index));
                    }
                }
            }
            ModelStreamEvent::ToolCallStart { index, id, name } => {
                require_started(started)?;
                if id.trim().is_empty() || name.trim().is_empty() {
                    return Err(protocol_error("工具调用标识和名称不能为空"));
                }
                if !tool_call_ids.insert(id.clone()) {
                    return Err(protocol_error(format!("工具调用标识 {id} 在响应中重复")));
                }
                if blocks
                    .insert(
                        index,
                        PendingBlock::Tool {
                            id,
                            name,
                            arguments: String::new(),
                            ended: false,
                        },
                    )
                    .is_some()
                {
                    return Err(protocol_error(format!("内容块序号 {index} 重复开始")));
                }
            }
            ModelStreamEvent::ToolCallArgumentsDelta { index, id, delta } => {
                require_started(started)?;
                match blocks.get_mut(&index) {
                    Some(PendingBlock::Tool {
                        id: expected,
                        arguments,
                        ended,
                        ..
                    }) => {
                        if expected != &id {
                            return Err(protocol_error(format!(
                                "内容块 {index} 的工具调用标识不一致"
                            )));
                        }
                        if *ended {
                            return Err(protocol_error(format!(
                                "工具调用 {id} 结束后仍收到参数增量"
                            )));
                        }
                        arguments.push_str(&delta);
                    }
                    Some(PendingBlock::Text(_) | PendingBlock::Reasoning { .. }) => {
                        return Err(index_type_error(index));
                    }
                    None => return Err(protocol_error(format!("工具调用 {id} 尚未开始"))),
                }
            }
            ModelStreamEvent::ToolCallEnd { index, id } => {
                require_started(started)?;
                match blocks.get_mut(&index) {
                    Some(PendingBlock::Tool {
                        id: expected,
                        ended,
                        ..
                    }) => {
                        if expected != &id {
                            return Err(protocol_error(format!(
                                "内容块 {index} 的工具调用标识不一致"
                            )));
                        }
                        if *ended {
                            return Err(protocol_error(format!("工具调用 {id} 重复结束")));
                        }
                        *ended = true;
                    }
                    Some(PendingBlock::Text(_) | PendingBlock::Reasoning { .. }) => {
                        return Err(index_type_error(index));
                    }
                    None => return Err(protocol_error(format!("工具调用 {id} 尚未开始"))),
                }
            }
            ModelStreamEvent::Usage { usage: snapshot } => {
                require_started(started)?;
                usage.update_from(&snapshot);
            }
            ModelStreamEvent::MessageEnd {
                stop_reason: reason,
            } => {
                require_started(started)?;
                ended = true;
                stop_reason = Some(reason);
            }
        }
    }

    if !started {
        return Err(ModelError::StreamInterrupted {
            message: "事件流在响应开始事件之前关闭".to_owned(),
            retryable: true,
        });
    }
    if !ended {
        return Err(ModelError::StreamInterrupted {
            message: "事件流在响应结束事件之前关闭".to_owned(),
            retryable: true,
        });
    }

    let mut content = Vec::with_capacity(blocks.len());
    for (index, block) in blocks {
        let content_block = match block {
            PendingBlock::Text(text) => {
                if text.is_empty() {
                    return Err(protocol_error(format!(
                        "内容块 {index} 的文本不能是空字符串"
                    )));
                }
                ContentBlock::Text { text }
            }
            PendingBlock::Reasoning {
                text,
                summary,
                continuation,
            } => ContentBlock::Reasoning {
                reasoning: ReasoningContent {
                    text,
                    summary,
                    continuation,
                },
            },
            PendingBlock::Tool {
                id,
                name,
                arguments,
                ended,
            } => {
                if !ended {
                    return Err(protocol_error(format!("内容块 {index} 的工具调用未结束")));
                }
                let arguments = if arguments.trim().is_empty() {
                    Value::Object(Default::default())
                } else {
                    serde_json::from_str(&arguments).map_err(|error| {
                        protocol_error(format!("工具调用 {id} 的参数不是有效 JSON：{error}"))
                    })?
                };
                let tool_call = ToolCall::new(id, name, arguments);
                tool_call.validate()?;
                ContentBlock::ToolCall { tool_call }
            }
        };
        content.push(content_block);
    }

    let stop_reason = stop_reason.ok_or_else(|| protocol_error("响应缺少结束原因"))?;
    let metadata = metadata.ok_or_else(|| protocol_error("响应缺少开始元数据"))?;
    let response = ModelResponse::new(metadata, content, usage, stop_reason);
    response.validate()?;
    Ok(response)
}

fn require_started(started: bool) -> Result<(), ModelError> {
    if started {
        Ok(())
    } else {
        Err(protocol_error("响应开始事件之前收到内容事件"))
    }
}

fn index_type_error(index: u32) -> ModelError {
    protocol_error(format!("内容块序号 {index} 被用于不同内容类型"))
}

fn protocol_error(message: impl Into<String>) -> ModelError {
    ModelError::Protocol {
        message: message.into(),
    }
}
