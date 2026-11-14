
use super::*;
use crate::semantic::model::ToolArguments;

fn call(id: &str, name: &str, args: ToolArguments) -> ToolCall {
    ToolCall {
        id: ToolCallId {
            local: id.to_owned(),
            origin: IdOrigin::Source,
        },
        name: name.to_owned(),
        arguments: args,
        extensions: Default::default(),
    }
}

fn payloads(frames: &[String]) -> Vec<Value> {
    frames
        .iter()
        .filter_map(|frame| {
            let data = frame.trim().strip_prefix("data:")?.trim();
            serde_json::from_str::<Value>(data).ok()
        })
        .collect()
}

#[test]
fn multi_part_message_gets_independent_content_indices_and_order() {
    let mut client = ResponsesTargetClient::new();
    let updates = vec![
        StreamUpdate::ItemStart {
            key: StreamKey::item(0, 0),
            id: Some("msg_1".into()),
            kind: ItemKind::Message,
            extensions: Default::default(),
        },
        StreamUpdate::PartStart {
            key: StreamKey::new(0, 0, 0),
            id: None,
            part: crate::semantic::model::ContentPart::Text {
                text: String::new(),
            },
            extensions: Default::default(),
        },
        StreamUpdate::PartDelta {
            key: StreamKey::new(0, 0, 0),
            text: "hello ".into(),
        },
        StreamUpdate::PartEnd {
            key: StreamKey::new(0, 0, 0),
        },
        StreamUpdate::PartStart {
            key: StreamKey::new(0, 0, 1),
            id: None,
            part: crate::semantic::model::ContentPart::Text {
                text: String::new(),
            },
            extensions: Default::default(),
        },
        StreamUpdate::PartDelta {
            key: StreamKey::new(0, 0, 1),
            text: "world".into(),
        },
        StreamUpdate::PartEnd {
            key: StreamKey::new(0, 0, 1),
        },
        StreamUpdate::ItemEnd {
            key: StreamKey::item(0, 0),
        },
        StreamUpdate::Finish {
            candidate: 0,
            reason: FinishReason::Stop,
        },
        StreamUpdate::End { incomplete: false },
    ];
    let mut frames = client.consume(&updates);
    frames.extend(client.finalize());
    let payloads = payloads(&frames);
    let part_indices: Vec<u64> = payloads
        .iter()
        .filter(|value| value["type"] == json!("response.content_part.added"))
        .filter_map(|value| value["content_index"].as_u64())
        .collect();
    assert_eq!(part_indices, vec![0, 1], "{payloads:?}");
    let completed = payloads
        .iter()
        .find(|value| value["type"] == json!("response.completed"))
        .expect("completed");
    let content = completed["response"]["output"][0]["content"]
        .as_array()
        .expect("message content");
    assert_eq!(content.len(), 2, "{completed}");
    assert_eq!(content[0]["text"], json!("hello "));
    assert_eq!(content[1]["text"], json!("world"));
    assert_eq!(completed["response"]["output_text"], json!("hello world"));
}

#[test]
fn snapshot_only_tool_arguments_are_emitted_once() {
    let mut client = ResponsesTargetClient::new();
    let updates = vec![
        StreamUpdate::ToolStart {
            key: StreamKey::item(0, 2),
            tool_index: 0,
            call: call("call_1", "lookup", ToolArguments::Parsed(json!({}))),
        },
        StreamUpdate::ItemSnapshot {
            key: StreamKey::item(0, 2),
            snapshot: json!({
                "id": "fc_1", "type": "function_call", "call_id": "call_1",
                "name": "lookup", "arguments": "{\"q\":1}",
            }),
        },
        StreamUpdate::ItemEnd {
            key: StreamKey::item(0, 2),
        },
    ];
    let mut frames = client.consume(&updates);
    frames.extend(client.finalize());
    let payloads = payloads(&frames);
    let deltas: Vec<&str> = payloads
        .iter()
        .filter(|value| value["type"] == json!("response.function_call_arguments.delta"))
        .filter_map(|value| value["delta"].as_str())
        .collect();
    assert_eq!(deltas, vec!["{\"q\":1}"], "{payloads:?}");
    let done = payloads
        .iter()
        .find(|value| value["type"] == json!("response.function_call_arguments.done"))
        .expect("arguments done");
    assert_eq!(done["arguments"], json!("{\"q\":1}"));
}

#[test]
fn snapshot_extends_committed_prefix_without_replaying() {
    let mut client = ResponsesTargetClient::new();
    let updates = vec![
        StreamUpdate::ToolStart {
            key: StreamKey::item(0, 2),
            tool_index: 0,
            call: call("call_1", "lookup", ToolArguments::Raw(String::new())),
        },
        StreamUpdate::ToolArgumentsDelta {
            key: StreamKey::item(0, 2),
            tool_index: 0,
            fragment: "{\"q\":".into(),
        },
        StreamUpdate::ItemSnapshot {
            key: StreamKey::item(0, 2),
            snapshot: json!({
                "id": "fc_1", "type": "function_call", "call_id": "call_1",
                "name": "lookup", "arguments": "{\"q\":1}",
            }),
        },
        StreamUpdate::ItemEnd {
            key: StreamKey::item(0, 2),
        },
    ];
    let mut frames = client.consume(&updates);
    frames.extend(client.finalize());
    let payloads = payloads(&frames);
    let deltas: String = payloads
        .iter()
        .filter(|value| value["type"] == json!("response.function_call_arguments.delta"))
        .filter_map(|value| value["delta"].as_str())
        .collect();
    assert_eq!(deltas, "{\"q\":1}", "只补后缀,不重放前缀:{payloads:?}");
    let done = payloads
        .iter()
        .find(|value| value["type"] == json!("response.function_call_arguments.done"))
        .expect("arguments done");
    assert_eq!(done["arguments"], json!("{\"q\":1}"));
}

#[test]
fn snapshot_conflict_keeps_committed_arguments() {
    let mut client = ResponsesTargetClient::new();
    let updates = vec![
        StreamUpdate::ToolStart {
            key: StreamKey::item(0, 2),
            tool_index: 0,
            call: call("call_1", "lookup", ToolArguments::Raw(String::new())),
        },
        StreamUpdate::ToolArgumentsDelta {
            key: StreamKey::item(0, 2),
            tool_index: 0,
            fragment: "{\"q\":2}".into(),
        },
        StreamUpdate::ItemSnapshot {
            key: StreamKey::item(0, 2),
            snapshot: json!({
                "id": "fc_1", "type": "function_call", "call_id": "call_1",
                "name": "lookup", "arguments": "{\"q\":1}",
            }),
        },
        StreamUpdate::ItemEnd {
            key: StreamKey::item(0, 2),
        },
    ];
    let mut frames = client.consume(&updates);
    frames.extend(client.finalize());
    assert!(
        client
            .conflicts
            .iter()
            .any(|conflict| conflict.contains("conflict")),
        "{:?}",
        client.conflicts
    );
    let payloads = payloads(&frames);
    let done = payloads
        .iter()
        .find(|value| value["type"] == json!("response.function_call_arguments.done"))
        .expect("arguments done");
    assert_eq!(done["arguments"], json!("{\"q\":2}"));
}

#[test]
fn late_snapshot_name_fills_missing_tool_name() {
    let mut client = ResponsesTargetClient::new();
    let updates = vec![
        StreamUpdate::ToolStart {
            key: StreamKey::item(0, 2),
            tool_index: 0,
            call: call("call_1", "", ToolArguments::Raw(String::new())),
        },
        StreamUpdate::ItemSnapshot {
            key: StreamKey::item(0, 2),
            snapshot: json!({
                "id": "fc_1", "type": "function_call", "call_id": "call_1",
                "name": "lookup", "arguments": "{}",
            }),
        },
        StreamUpdate::ItemEnd {
            key: StreamKey::item(0, 2),
        },
    ];
    let mut frames = client.consume(&updates);
    frames.extend(client.finalize());
    let payloads = payloads(&frames);
    let done = payloads
        .iter()
        .find(|value| value["type"] == json!("response.function_call_arguments.done"))
        .expect("arguments done");
    assert_eq!(done["name"], json!("lookup"));
}
