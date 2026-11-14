//! 模型指纹库导入/导出(方案 20261030_01 §3.7;2026-11-01 自
//! `model_fingerprint.rs` 按职责拆出 —— 导入导出与探针/检测完全正交)。
//!
//! 导出文件带两段(`robust-number-fingerprint-bank`):
//! - `models` / `robust` / `calibration`:由样本**现算**的拟合产物;
//! - [`FINGERPRINT_EXPORT_ENTRIES_KEY`]:原始条目与样本,往返的唯一载体。
//!
//! 导入**只接受**带原始样本段的文件。只有拟合产物的外部指纹库一律拒绝:
//! 那类库采集时用的探针套件已下线,强行导入会拿错套件比对,给出不可信的
//! 归因。导入走 **X7 查重**;`cvAccuracy` 一律由**本地现算**的校准给出,
//! **不沿用**文件里的值(方案 §3.7:避免上游 fallback 旧值污染)。

use serde_json::{Map, Value, json};

use wr_app::ports::FingerprintStore;
use wr_domain::fingerprint::ProbeSuite;
use wr_domain::fingerprint::bank::build_bank;

use super::AdminState;
use super::model_fingerprint::{duplicate_model_reason, entries_to_samples};

/// 导出文件的 schema 标识。
///
/// 顶层除拟合产物段外,另附 [`FINGERPRINT_EXPORT_ENTRIES_KEY`] 原始条目段;
/// 外部工具按字段名取值,多余键会被忽略。
pub const FINGERPRINT_EXPORT_SCHEMA: &str = "robust-number-fingerprint-bank";

/// WRouter 扩展段:原始条目(含样本正文)—— 无损往返的唯一载体。
pub const FINGERPRINT_EXPORT_ENTRIES_KEY: &str = "wrouterEntries";

/// `exportModelFingerprints`:导出用户自建库。
pub async fn export_model_fingerprints(state: &AdminState, _args: &[Value]) -> Result<Value, String> {
    let entries = state
        .store
        .all_entries()
        .await
        .map_err(|error| error.to_string())?;
    let document = build_export_document(&entries);
    let text = serde_json::to_string(&document.to_value())
        .map_err(|error| format!("序列化导出文件失败:{error}"))?;
    Ok(json!({
        "ok": true,
        "json": text,
        "modelCount": document.model_count,
        "sampleCount": document.sample_count,
        "bankIncluded": document.bank_included,
        "probeSuite": ProbeSuite::WrouterV1.as_str(),
        "reason": document.reason,
    }))
}

/// 导出文档(纯函数:便于单测)。
struct ExportDocument {
    bank_included: bool,
    model_count: usize,
    reason: Value,
    sample_count: usize,
    /// 序列化后的文档本体。
    document: Value,
}

impl ExportDocument {
    /// 序列化成 JSON 值。
    fn to_value(&self) -> Value {
        self.document.clone()
    }
}

/// 构造导出文档(纯函数)。
fn build_export_document(entries: &[Value]) -> ExportDocument {
    let samples = entries_to_samples(entries, None);

    let mut document: Map<String, Value> = build_bank(&samples)
        .ok()
        .and_then(|bank| serde_json::to_value(bank).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();

    let mut model_ids: Vec<&str> = samples.iter().map(|sample| sample.source.as_str()).collect();
    model_ids.sort_unstable();
    model_ids.dedup();

    document.insert(
        "schema".to_owned(),
        Value::String(FINGERPRINT_EXPORT_SCHEMA.to_owned()),
    );
    document.insert("version".to_owned(), json!(1));
    document.insert(
        "exportedAt".to_owned(),
        Value::String(chrono::Utc::now().to_rfc3339()),
    );
    document.insert(
        FINGERPRINT_EXPORT_ENTRIES_KEY.to_owned(),
        Value::Array(entries.to_vec()),
    );

    let bank_included = document.contains_key("robust");
    ExportDocument {
        bank_included,
        model_count: model_ids.len(),
        reason: if bank_included {
            Value::Null
        } else {
            Value::String(
                "用户自建库不足 2 个模型,未生成拟合产物段(原始条目段仍完整)。".to_owned(),
            )
        },
        sample_count: samples.len(),
        document: Value::Object(document),
    }
}

/// 一个待导入条目(已归一为「模型名 + 样本」)。
struct ImportCandidate {
    family: Option<String>,
    family_name: Option<String>,
    kind: String,
    label: String,
    model_name: String,
    provider_id: Option<String>,
    samples: Vec<Value>,
}

/// `importModelFingerprint`:导入指纹库文件。
///
/// 两种来源:
/// 1. **WRouter 导出文件**(顶层带 `wrouterEntries`)—— 无损:条目与样本
///    原样落库,环境 id 与探针 id 一并保留 → 不破坏库的环境完整性;
/// 2. **ModelTrace `unified_bank.json`**(只有拟合产物)—— 有损:按
///    `models[].counts` 重建伪样本(见 [`reconstruct_import_samples`]),
///    并把伪样本**挂到既有库的环境 id 上**(否则新环境会让
///    `complete_environments` 变空 → 整库失去干扰基)。
pub async fn import_model_fingerprints(state: &AdminState, args: &[Value]) -> Result<Value, String> {
    let payload = args
        .first()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let document = if let Some(text) = payload.get("json").and_then(Value::as_str) {
        serde_json::from_str::<Value>(text)
            .map_err(|error| format!("导入文件不是合法 JSON:{error}"))?
    } else if let Some(bank) = payload.get("bank") {
        bank.clone()
    } else {
        return Err("importModelFingerprint requires json".to_owned());
    };
    let object = document
        .as_object()
        .ok_or_else(|| "导入文件必须是 JSON 对象".to_owned())?;
    let schema = object.get("schema").and_then(Value::as_str).unwrap_or_default();
    if schema != FINGERPRINT_EXPORT_SCHEMA {
        return Err(format!(
            "不支持的导入格式:{schema}(期望 {FINGERPRINT_EXPORT_SCHEMA})。"
        ));
    }

    let existing = state
        .store
        .all_entries()
        .await
        .map_err(|error| error.to_string())?;
    let mut imported: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut known = existing.clone();

    for candidate in collect_import_candidates(object)? {
        let entry = candidate_entry_value(&candidate);
        let new_samples = entries_to_samples(std::slice::from_ref(&entry), None);
        if new_samples.is_empty() {
            skipped.push(json!({
                "model": candidate.model_name,
                "reason": "没有可解析的样本回答(数字数不足)。",
            }));
            continue;
        }
        // X7:与既有条目 + 本次已导入条目合并后查重;同一份合并库**建一次**,
        // 复用于校准现算(校准交叉验证是建库里最贵的部分,不再为取
        // cvAccuracy 重复建库)。
        let mut merged = entries_to_samples(&known, None);
        merged.extend(new_samples);
        let merged_bank = build_bank(&merged).ok();
        if let Some(reason) = merged_bank
            .as_ref()
            .and_then(|bank| duplicate_model_reason(bank, &candidate.model_name))
        {
            skipped.push(json!({"model": candidate.model_name, "reason": reason}));
            continue;
        }
        // 校准本地现算(不沿用文件里的 cv_accuracy)。
        let local_accuracy = merged_bank.as_ref().and_then(|bank| {
            bank.calibration.get("1").and_then(|entry| entry.cv_accuracy)
        });
        let mut entry = entry;
        if let Some(map) = entry.as_object_mut() {
            map.insert(
                "cvAccuracy".to_owned(),
                local_accuracy.map_or(Value::Null, Value::from),
            );
        }
        let entry_id = entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        state
            .store
            .upsert_entry(&entry)
            .await
            .map_err(|error| error.to_string())?;
        known.push(entry);
        imported.push(json!({
            "model": candidate.model_name,
            "id": entry_id,
            "sampleCount": candidate.samples.len(),
        }));
    }

    Ok(json!({
        "ok": true,
        "imported": imported,
        "skipped": skipped,
        "probeSuite": ProbeSuite::WrouterV1.as_str(),
    }))
}

/// 候选条目 → 存储条目(与 `enroll` 落库的字段集合一致)。
fn candidate_entry_value(candidate: &ImportCandidate) -> Value {
    let provider_id = candidate.provider_id.clone();
    let entry_id = format!(
        "{}:{}:{}",
        candidate.kind,
        provider_id.as_deref().unwrap_or("-"),
        candidate.model_name
    );
    json!({
        "id": entry_id,
        "kind": candidate.kind,
        "providerId": provider_id,
        "modelName": candidate.model_name,
        "family": candidate.family,
        "familyName": candidate.family_name,
        "label": candidate.label,
        // 用户自建库统一用 wrouter-v1 套件(库内套件必须一致)。
        "probeSuite": ProbeSuite::WrouterV1.as_str(),
        "sampleCount": candidate.samples.len(),
        "probeSeed": format!("import-{}", candidate.model_name),
        "probeEnv": candidate
            .samples
            .iter()
            .filter_map(|sample| sample.get("conditionId").and_then(Value::as_str))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(","),
        "cvAccuracy": Value::Null,
        "samples": candidate.samples,
    })
}

/// 收集导入候选:只接受带原始样本段(`wrouterEntries`)的文件。
fn collect_import_candidates(object: &Map<String, Value>) -> Result<Vec<ImportCandidate>, String> {
    let entries = object
        .get(FINGERPRINT_EXPORT_ENTRIES_KEY)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "导入文件缺少原始样本段(wrouterEntries):只有拟合产物的外部指纹库无法比对,请在本项目内重新采集。".to_owned()
        })?;
    let mut candidates = Vec::new();
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let model_name = entry
            .get("modelName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if model_name.is_empty() {
            continue;
        }
        let samples = entry
            .get("samples")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if samples.is_empty() {
            continue;
        }
        candidates.push(ImportCandidate {
            family: entry.get("family").and_then(Value::as_str).map(str::to_owned),
            family_name: entry.get("familyName").and_then(Value::as_str).map(str::to_owned),
            kind: entry
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("global")
                .to_owned(),
            label: entry
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or(&model_name)
                .to_owned(),
            model_name,
            provider_id: entry.get("providerId").and_then(Value::as_str).map(str::to_owned),
            samples,
        });
    }
    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wr_domain::fingerprint::probe::SeededRng;

    /// 造一条数字文本(确定性伪随机)。
    fn numbers_text(seed: &str, count: usize) -> String {
        let mut rng = SeededRng::from_str_seed(seed);
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(((rng.next_u64() % 355) + 1).to_string());
        }
        values.join(" ")
    }

    /// 造一条存储条目。
    fn entry(model: &str, samples: &[(&str, &str)]) -> Value {
        json!({
            "id": format!("global:-:{model}"),
            "kind": "global",
            "providerId": Value::Null,
            "modelName": model,
            "family": "gpt",
            "familyName": "GPT",
            "label": model,
            "probeSuite": ProbeSuite::WrouterV1.as_str(),
            "sampleCount": samples.len(),
            "samples": samples.iter().map(|(challenge, condition)| json!({
                "challengeId": challenge,
                "conditionId": condition,
                "expectedCount": 300,
                "text": numbers_text(&format!("{model}-{challenge}"), 300),
            })).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn export_document_carries_bank_and_entries() {
        let entries = vec![
            entry("m1", &[("c1", "env-1"), ("c2", "env-2")]),
            entry("m2", &[("c3", "env-1"), ("c4", "env-2")]),
        ];
        let document = build_export_document(&entries);
        let value = document.to_value();
        let object = value.as_object().expect("object");

        assert_eq!(
            object.get("schema").and_then(Value::as_str),
            Some(FINGERPRINT_EXPORT_SCHEMA)
        );
        assert!(object.contains_key("robust"), "必须带拟合产物段");
        assert!(object.contains_key("calibration"));
        assert!(document.bank_included);
        assert_eq!(document.model_count, 2);
        assert_eq!(document.sample_count, 4);
        // WRouter 扩展段:原始条目(含样本)→ 无损往返的载体。
        assert_eq!(
            object
                .get(FINGERPRINT_EXPORT_ENTRIES_KEY)
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn export_document_keeps_entries_when_library_is_too_small() {
        let entries = vec![entry("m1", &[("c1", "env-1")])];
        let document = build_export_document(&entries);
        let value = document.to_value();
        assert!(!document.bank_included);
        assert!(document.reason.is_string(), "不足 2 个模型要给出原因");
        assert_eq!(
            value
                .get(FINGERPRINT_EXPORT_ENTRIES_KEY)
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1),
            "条目段仍然完整"
        );
    }

    #[test]
    fn import_prefers_native_entries_section() {
        let entries = vec![entry("m1", &[("c1", "env-1"), ("c2", "env-2")])];
        let document = build_export_document(&entries).to_value();
        let object = document.as_object().expect("object");
        let candidates = collect_import_candidates(object).expect("candidates");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].model_name, "m1");
        assert_eq!(candidates[0].samples.len(), 2, "样本原样保留(无损)");
    }

    #[test]
    fn import_rejects_file_without_entries_section() {
        // 只有拟合产物段、没有原始样本段的外部库:采集套件已下线,必须拒绝。
        let document = json!({
            "schema": FINGERPRINT_EXPORT_SCHEMA,
            "models": [{"id": "gpt-x", "counts": [1, 2, 3]}],
        });
        let object = document.as_object().expect("object");
        let error = match collect_import_candidates(object) {
            Ok(_) => panic!("缺原始样本段必须拒绝"),
            Err(error) => error,
        };
        assert!(error.contains("wrouterEntries"), "{error}");
    }

    #[test]
    fn imported_entries_use_user_library_suite() {
        let candidate = ImportCandidate {
            family: None,
            family_name: None,
            kind: "global".to_owned(),
            label: "m1".to_owned(),
            model_name: "m1".to_owned(),
            provider_id: None,
            samples: vec![json!({
                "challengeId": "c1",
                "conditionId": "env-1",
                "expectedCount": 300,
                "text": numbers_text("m1", 300),
            })],
        };
        let entry = candidate_entry_value(&candidate);
        assert_eq!(
            entry.get("probeSuite").and_then(Value::as_str),
            Some(ProbeSuite::WrouterV1.as_str()),
            "用户自建库统一套件(X9)"
        );
        assert_eq!(
            entry.get("id").and_then(Value::as_str),
            Some("global:-:m1"),
            "条目 id 与 enroll 一致"
        );
    }
}
