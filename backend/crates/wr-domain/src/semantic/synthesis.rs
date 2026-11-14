//! C6 双侧聚合与 synthesis(方案 §3.4)。
//!
//! 两个方向共用一个语义事件词汇表,不各写一套:
//!
//! - **upstream events → `SemanticResponse`**:上游 SSE 事件流经
//!   [`decode_upstream_frame`] 变成语义事实,再由 [`StreamAggregator`]
//!   重组为完整响应;缺 terminal 时保留 `incomplete` 而不伪造终止。
//! - **`SemanticResponse` → 客户端事件/帧**:非流上游拿到的完整响应先
//!   展开成语义事件序列,再按客户端协议编码为 SSE 帧,使「客户端流 /
//!   上游非流」与「客户端流 / 上游流」在客户端侧可观察结果一致。
//!
//! 本模块只做语义 ↔ 事件的双向转换与聚合,不做 I/O、不碰 HTTP/SSE
//! framing(那是 `crate::sse` 与网关的事)。

use serde_json::Value;

use crate::provider::ProviderProtocol;
use crate::semantic::error::SemanticError;
use crate::semantic::ir::{ItemKind, StreamKey, StreamUpdate};
use crate::semantic::model::{
    ContentPart, FinishReason, SemanticEvent, SemanticResponse, SourceProtocol,
};
use crate::semantic::stream::{
    StreamAggregator, StreamStatus, StreamViolation, decode_upstream_frame,
};
use crate::sse::SseEvent;

/// 上游事件流的聚合产物:响应 + 终态违规(如果有)。
///
/// 与 [`StreamAggregator::finish`] 的区别是**不丢数据**:断流时返回已聚合
/// 到的部分响应并附带 `Incomplete`/`Failed`,调用方据此决定是否向客户端
/// 报错,而不是把部分内容当成完整成功。
#[derive(Debug, Clone)]
pub struct AggregatedStream {
    pub response: SemanticResponse,
    /// `None` = 正常终结;`Some(_)` = 断流/重复 finish/终态后事件。
    pub violation: Option<StreamViolation>,
}

impl AggregatedStream {
    /// 是否拿到完整、正常终结的响应。
    pub fn is_complete(&self) -> bool {
        self.violation.is_none()
    }
}

/// 上游 SSE 事件 → 语义响应(流式上游聚合)。
///
/// 无论正常还是断流都返回**已收到的部分**;是否完整由
/// [`AggregatedStream::is_complete`] 判定,调用方不得把不完整结果当成功。
pub fn aggregate_upstream_events(
    protocol: ProviderProtocol,
    events: &[SseEvent],
) -> AggregatedStream {
    let mut aggregator = StreamAggregator::new();
    let mut violation = None;
    for event in events {
        let decoded = decode_upstream_frame(protocol, event);
        for semantic in decoded.events {
            if let Err(error) = aggregator.push(semantic) {
                violation.get_or_insert(error);
            }
        }
        if let Some(snapshot) = decoded.snapshot {
            aggregator.merge_snapshot(snapshot.format, &snapshot.value);
        }
    }
    // 终态由 lifecycle 判定:Complete = 正常终结;Failed = 收到 error 事件;
    // 其余(Open/Incomplete)= 缺 terminal 的断流。不做启发式猜测。
    let status = aggregator.lifecycle().status();
    let response = aggregator.into_partial_response();
    let terminal = match status {
        StreamStatus::Complete => None,
        StreamStatus::Failed => Some(StreamViolation::Failed),
        _ => Some(StreamViolation::Incomplete),
    };
    AggregatedStream {
        response,
        violation: violation.or(terminal),
    }
}

/// 完整语义响应 → 语义事件序列(用于「上游非流 → 客户端流」合成)。
///
/// 事件顺序镜像真实流:message_start → (reasoning) → 内容 → 工具调用 →
/// usage → finish → stream_end。incomplete 响应以
/// `StreamEnd { incomplete: true }` 收尾,不合成正常终止。
pub fn response_to_events(response: &SemanticResponse) -> Vec<SemanticEvent> {
    let mut events = vec![SemanticEvent::MessageStart {
        id: response.id.clone(),
        model: response.model.clone(),
    }];
    let mut finished_candidates = Vec::new();
    for candidate in &response.candidates {
        // 目标协议的 block index 在一个候选内全局递增(reasoning → 正文
        // part → 工具调用),避免正文 part 与工具调用的 index 相撞。
        let mut block = 0usize;
        if let Some(reasoning) = &candidate.reasoning {
            events.push(SemanticEvent::ReasoningStart {
                candidate: candidate.index,
                index: block,
            });
            if !reasoning.text.is_empty() {
                events.push(SemanticEvent::ReasoningDelta {
                    candidate: candidate.index,
                    index: block,
                    text: reasoning.text.clone(),
                });
            }
            if let Some(signature) = &reasoning.signature {
                events.push(SemanticEvent::ReasoningSignature {
                    candidate: candidate.index,
                    index: block,
                    signature: signature.clone(),
                });
            }
            events.push(SemanticEvent::PartEnd {
                candidate: candidate.index,
                index: block,
            });
            block += 1;
        }
        let refusal_replaces_text = candidate.finish == Some(FinishReason::Refusal)
            && response
                .refusal
                .as_deref()
                .is_some_and(|text| !text.is_empty())
            && !candidate
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Refusal { .. }));
        for part in &candidate.content {
            let part = if refusal_replaces_text {
                match part {
                    ContentPart::Text { text } => ContentPart::Refusal { text: text.clone() },
                    other => other.clone(),
                }
            } else {
                part.clone()
            };
            match &part {
                ContentPart::Text { text } => {
                    events.push(SemanticEvent::PartStart {
                        candidate: candidate.index,
                        index: block,
                        part: ContentPart::Text {
                            text: String::new(),
                        },
                    });
                    if !text.is_empty() {
                        events.push(SemanticEvent::PartDelta {
                            candidate: candidate.index,
                            index: block,
                            text: text.clone(),
                        });
                    }
                }
                ContentPart::Refusal { text } => {
                    events.push(SemanticEvent::PartStart {
                        candidate: candidate.index,
                        index: block,
                        part: ContentPart::Refusal {
                            text: String::new(),
                        },
                    });
                    if !text.is_empty() {
                        events.push(SemanticEvent::RefusalDelta {
                            candidate: candidate.index,
                            index: block,
                            text: text.clone(),
                        });
                    }
                }
                other => events.push(SemanticEvent::PartStart {
                    candidate: candidate.index,
                    index: block,
                    part: other.clone(),
                }),
            }
            events.push(SemanticEvent::PartEnd {
                candidate: candidate.index,
                index: block,
            });
            block += 1;
        }
        if candidate.content.is_empty()
            && let Some(refusal) = response.refusal.as_deref().filter(|text| !text.is_empty())
        {
            events.push(SemanticEvent::PartStart {
                candidate: candidate.index,
                index: block,
                part: ContentPart::Refusal {
                    text: String::new(),
                },
            });
            events.push(SemanticEvent::RefusalDelta {
                candidate: candidate.index,
                index: block,
                text: refusal.to_owned(),
            });
            events.push(SemanticEvent::PartEnd {
                candidate: candidate.index,
                index: block,
            });
            block += 1;
        }
        for call in &candidate.tool_calls {
            // 契约:`ToolCallStart` 携带完整参数,`ToolArgumentsDelta` 只表示
            // 增量。这里已经拿到完整调用,因此不再补 delta(补了会被聚合
            // 端重复追加)。各客户端编码器据此把参数放在 start 帧里。
            events.push(SemanticEvent::ToolCallStart {
                candidate: candidate.index,
                index: block,
                call: call.clone(),
            });
            events.push(SemanticEvent::PartEnd {
                candidate: candidate.index,
                index: block,
            });
            block += 1;
        }
        if let Some(finish) = &candidate.finish {
            events.push(SemanticEvent::Finish {
                candidate: candidate.index,
                reason: finish.clone(),
            });
            finished_candidates.push(candidate.index);
        }
    }
    if let Some(usage) = response.usage {
        events.push(SemanticEvent::Usage(usage));
    }
    // 响应级 finish 未被任何候选发出时补一条(协议流通常需要 finish)。
    // incomplete 响应不补,以免把断流伪装成正常终止。
    if response.incomplete.is_none()
        && !finished_candidates.contains(&0)
        && let Some(finish) = &response.finish
    {
        events.push(SemanticEvent::Finish {
            candidate: 0,
            reason: finish.clone(),
        });
    }
    events.push(SemanticEvent::StreamEnd {
        incomplete: response.incomplete.is_some(),
    });
    events
}

/// 完整语义响应 → 层级更新序列(Responses 目标合成使用)。
///
/// item 序号按真实输出顺序分配:reasoning → message/parts → tool calls。
/// 文本 part 保留独立开/增量/收口,工具调用保留身份和完整参数。
pub fn response_to_updates(response: &SemanticResponse) -> Vec<StreamUpdate> {
    let mut updates = vec![StreamUpdate::ResponseStart {
        source: SourceProtocol::OpenAiResponses,
        id: response.id.clone(),
        model: response.model.clone(),
    }];
    for candidate in &response.candidates {
        let mut next_item = 0usize;
        if let Some(reasoning) = &candidate.reasoning
            && (!reasoning.text.is_empty() || reasoning.signature.is_some())
        {
            let key = StreamKey::item(candidate.index, next_item);
            updates.push(StreamUpdate::ReasoningStart { key, id: None });
            if !reasoning.text.is_empty() {
                updates.push(StreamUpdate::ReasoningDelta {
                    key,
                    text: reasoning.text.clone(),
                });
            }
            if let Some(signature) = &reasoning.signature {
                updates.push(StreamUpdate::ReasoningSignature {
                    key,
                    signature: signature.clone(),
                });
            }
            updates.push(StreamUpdate::ItemEnd { key });
            next_item += 1;
        }
        let text_item = next_item;
        let mut text_parts = 0usize;
        let refusal_replaces_text = candidate.finish == Some(FinishReason::Refusal)
            && response
                .refusal
                .as_deref()
                .is_some_and(|text| !text.is_empty())
            && !candidate
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Refusal { .. }));
        for part in &candidate.content {
            let part = if refusal_replaces_text {
                match part {
                    ContentPart::Text { text } => ContentPart::Refusal { text: text.clone() },
                    other => other.clone(),
                }
            } else {
                part.clone()
            };
            match &part {
                ContentPart::Text { text } => {
                    let key = StreamKey::new(candidate.index, text_item, text_parts);
                    updates.push(StreamUpdate::PartStart {
                        key,
                        id: None,
                        part: ContentPart::Text {
                            text: String::new(),
                        },
                        extensions: Default::default(),
                    });
                    if !text.is_empty() {
                        updates.push(StreamUpdate::PartDelta {
                            key,
                            text: text.clone(),
                        });
                    }
                    updates.push(StreamUpdate::PartEnd { key });
                    text_parts += 1;
                }
                ContentPart::Refusal { text } => {
                    let key = StreamKey::new(candidate.index, text_item, text_parts);
                    updates.push(StreamUpdate::PartStart {
                        key,
                        id: None,
                        part: ContentPart::Refusal {
                            text: String::new(),
                        },
                        extensions: Default::default(),
                    });
                    if !text.is_empty() {
                        updates.push(StreamUpdate::RefusalDelta {
                            key,
                            text: text.clone(),
                        });
                    }
                    updates.push(StreamUpdate::PartEnd { key });
                    text_parts += 1;
                }
                _ => {}
            }
        }
        if text_parts == 0
            && let Some(refusal) = response.refusal.as_deref().filter(|text| !text.is_empty())
        {
            let key = StreamKey::new(candidate.index, text_item, 0);
            updates.push(StreamUpdate::PartStart {
                key,
                id: None,
                part: ContentPart::Refusal {
                    text: String::new(),
                },
                extensions: Default::default(),
            });
            updates.push(StreamUpdate::RefusalDelta {
                key,
                text: refusal.to_owned(),
            });
            updates.push(StreamUpdate::PartEnd { key });
            text_parts = 1;
        }
        if text_parts > 0 {
            updates.push(StreamUpdate::ItemEnd {
                key: StreamKey::item(candidate.index, text_item),
            });
            next_item += 1;
        }
        for (index, call) in candidate.tool_calls.iter().enumerate() {
            let key = StreamKey::item(candidate.index, next_item + index);
            updates.push(StreamUpdate::ItemStart {
                key,
                id: None,
                kind: ItemKind::ToolCall,
                extensions: Default::default(),
            });
            updates.push(StreamUpdate::ToolStart {
                key,
                tool_index: index,
                call: call.clone(),
            });
            updates.push(StreamUpdate::ItemEnd { key });
        }
        if let Some(finish) = &candidate.finish {
            updates.push(StreamUpdate::Finish {
                candidate: candidate.index,
                reason: finish.clone(),
            });
        }
    }
    if let Some(usage) = response.usage {
        updates.push(StreamUpdate::Usage(usage));
    }
    updates.push(StreamUpdate::End {
        incomplete: response.incomplete.is_some(),
    });
    updates
}

/// `SemanticResponse` → 客户端协议 SSE 帧序列(非流上游 → 客户端流)。
///
/// 与生产流式路径共用 [`crate::semantic::client_stream::ClientStream`] 的
/// 目标编码器;不再维护第二套 `frame_client_event` 帧生成器。
pub fn synthesize_client_frames(
    source: SourceProtocol,
    response: &SemanticResponse,
) -> Result<Vec<String>, SemanticError> {
    let mut client = crate::semantic::client_stream::ClientStream::for_synthesis(source)?;
    client.encode_response(response)
}

/// 客户端协议形态的完整响应 JSON → 客户端 SSE 帧序列。
///
/// 用于 A6 合成支路与响应缓存重放:先把缓存/上游 JSON 解码回语义响应,
/// 再经同一目标编码器生成帧。解码失败显式返回错误,不把整段 JSON 塞进
/// 单个 `data:` 帧冒充合法流。
pub fn synthesize_client_response_frames(
    protocol: &str,
    response: &Value,
) -> Result<Vec<String>, SemanticError> {
    let source = SourceProtocol::parse(protocol).ok_or_else(|| {
        SemanticError::unsupported(format!("未知客户端协议 '{protocol}',无法合成流式帧"))
    })?;
    let decoded = crate::semantic::decode_client_response(source, response)?;
    synthesize_client_frames(source, &decoded)
}

/// `SemanticResponse` → 客户端非流响应 JSON(客户端非流 / 上游流组合)。
///
/// 上游事件经 [`aggregate_upstream_events`] 得到语义响应后,由入口协议
/// 适配器编码回客户端形态;incomplete 响应**不**在这里被当成成功,调用方
/// 需先检查 [`AggregatedStream::is_complete`] 再决定是否用它当 200 响应。
pub fn synthesize_client_response(
    source: SourceProtocol,
    response: &SemanticResponse,
) -> Result<Value, SemanticError> {
    let inbound = crate::semantic::adapter::inbound(source).ok_or_else(|| {
        SemanticError::unsupported(format!("入口协议 {} 无语义适配器", source.as_str()))
    })?;
    Ok(inbound.encode_response(response)?.value)
}

#[cfg(test)]
mod tests;
