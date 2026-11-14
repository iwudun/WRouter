//! 语义流泵(阶段 E1):上游 SSE → [`SemanticEvent`] → 客户端 SSE 帧。
//!
//! 这是网关流式路径的**唯一**受支持转换引擎(阶段 E2 已删除旧
//! `relay.rs` 双栈)。工作方式:
//!
//! 1. [`crate::semantic::stream::outbound_stream`] 把上游事件解码成语义事件,
//!    同时驱动 [`StreamLifecycle`] 完整性门与用量累计;
//! 2. 按客户端协议分派到本模块的客户端状态机
//!    (Chat / Anthropic / Responses / Gemini)重建各自协议的流形状;
//! 3. 无法语义化的入口(embeddings、未知协议)逐事件透传。
//!
//! 与旧 relay 的**有意差异**(基线已随 E2 更新,详见 planning
//! 20261011_01 §10 阶段 E 记录):
//! - Anthropic 客户端不再重复终结序列:旧 relay 对原生透传流会在上游
//!   `message_stop` 之后追加第二组 `message_delta`+`message_stop`,且
//!   `stop_reason` 恒为 `end_turn`,覆盖上游真实原因;泵的终结序列只在
//!   finalize 输出一次,`stop_reason` 来自语义 Finish。
//! - Gemini 客户端收到 Gemini 形状的帧:旧 relay 镜像 Python 把文本
//!   delta 输出成 chat.completion.chunk 形状;泵输出
//!   `candidates[].content.parts[]` / `usageMetadata`。
//! - `gemini_stream` 入口 × OpenAI/Anthropic 上游不再静默 0 事件:请求面
//!   经语义转换(`protocol::upstream_payload`),响应面经本泵回译。
//! - embeddings / 未知客户端协议逐事件透传(旧 relay 对 embeddings 套用
//!   chat chunk 形状,语义上同样不成立)。
//!
//! **终止字段的唯一性**(2026-11-05):整条流里**只能有一个**结束原因
//! (`finish_reason` / `stop_reason` / `finishReason`)。四个客户端状态机
//! 都**记住**上游给的结束原因,收尾时只补「不含终止字段的尾块」
//! (OpenAI 的 `choices: []` + usage)+ 哨兵;上游没给结束原因才兜底补
//! 一个默认值。
//! - 旧实现里 `ChatClient` 不记上游给过什么,收尾**无条件**补一个硬编码
//!   `finish_reason: "stop"` 终块 → 上游给 `tool_calls` 时,客户端看到
//!   `tool_calls` + `stop` 两个**不同**值 → 严格客户端(Qoder CN 等)判
//!   `conflicting_finish_reason` 并拒绝整条流;短文本对话因上游也是
//!   `stop`、两值相同而侥幸正常。
//! - 另三个状态机(Anthropic / Responses / Gemini)当时是对的 —— **只有 Chat
//!   分支漏了**,这个同族不对称本身就是缺陷(见 `docs/LESSONS.md` L-024)。

use std::collections::HashMap;
use std::time::Instant;

use crate::json_py::py_json_dumps;
use crate::provider::client_protocol;
use crate::semantic::ir::{ArgumentReconciliation, reconcile_arguments};
use crate::semantic::model::{
    ContentPart, SemanticEvent, SemanticResponse, SourceProtocol, ToolArguments, ToolCall,
};
use crate::semantic::responses_target::ResponsesTargetClient;
use crate::semantic::source::SourceFacts;
use crate::usage::Usage;

mod anthropic;
mod chat;
mod gemini;

#[cfg(test)]
mod tests;

use anthropic::AnthropicClient;
use chat::ChatClient;
use gemini::GeminiClient;

// ---------------------------------------------------------------------------
// 外壳:协议分派 + 生命周期门 + 用量累计 + model 回写
// ---------------------------------------------------------------------------

fn inner_for_protocol(client_protocol: &str) -> ClientStreamInner {
    if client_protocol == client_protocol::OPENAI_RESPONSES {
        ClientStreamInner::Responses(ResponsesTargetClient::new())
    } else if client_protocol == client_protocol::ANTHROPIC {
        ClientStreamInner::Anthropic(AnthropicClient::new())
    } else if client_protocol::is_gemini(client_protocol) {
        ClientStreamInner::Gemini(GeminiClient::default())
    } else if client_protocol == client_protocol::OPENAI {
        ClientStreamInner::Chat(ChatClient::new())
    } else {
        // embeddings / 未知客户端协议:逐事件透传(语义不可表示)。
        ClientStreamInner::Passthrough
    }
}

/// 构造客户端流泵（每候选一次）。目标编码只依赖客户端协议；上游解释已由
/// [`SourceFacts`] 完成。
pub fn build_client_stream(client_protocol: &str) -> ClientStream {
    ClientStream {
        inner: inner_for_protocol(client_protocol),
        output_model: None,
        // 编码器内部仍保留一份用量缓存:目标协议的终帧需要按目标形状
        // 合成 usage。**权威计量与质量/作用域归 `SourceObserver`**,
        // 这里只服务编码。
        usage: Usage::empty(),
        // 阶段四 C-7:内容推进点测量(10%→90% 跨度 = flow)。
        flow: FlowMeter::default(),
        // 20261108_01 §5.1:答案段 / 思考段独立推进点(吞吐分母与诊断)。
        answer_flow: FlowMeter::default(),
        reasoning_flow: FlowMeter::default(),
        // 20261108_01:是否观察到思考增量(分子能否从 output 中剥离的判断)。
        saw_reasoning: false,
        // 2026-11-06:工具调用账本(去重 + 参数补齐)。
        ledger: ToolCallLedger::default(),
    }
}

impl ClientStream {
    /// 目标编码器(合成/缓存重放):没有上游协议,只消费完整语义响应。
    pub fn for_synthesis(
        protocol: SourceProtocol,
    ) -> Result<Self, crate::semantic::error::SemanticError> {
        if matches!(
            protocol,
            SourceProtocol::OpenAiEmbeddings | SourceProtocol::GeminiInteractions
        ) {
            return Err(crate::semantic::error::SemanticError::unsupported(format!(
                "客户端协议 {} 没有流式合成编码器",
                protocol.as_str()
            )));
        }
        Ok(Self {
            inner: inner_for_protocol(protocol.as_str()),
            output_model: None,
            usage: Usage::empty(),
            flow: FlowMeter::default(),
            answer_flow: FlowMeter::default(),
            reasoning_flow: FlowMeter::default(),
            saw_reasoning: false,
            ledger: ToolCallLedger::default(),
        })
    }

    /// 完整语义响应 → 目标协议帧。与生产流式路径共用同一组目标状态机。
    pub fn encode_response(
        &mut self,
        response: &SemanticResponse,
    ) -> Result<Vec<String>, crate::semantic::error::SemanticError> {
        ensure_synthesizable(response)?;
        let events = crate::semantic::synthesis::response_to_events(response);
        let updates = crate::semantic::synthesis::response_to_updates(response);
        let usage = response
            .usage
            .map(|usage| usage.to_domain())
            .unwrap_or_else(Usage::empty);
        let frames = match &mut self.inner {
            ClientStreamInner::Chat(client) => client.consume(&events, usage),
            ClientStreamInner::Anthropic(client) => client.consume(&events, usage),
            ClientStreamInner::Gemini(client) => client.consume(&events),
            ClientStreamInner::Responses(client) => client.consume(&updates),
            ClientStreamInner::Passthrough => {
                return Err(crate::semantic::error::SemanticError::unsupported(
                    "该客户端协议没有语义流编码器",
                ));
            }
        };
        let mut frames = frames;
        if response.incomplete.is_some() {
            if let ClientStreamInner::Responses(client) = &mut self.inner {
                frames.extend(client.finalize_incomplete());
            }
        } else {
            frames.extend(match &mut self.inner {
                ClientStreamInner::Chat(client) => client.finalize(usage),
                ClientStreamInner::Anthropic(client) => client.finalize(usage),
                ClientStreamInner::Gemini(client) => client.finalize(),
                ClientStreamInner::Responses(client) => client.finalize(),
                ClientStreamInner::Passthrough => Vec::new(),
            });
        }
        Ok(self.note_frames(frames))
    }
}

/// 合成路径只支持能力矩阵声明可表达的内容;未知/多模态输出显式失败,
/// 不能静默丢 part 后返回一条看似正常的流。
fn ensure_synthesizable(
    response: &SemanticResponse,
) -> Result<(), crate::semantic::error::SemanticError> {
    for candidate in &response.candidates {
        for part in &candidate.content {
            match part {
                ContentPart::Text { .. } | ContentPart::Refusal { .. } => {}
                other => {
                    return Err(crate::semantic::error::SemanticError::unsupported(format!(
                        "响应包含无法合成到客户端流的 content part: {}",
                        part_kind(other)
                    )));
                }
            }
        }
    }
    Ok(())
}

fn part_kind(part: &ContentPart) -> &'static str {
    match part {
        ContentPart::Text { .. } => "text",
        ContentPart::Refusal { .. } => "refusal",
        ContentPart::ImageInline { .. } => "image_inline",
        ContentPart::ImageRef { .. } => "image_ref",
        ContentPart::Audio { .. } => "audio",
        ContentPart::File { .. } => "file",
        ContentPart::Opaque { .. } => "opaque",
    }
}

/// 一次编码的产物:客户端帧 + 该批是否包含内容(首字锚点事实)。
///
/// 只有投递层成功入队后才能把 `has_content` 升级为“已提交内容”;
/// 编码器不再自行维护输出/内容标志。
#[derive(Debug, Default)]
pub struct EncodedBatch {
    pub frames: Vec<String>,
    pub has_content: bool,
}

/// 流式输出泵外壳。`output_model` 为 20260905_04 §3.3 的聚合名回写
/// 覆盖(None 时帧不改动)。
///
/// P2 起本类型只负责**目标帧编码**:上游生命周期、usage 质量/作用域、
/// 接收/接受统计与已提交正文投影归 [`crate::semantic::source::SourceObserver`]。
pub struct ClientStream {
    inner: ClientStreamInner,
    output_model: Option<String>,
    usage: Usage,
    /// 阶段四 C-7:内容字节的推进点(10%→90% 跨度测量)。
    flow: FlowMeter,
    /// 20261108_01 §5.1:答案内容(排除 `ReasoningDelta`)的推进点 ——
    /// 答案 TTFT 来源;隐藏思考分支的吞吐分母。
    answer_flow: FlowMeter,
    /// 20261108_01 §5.1:思考内容(`ReasoningDelta`)的推进点 —— 诊断;
    /// 其可测性(是否为空)用于判断思考是否以文本流可见。
    reasoning_flow: FlowMeter,
    /// 20261108_01:本流是否出现过思考增量(含仅有 Start 的加密 reasoning)。
    /// 仅诊断;是否剥离 `reasoning_tokens` 由线格式与 `reasoning_decode_ms`
    /// 决定(见 `usage_store::decode_tps_sample`)。
    saw_reasoning: bool,
    /// 2026-11-06:工具调用账本 —— 记录每个工具调用**已推送给客户端的参数
    /// 长度**,用于「done 帧只作补齐、不重发」。
    ledger: ToolCallLedger,
}

/// 工具调用账本(2026-11-06):记录每个工具调用**已推送给客户端的参数
/// 长度**。
///
/// 用途只有一个 —— 判断「done 帧里的完整参数是该补齐还是该丢弃」:
/// 推过哪怕一个字符就不再进通道(**绝不重发**),一个都没推过才补一次。
/// 解码层(`decode_responses_upstream`)保证「开场只推一次」,这里保证
/// 「参数不重发」—— 两者合起来才是「开场唯一 + 参数唯一」。
#[derive(Default)]
struct ToolCallLedger {
    /// (candidate, 上游 index) → 已接收/已推送参数文本。
    calls: HashMap<(usize, usize), String>,
    conflicts: u64,
}

impl ToolCallLedger {
    fn note_start(&mut self, candidate: usize, index: usize, call: &ToolCall) {
        self.calls.insert(
            (candidate, index),
            opening_arguments(call).unwrap_or_default(),
        );
    }

    fn note_delta(&mut self, candidate: usize, index: usize, fragment: &str) {
        self.calls
            .entry((candidate, index))
            .or_default()
            .push_str(fragment);
    }

    fn reconcile(
        &self,
        candidate: usize,
        index: usize,
        final_arguments: &str,
    ) -> ArgumentReconciliation {
        let received = self
            .calls
            .get(&(candidate, index))
            .map(String::as_str)
            .unwrap_or("");
        reconcile_arguments(received, final_arguments)
    }

    fn note_conflict(&mut self) {
        self.conflicts = self.conflicts.saturating_add(1);
    }
}

impl ClientStream {
    /// 设置输出帧的 model 覆盖名(仅聚合请求调用)。
    pub fn set_output_model(&mut self, model: &str) {
        self.output_model = Some(model.to_owned());
    }

    /// 消费同一份源事实并编码目标帧。
    ///
    /// 事件解释已由 [`SourceFacts`] 完成;本方法只做目标协议状态合成、
    /// 工具参数补齐和帧编码。是否已提交内容由投递层根据
    /// [`EncodedBatch::has_content`] 与实际入队结果判定。
    pub fn consume_facts(&mut self, facts: &SourceFacts) -> EncodedBatch {
        // 20261108_01 §5.1:计时插桩收敛到共享路径 —— 转换与原生 Raw 消费
        // 同一份源事实、同一组内容判据,两路径的 flow / 答案窗口不会漂移。
        self.mark_flow(facts);
        let semantic_events = if matches!(&self.inner, ClientStreamInner::Responses(_)) {
            facts.events.clone()
        } else {
            self.repair_tool_arguments(facts.events.clone())
        };
        let usage = self.usage;
        let frames = match &mut self.inner {
            ClientStreamInner::Chat(client) => client.consume(&semantic_events, usage),
            ClientStreamInner::Anthropic(client) => client.consume(&semantic_events, usage),
            ClientStreamInner::Responses(client) => client.consume(&facts.updates),
            ClientStreamInner::Gemini(client) => client.consume(&semantic_events),
            // 透传协议由管线输出原记录字节,不在这里重组 SSE。
            ClientStreamInner::Passthrough => Vec::new(),
        };
        EncodedBatch {
            frames: self.note_frames(frames),
            has_content: semantic_events.iter().any(is_content_event),
        }
    }

    /// 无信封的透传入口(embeddings/未知协议):管线应输出记录原字节。
    pub fn is_passthrough(&self) -> bool {
        matches!(self.inner, ClientStreamInner::Passthrough)
    }

    /// 共享计时插桩(原生 Raw 与转换路径都经此):用量缓存 + 三套内容
    /// 推进点。
    ///
    /// 三套口径:
    /// - `flow`:全内容(含推理增量)—— 健康度参考;
    /// - `answer_flow`:答案内容(排除 `ReasoningDelta`)—— 吞吐分母与
    ///   答案 TTFT;
    /// - `reasoning_flow`:思考增量 —— 仅诊断。
    ///
    /// 同一批事件共用一个时刻(与上游事件到达时刻同源)。
    pub fn mark_flow(&mut self, facts: &SourceFacts) {
        for event in &facts.events {
            if let SemanticEvent::Usage(update) = event {
                self.usage.merge_max(update.to_domain());
            }
        }
        let mut content_now: Option<Instant> = None;
        for event in &facts.events {
            if matches!(
                event,
                SemanticEvent::ReasoningStart { .. } | SemanticEvent::ReasoningDelta { .. }
            ) {
                self.saw_reasoning = true;
            }
            let bytes = content_bytes(event);
            if bytes == 0 {
                continue;
            }
            let now = *content_now.get_or_insert_with(Instant::now);
            self.flow.mark(now, bytes);
            match event {
                SemanticEvent::ReasoningDelta { .. } => self.reasoning_flow.mark(now, bytes),
                _ => self.answer_flow.mark(now, bytes),
            }
        }
    }

    /// done 帧里的完整参数 → **只作补齐**,绝不重发(2026-11-06)。
    ///
    /// Responses 原生上游对同一个 function_call 推两次开场:
    /// `response.output_item.added` 与 `response.output_item.done`(后者带
    /// **最终**参数)。旧实现两次都当开场下发 → 客户端把参数累加两遍,
    /// 工具参数被污染成 `{}`(2026-11-06 ZCode 实测:8 次子智能体 + 3 次
    /// Bash 全部执行失败)。
    ///
    /// 解码层已保证「开场只推一次」,这里补的是另一头:上游若**一个参数
    /// 增量都没推**(兼容实现只发 done 帧),用 done 帧的完整参数补一次。
    /// 判据是账本里「已推送参数长度 == 0」—— 只要推过哪怕一个字符,
    /// done 帧就只作校验、不再进通道。
    fn repair_tool_arguments(&mut self, events: Vec<SemanticEvent>) -> Vec<SemanticEvent> {
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            match &event {
                SemanticEvent::ToolCallStart {
                    candidate,
                    index,
                    call,
                } => {
                    self.ledger.note_start(*candidate, *index, call);
                    out.push(event);
                }
                SemanticEvent::ToolArgumentsDelta {
                    candidate,
                    index,
                    fragment,
                } => {
                    self.ledger.note_delta(*candidate, *index, fragment);
                    out.push(event);
                }
                SemanticEvent::Opaque { raw } => {
                    if let Some((candidate, index, arguments)) =
                        crate::semantic::stream::responses_done_arguments(raw)
                    {
                        match self.ledger.reconcile(candidate, index, &arguments) {
                            ArgumentReconciliation::Duplicate => {}
                            ArgumentReconciliation::Extension { suffix } => {
                                self.ledger.note_delta(candidate, index, &suffix);
                                out.push(SemanticEvent::ToolArgumentsDelta {
                                    candidate,
                                    index,
                                    fragment: suffix,
                                });
                            }
                            ArgumentReconciliation::Snapshot { arguments } => {
                                self.ledger.note_delta(candidate, index, &arguments);
                                out.push(SemanticEvent::ToolArgumentsDelta {
                                    candidate,
                                    index,
                                    fragment: arguments,
                                });
                            }
                            ArgumentReconciliation::Conflict { .. } => {
                                // 已提交前缀不可撤回:保留它,快照只作
                                // 校验,不把冲突字节混进客户端流。
                                self.ledger.note_conflict();
                            }
                        }
                    }
                    // opaque 事件**原样保留**:它还带着上游 item 的完整
                    // 形态,客户端状态机要用它做「该收口哪个 item」的判定。
                    out.push(event);
                }
                _ => out.push(event),
            }
        }
        out
    }

    /// 正常收口(上游 EOF 且完整性门通过):补发协议终止序列。
    ///
    /// `usage` 由 [`crate::semantic::source::SourceObserver`] 提供 ——
    /// 目标是按目标协议合成终帧,不再由编码器自行计量。
    pub fn finalize(&mut self) -> Vec<String> {
        self.finalize_with_usage(self.usage)
    }

    pub fn finalize_with_usage(&mut self, usage: Usage) -> Vec<String> {
        let frames = match &mut self.inner {
            ClientStreamInner::Chat(client) => client.finalize(usage),
            ClientStreamInner::Anthropic(client) => client.finalize(usage),
            ClientStreamInner::Responses(client) => client.finalize(),
            ClientStreamInner::Gemini(client) => client.finalize(),
            // 透传流由上游收口。
            ClientStreamInner::Passthrough => Vec::new(),
        };
        self.note_frames(frames)
    }

    /// C7 断流终结:只在客户端协议真有「不完整」表示时输出
    /// (Responses → `response.incomplete`);其余协议返回空,由 EOF 表达
    /// 截断,不合成正常终止。
    pub fn finalize_incomplete(&mut self) -> Vec<String> {
        let frames = match &mut self.inner {
            ClientStreamInner::Responses(client) => client.finalize_incomplete(),
            _ => Vec::new(),
        };
        self.note_frames(frames)
    }

    /// 阶段四 C-7:内容 10%→90% 的时间跨度(毫秒,已按 `×10/8` 折算成
    /// 全量跨度);不可测时为 `None`(无内容 / 标记溢出)。
    ///
    /// ⚠️ 只能在**流结束之后**读:总内容长度要流结束才知道(10%/90%
    /// 的分位点依赖它)。在途读到的值没有意义。
    pub fn flow_ms(&self) -> Option<f64> {
        self.flow.flow_ms()
    }

    /// 20261108_01 §5.2:答案内容(排除思考)的 10%→90% 跨度(毫秒,
    /// 已按 `×10/8` 折算);不可测时为 `None`。⚠️ 流结束后才可读。
    pub fn answer_flow_ms(&self) -> Option<f64> {
        self.answer_flow.flow_ms()
    }

    /// 20261108_01 §5.2:思考内容的 10%→90% 跨度(毫秒,诊断用)。
    pub fn reasoning_flow_ms(&self) -> Option<f64> {
        self.reasoning_flow.flow_ms()
    }

    /// 20261108_01 §13(Chat 可见性):思考增量以文本流可见的字节总量。
    /// 与上报的 `reasoning_tokens` 比对,判断思考是否只是摘要/不可见。
    pub fn reasoning_content_bytes(&self) -> Option<usize> {
        let bytes = self.reasoning_flow.bytes();
        (bytes > 0).then_some(bytes)
    }

    /// 答案段起点(首个答案内容事件的时刻);用于 `first_answer_byte_ms`。
    pub fn answer_origin(&self) -> Option<Instant> {
        self.answer_flow.origin()
    }

    /// 本流是否出现过思考增量(20261108_01 吞吐分子可剥离性判断)。
    pub fn saw_reasoning(&self) -> bool {
        self.saw_reasoning
    }

    /// 编码器内部的用量缓存(仅供终帧合成与兼容测试;权威计量在
    /// `SourceObserver`)。
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// 进入续写模式:客户端状态机抑制重复的 `message_start`。
    /// 生命周期/用量/已提交正文由 `SourceObserver` 显式管理。
    pub fn begin_continuation(&mut self) {
        if let ClientStreamInner::Anthropic(client) = &mut self.inner {
            client.begin_continuation();
        }
    }

    fn note_frames(&mut self, mut frames: Vec<String>) -> Vec<String> {
        if let Some(model) = &self.output_model {
            for frame in frames.iter_mut() {
                *frame = crate::protocol::rewrite_sse_frame_model(frame, model);
            }
        }
        frames
    }
}

enum ClientStreamInner {
    Chat(ChatClient),
    Anthropic(AnthropicClient),
    Responses(ResponsesTargetClient),
    Gemini(GeminiClient),
    Passthrough,
}

/// 语义事件是否为「内容」(非信封):首字打点锚定首个内容 token,而非
/// 首个下发帧。信封帧(`MessageStart` / 仅 role 的占位 / 空块 `PartStart`)
/// 不代表模型已开始解码。口径对齐 minimax-code「首个 thinking_start 或
/// 非空 text/thinking/tool token」。
/// 源事实是否含模型内容(Raw 路径首字打点用;信封帧不算内容)。
pub fn facts_have_content(facts: &SourceFacts) -> bool {
    facts.events.iter().any(is_content_event)
}

fn is_content_event(event: &SemanticEvent) -> bool {
    match event {
        SemanticEvent::PartDelta { text, .. } => !text.is_empty(),
        SemanticEvent::RefusalDelta { text, .. } => !text.is_empty(),
        SemanticEvent::ReasoningStart { .. } => true,
        SemanticEvent::ReasoningDelta { text, .. } => !text.is_empty(),
        SemanticEvent::ToolCallStart { .. } => true,
        SemanticEvent::ToolArgumentsDelta { fragment, .. } => !fragment.is_empty(),
        // 文本块起始常带空串(占位),只有非空文本或非文本内容才算内容。
        SemanticEvent::PartStart { part, .. } => {
            !matches!(part, ContentPart::Text { text } if text.is_empty())
        }
        _ => false,
    }
}

/// 语义事件的**内容字节数**(阶段四 C-7 的 flow 测量口径)。
///
/// 只算内容:文本增量、推理增量、工具参数增量、文本块起始的正文。
/// 信封帧(角色占位 / 空块)与工具调用的「开始」记 0 —— 它们不代表
/// 内容推进(`flowContent` 口径:
/// 只看带 delta 的事件)。
fn content_bytes(event: &SemanticEvent) -> usize {
    match event {
        SemanticEvent::PartDelta { text, .. } => text.len(),
        SemanticEvent::RefusalDelta { text, .. } => text.len(),
        SemanticEvent::ReasoningDelta { text, .. } => text.len(),
        SemanticEvent::ToolArgumentsDelta { fragment, .. } => fragment.len(),
        SemanticEvent::PartStart {
            part: ContentPart::Text { text },
            ..
        } => text.len(),
        _ => 0,
    }
}

/// 阶段四 C-7:内容 10%→90% 跨度测量(`flow` 口径)。
///
///
/// 口径:累计**内容字节**随时间的推进点(按毫秒去重,同一毫秒只留一条
/// 并把累计字节更新到最新)。流结束时取「累计字节达 10% 的时刻」到
/// 「达 90% 的时刻」的跨度,再按 `× 10 / 8` 折算成全量跨度(`flowMs` 同款)。
///
///
/// 折算的意义:均匀投递的回复 flow ≈ 整个解码窗口;被上游攒住后
/// 一次性倒出来的回复 flow ≈ 0 —— 「吐字有多快」不再靠速率阈值猜。
#[derive(Debug, Default)]
struct FlowMeter {
    /// 计时原点(首个内容事件的时刻)。
    origin: Option<Instant>,
    /// 累计内容字节。
    total: usize,
    /// 推进点(毫秒,累计字节);同一毫秒只留一条。
    marks: Vec<(u64, usize)>,
/// 推进点过多 → 本次不可测(`flowMost` 上限)。
    overflow: bool,
}

/// 推进点上限(`flowMost`)。
const FLOW_MOST: usize = 1 << 16;

impl FlowMeter {
    /// 首个内容事件的时刻(答案 TTFT 锚点);无内容时 `None`。
    fn origin(&self) -> Option<Instant> {
        self.origin
    }

    /// 本计量覆盖的内容字节总量。
    fn bytes(&self) -> usize {
        self.total
    }

    /// 记一次内容推进:`bytes` 字节内容在 `now` 时刻到达。
    fn mark(&mut self, now: Instant, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let origin = *self.origin.get_or_insert(now);
        self.total += bytes;
        let at = now.saturating_duration_since(origin).as_millis() as u64;
        if let Some(last) = self.marks.last_mut()
            && last.0 == at
        {
            // 同一毫秒:只更新累计字节,不新增推进点。
            last.1 = self.total;
            return;
        }
        if self.marks.len() >= FLOW_MOST {
            self.overflow = true;
            return;
        }
        self.marks.push((at, self.total));
    }

    /// 内容 10%→90% 的跨度(毫秒,已折算成全量跨度);不可测时 `None`。
    ///
    /// 20261108_01:少于 2 个推进点(整段内容在一个瞬间到达)或量化跨度为
    /// 0 时视为**不可测** —— 上游把答案一次性倒出来,测不出解码速度。
    /// 旧实现用 `max(span, 1ms)` 兜底,把这个「不可测」伪装成 1ms,吞吐
    /// 分母因此被算成 1ms(296 答案 token → 296000 tok/s)。
    fn flow_ms(&self) -> Option<f64> {
        if self.overflow || self.total == 0 || self.marks.len() < 2 {
            return None;
        }
        let total = self.total as u64;
        // at(share):累计字节首次达到总量 share% 的推进点时刻。
        let at = |share: u64| -> u64 {
            for (at, sum) in &self.marks {
                if (*sum as u64) * 100 >= total * share {
                    return *at;
                }
            }
            self.marks[self.marks.len() - 1].0
        };
        let span = at(90).saturating_sub(at(10));
        if span == 0 {
            return None;
        }
// ×10/8:把「中间 80% 的跨度」折算成「全量跨度」。
        Some((span as f64 * 10.0 / 8.0).max(1.0))
    }
}

// ---------------------------------------------------------------------------
// OpenAI Chat Completions 客户端
// ---------------------------------------------------------------------------

/// 开场工具调用块如果已带完整参数(Raw 字符串或非空 Parsed 对象),
/// 立即补发一个 arguments-only 帧,避免丢开头大括号(回归样本
/// `openai-chat-stream-tool-first-fragment`)。
fn opening_arguments(call: &ToolCall) -> Option<String> {
    match &call.arguments {
        ToolArguments::Raw(raw) if !raw.is_empty() => Some(raw.clone()),
        ToolArguments::Parsed(value) => {
            if value.as_object().is_some_and(|map| !map.is_empty()) || value.is_array() {
                Some(py_json_dumps(value))
            } else {
                None
            }
        }
        _ => None,
    }
}
