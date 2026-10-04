//! 模型适配层（FR-R09）+ 草稿整理（FR-R02 / FR-R05）。
//!
//! ## 为什么要"命令适配"而不是内置某家 SDK
//! 语音识别与多模态模型换代极快，而且**本地跑**（whisper.cpp / mlx-whisper / ollama）
//! 才是田间默认姿态（FR-R07 离线优先）。所以这一层只做两件事：
//! - 按用户给的**命令模板**调用（`{audio}` / `{image}` / `{out}` / `{prompt_file}` 占位符），
//!   想用本地模型、想用云端（例如 `curl` 打 OpenAI 兼容接口）都由用户决定；
//! - 把模型返回的**原始输出一字不改地存档**，并记下命令、模型名与时间
//!   （FR-R09：换供应商不改数据格式，会话里说得清"是谁识别的"）。
//!
//! ## 草稿整理的三条纪律
//! 1. **不得编造**：模型没给的字段留空，不替它猜；
//! 2. **听不清就留空 + 记账**：写进待确认清单，而不是塞个"？"进数据；
//! 3. **模板之外的键一律丢弃**并列出（模型偶尔会多加 `remark` 之类）。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use crate::brief::FieldInfo;
use crate::row::Row;
use crate::{Fail, R};

/// 待确认的一条。
#[derive(Debug, Clone)]
pub struct Uncertain {
    /// 第几条（1 起，对应草稿表里的行号）
    pub index: usize,
    pub field: String,
    pub reason: String,
    /// 分类：number（数值，必须人工确认）/ required（必填为空）/ low_confidence / unknown_field
    pub kind: String,
    /// 是否必须人工确认后才允许入库（数值与必填缺失为 true）
    pub must_confirm: bool,
}

/// 一次识别的产物（还没落盘）。
pub struct Parsed {
    pub rows: Vec<Row>,
    pub uncertain: Vec<Uncertain>,
    pub notes: Vec<String>,
    /// 模型给了但模板里没有的键
    pub ignored_keys: Vec<String>,
    /// **逐格置信度**：与 `rows` 一一对应（第 i 行 → 字段 → 分数）
    pub confidence: Vec<std::collections::BTreeMap<String, CellScore>>,
}

/// 一个格子的置信度：模型自报与规则判定取**较小值**（保守）。
#[derive(Debug, Clone, PartialEq)]
pub struct CellScore {
    /// 0.0–1.0
    pub score: f32,
    /// `model` / `heuristic` / `both`
    pub source: String,
    /// 为什么给这个分（给复核的人看）
    pub reason: String,
}

impl CellScore {
    fn new(score: f32, source: &str, reason: &str) -> Self {
        Self {
            score: score.clamp(0.0, 1.0),
            source: source.to_string(),
            reason: reason.to_string(),
        }
    }
}

/// 低于这个分数的格子要在复核里点名。
pub const LOW_CONFIDENCE: f32 = 0.7;

/// 解析模型返回的 JSON。
///
/// 容忍三种写法（真实模型都出现过）：`{"rows": [...]}`、裸数组 `[...]`、
/// 以及带 `uncertain` / `notes` 的完整形状。
pub fn parse_answer(answer: &Value, fields: &[FieldInfo]) -> R<Parsed> {
    let rows_value: Vec<Value> = if let Some(arr) = answer.as_array() {
        arr.clone()
    } else if let Some(arr) = answer.get("rows").and_then(|v| v.as_array()) {
        arr.clone()
    } else if let Some(arr) = answer.get("data").and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        return Err(Fail::new(
            "模型返回的 JSON 里找不到 rows 数组",
            "期望形如 {\"rows\": [ {...} ]}；也可以直接给一个数组",
        ));
    };

    let known: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    let mut rows: Vec<Row> = Vec::new();
    let mut ignored_keys: Vec<String> = Vec::new();
    let mut model_conf: Vec<std::collections::BTreeMap<String, (f32, String)>> = Vec::new();

    for item in &rows_value {
        let obj = item.as_object().ok_or_else(|| {
            Fail::new(
                "rows 里有一项不是对象",
                "每一项应当是 {\"字段名\": \"值\"} 这样的对象",
            )
        })?;
        let mut row = Row::new();
        let mut conf = std::collections::BTreeMap::new();
        for (k, v) in obj {
            // 逐格置信度：`"confidence": {"株高": 0.8}` 或 {"株高": {"score": 0.8, "reason": "…"}}
            if k == "confidence" {
                if let Some(map) = v.as_object() {
                    for (fname, cv) in map {
                        let (score, reason) = match cv {
                            Value::Number(n) => (n.as_f64().unwrap_or(0.0) as f32, String::new()),
                            Value::Object(o) => (
                                o.get("score")
                                    .and_then(|s| s.as_f64())
                                    .unwrap_or(0.0) as f32,
                                o.get("reason")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            ),
                            _ => continue,
                        };
                        conf.insert(fname.clone(), (score.clamp(0.0, 1.0), reason));
                    }
                }
                continue;
            }
            if !known.contains(&k.as_str()) {
                if !ignored_keys.contains(k) {
                    ignored_keys.push(k.clone());
                }
                continue;
            }
            row.insert(k.clone(), cell_text(v));
        }
        for f in fields {
            row.entry(f.name.clone()).or_default();
        }
        rows.push(row);
        model_conf.push(conf);
    }

    // 逐格置信度 = min(模型自报, 规则判定)：模型没说就只看规则；规则也拿不准就低分
    let mut confidence: Vec<std::collections::BTreeMap<String, CellScore>> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let mut cells = std::collections::BTreeMap::new();
        for f in fields {
            let value = row.get(&f.name).map(|s| s.as_str()).unwrap_or("");
            let (h, why) = heuristic_score(f, value);
            let model = model_conf.get(i).and_then(|m| m.get(&f.name).cloned());
            let cell = match model {
                Some((m, reason)) => {
                    let (score, source) = if m < h {
                        (m, "model")
                    } else if (m - h).abs() < 0.001 {
                        (m, "both")
                    } else {
                        (h, "heuristic")
                    };
                    let why = if reason.trim().is_empty() {
                        why.to_string()
                    } else {
                        format!("{why}；模型说：{reason}")
                    };
                    CellScore::new(score, source, &why)
                }
                None => CellScore::new(h, "heuristic", why),
            };
            cells.insert(f.name.clone(), cell);
        }
        confidence.push(cells);
    }

    // 待确认：模型自己报的
    let mut uncertain: Vec<Uncertain> = Vec::new();
    if let Some(list) = answer.get("uncertain").and_then(|v| v.as_array()) {
        for u in list {
            let index = u.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let field = u
                .get("field")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let reason = u
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("模型没有说明原因")
                .to_string();
            if index == 0 || index > rows.len() {
                continue; // 指向不存在的行：忽略，不制造噪音
            }
            let f = fields.iter().find(|f| f.name == field);
            let (kind, must) = classify(f, rows.get(index - 1), &field);
            uncertain.push(Uncertain {
                index,
                field,
                reason,
                kind,
                must_confirm: must,
            });
        }
    }

    // 必填为空：即使模型没说，也要进清单（FR-R05）
    for (i, row) in rows.iter().enumerate() {
        for f in fields.iter().filter(|f| f.required) {
            let empty = row.get(&f.name).map(|v| v.trim().is_empty()).unwrap_or(true);
            if empty
                && !uncertain
                    .iter()
                    .any(|u| u.index == i + 1 && u.field == f.name)
            {
                uncertain.push(Uncertain {
                    index: i + 1,
                    field: f.name.clone(),
                    reason: "必填项为空（用户没说，或没听清）".into(),
                    kind: "required".into(),
                    must_confirm: true,
                });
            }
        }
    }

    let notes: Vec<String> = answer
        .get("notes")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(Parsed {
        rows,
        uncertain,
        notes,
        ignored_keys,
        confidence,
    })
}

/// 规则判定的置信度（不依赖模型，离线也有底线）：
/// - 数值列：能解析成数字 → 0.9；解析不了 → 0.3
/// - 日期列：`YYYY-MM-DD` → 0.95；别的写法 → 0.4
/// - 必填为空 → 0.2；选填为空 → 0.75
/// - 文本里有"可能/大约/？/听不清"这类字样 → 0.4
/// - 其它非空文本 → 0.8
fn heuristic_score(f: &FieldInfo, value: &str) -> (f32, &'static str) {
    let v = value.trim();
    if v.is_empty() {
        return if f.required {
            (0.2, "必填但为空")
        } else {
            (0.75, "选填为空（缺失留空是允许的）")
        };
    }
    match f.field_type.as_str() {
        "number" | "integer" => {
            if v.parse::<f64>().is_ok() {
                (0.9, "数值格式正确")
            } else {
                (0.3, "看不清是不是数字")
            }
        }
        "date" => {
            if chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok() {
                (0.95, "日期格式正确")
            } else {
                (0.4, "日期写法不认识（应为 YYYY-MM-DD）")
            }
        }
        "datetime" => {
            if chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%d %H:%M:%S").is_ok()
                || chrono::DateTime::parse_from_rfc3339(v).is_ok()
            {
                (0.95, "时间格式正确")
            } else {
                (0.4, "时间写法不认识")
            }
        }
        "bool" => match v {
            "true" | "false" | "是" | "否" | "有" | "无" => (0.95, "布尔值明确"),
            _ => (0.45, "是不是/否的意思拿不准"),
        },
        _ => {
            let fuzzy = ["可能", "大约", "大概", "?", "？", "听不清", "没看清", "差不多"];
            if fuzzy.iter().any(|k| v.contains(k)) {
                (0.4, "话里有不确定的字样")
            } else if v.len() > 60 {
                (0.6, "长文本容易听岔")
            } else {
                (0.8, "普通文本")
            }
        }
    }
}

/// 低于阈值的格子（复核时点名用）。
pub fn low_confidence_cells(
    rows: &[Row],
    confidence: &[std::collections::BTreeMap<String, CellScore>],
    threshold: f32,
) -> Vec<Uncertain> {
    let mut out = Vec::new();
    for (i, cells) in confidence.iter().enumerate() {
        for (field, c) in cells {
            if c.score < threshold {
                let _ = rows.get(i);
                out.push(Uncertain {
                    index: i + 1,
                    field: field.clone(),
                    reason: format!("置信度 {:.2}（{}）：{}", c.score, c.source, c.reason),
                    kind: "low_confidence".into(),
                    must_confirm: false,
                });
            }
        }
    }
    out.sort_by(|a, b| a.index.cmp(&b.index).then(a.field.cmp(&b.field)));
    out
}

/// 逐格置信度落成 JSON（存进会话目录，复核与 app 都读它）。
pub fn confidence_json(
    rows: &[Row],
    confidence: &[std::collections::BTreeMap<String, CellScore>],
) -> Value {
    let mut out_rows = Vec::new();
    for (i, cells) in confidence.iter().enumerate() {
        let mut obj = serde_json::Map::new();
        for (field, c) in cells {
            obj.insert(
                field.clone(),
                json!({
                    "score": (c.score * 100.0).round() / 100.0,
                    "source": c.source,
                    "reason": c.reason,
                    "value": rows.get(i).and_then(|r| r.get(field)).cloned().unwrap_or_default(),
                }),
            );
        }
        out_rows.push(json!({ "index": i + 1, "cells": Value::Object(obj) }));
    }
    json!({
        "format": "cercis-scribe-confidence/1",
        "threshold": LOW_CONFIDENCE,
        "rows": out_rows,
    })
}

/// 一条待确认属于哪一类、是否必须人工确认。
fn classify(f: Option<&FieldInfo>, row: Option<&Row>, field: &str) -> (String, bool) {
    match f {
        Some(info) => {
            let empty = row
                .and_then(|r| r.get(field))
                .map(|v| v.trim().is_empty())
                .unwrap_or(true);
            if matches!(info.field_type.as_str(), "number" | "integer") {
                // FR-R05：数值未经人工确认不得入库
                ("number".into(), true)
            } else if info.required && empty {
                ("required".into(), true)
            } else {
                ("low_confidence".into(), false)
            }
        }
        None => ("unknown_field".into(), false),
    }
}

/// 一次外部命令运行的结果。
pub struct ModelRun {
    pub cmd: String,
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// 运行一条外部命令（语音识别 / 多模态 / 大模型）。
///
/// 占位符：`{audio}`、`{image}`（多个用空格连接）、`{out}`、`{prompt_file}`。
/// 提示词通过 **stdin** 交给模型；模型把 JSON 打到 stdout。
pub fn run_command(
    cmd_template: &str,
    audio: &[PathBuf],
    images: &[PathBuf],
    out_dir: &Path,
    prompt_file: &Path,
    stdin_text: Option<&str>,
) -> R<ModelRun> {
    let cmd = cmd_template
        .replace(
            "{audio}",
            &audio
                .iter()
                .map(|p| shell_quote(p))
                .collect::<Vec<_>>()
                .join(" "),
        )
        .replace(
            "{image}",
            &images
                .iter()
                .map(|p| shell_quote(p))
                .collect::<Vec<_>>()
                .join(" "),
        )
        .replace("{out}", &shell_quote(out_dir))
        .replace("{prompt_file}", &shell_quote(prompt_file));

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Fail::new(format!("跑不起来模型命令：{e}"), "确认命令写对了、程序已安装"))?;
    if let Some(text) = stdin_text {
        use std::io::Write;
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(text.as_bytes())
                .map_err(|e| Fail::new(format!("把提示词交给模型时出错：{e}"), ""))?;
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Fail::new(format!("等模型返回时出错：{e}"), ""))?;
    Ok(ModelRun {
        cmd,
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        code: out.status.code().unwrap_or(-1),
    })
}

/// 从模型输出里抠出 JSON（模型有时会加解释文字或 ```json 围栏）。
pub fn extract_json(stdout: &str) -> R<Value> {
    let text = stdout.trim();
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Ok(v);
    }
    let unfenced = text
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<Value>(unfenced) {
        return Ok(v);
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if start < end {
            if let Ok(v) = serde_json::from_str::<Value>(&text[start..=end]) {
                return Ok(v);
            }
        }
    }
    if let (Some(start), Some(end)) = (text.find('['), text.rfind(']')) {
        if start < end {
            if let Ok(v) = serde_json::from_str::<Value>(&text[start..=end]) {
                return Ok(v);
            }
        }
    }
    Err(Fail::new(
        "模型输出里找不到 JSON",
        "看会话目录里的 raw.txt；提示词里已写明「只输出 JSON」，必要时换更强的模型",
    ))
}

/// 待确认清单 → Markdown（人读）。
pub fn review_markdown(
    session_id: &str,
    table: &str,
    row_count: usize,
    uncertain: &[Uncertain],
    notes: &[String],
    ignored_keys: &[String],
    extra: &[String],
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# 待确认清单（会话 {session_id}）\n\n"));
    s.push_str(&format!(
        "草稿表：{table}.draft.csv｜识别出 {row_count} 条记录\n\n"
    ));
    s.push_str("> 数值与必填项**必须人工确认后**才能入库（FR-R05）。\n\n");

    if uncertain.is_empty() {
        s.push_str("没有待确认项。\n");
    } else {
        s.push_str("| 第几条 | 字段 | 类别 | 必须确认 | 说明 |\n|---|---|---|---|---|\n");
        for u in uncertain {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                u.index,
                u.field,
                kind_cn(&u.kind),
                if u.must_confirm { "是" } else { "否" },
                u.reason.replace('|', "\\|")
            ));
        }
    }

    if !notes.is_empty() {
        s.push_str("\n## 模型记下的备注\n\n");
        for n in notes {
            s.push_str(&format!("- {n}\n"));
        }
    }
    if !ignored_keys.is_empty() {
        s.push_str("\n## 被丢弃的键（模板里没有这些列）\n\n");
        for k in ignored_keys {
            s.push_str(&format!("- {k}\n"));
        }
    }
    if !extra.is_empty() {
        s.push_str("\n## 与主表的对账\n\n");
        for e in extra {
            s.push_str(&format!("- {e}\n"));
        }
    }
    s
}

pub fn kind_cn(kind: &str) -> &'static str {
    match kind {
        "number" => "数值（必须确认）",
        "required" => "必填缺失",
        "low_confidence" => "低置信度",
        "duplicate" => "编号重复",
        "reference" => "引用不存在",
        "unknown_field" => "字段不在模板里",
        _ => "其他",
    }
}

/// 清单的机器可读形式。
pub fn uncertain_json(list: &[Uncertain]) -> Value {
    Value::Array(
        list.iter()
            .map(|u| {
                json!({
                    "index": u.index,
                    "field": u.field,
                    "kind": u.kind,
                    "must_confirm": u.must_confirm,
                    "reason": u.reason,
                })
            })
            .collect(),
    )
}

fn cell_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Null => String::new(),
        Value::Bool(b) => {
            if *b {
                "是".into()
            } else {
                "否".into()
            }
        }
        other => other.to_string(),
    }
}

/// 给 sh 用的最小转义（路径里的空格与中文都要能过）。
fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Vec<FieldInfo> {
        vec![
            FieldInfo {
                name: "obs_id".into(),
                label_cn: "记录编号".into(),
                field_type: "text".into(),
                required: true,
                reference: String::new(),
            },
            FieldInfo {
                name: "sample_id".into(),
                label_cn: "关联样本".into(),
                field_type: "text".into(),
                required: true,
                reference: "samples.sample_id".into(),
            },
            FieldInfo {
                name: "value_num".into(),
                label_cn: "测定值".into(),
                field_type: "number".into(),
                required: false,
                reference: String::new(),
            },
        ]
    }

    #[test]
    fn parses_rows_and_flags_numbers_for_confirmation() {
        let answer = serde_json::json!({
            "rows": [
                {"obs_id": "OB-0001", "sample_id": "S-001", "value_num": 62.3},
                {"obs_id": "OB-0002", "sample_id": "S-002", "value_num": ""}
            ],
            "uncertain": [
                {"index": 1, "field": "value_num", "reason": "听不清 62.3 还是 60.3"}
            ],
            "notes": ["用户说有一株异常，没说要记"]
        });
        let p = parse_answer(&answer, &fields()).unwrap();
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[0]["value_num"], "62.3", "数值转文本，不做四舍五入");
        let u = &p.uncertain[0];
        assert_eq!(u.kind, "number");
        assert!(u.must_confirm, "数值必须人工确认");
        assert_eq!(p.notes.len(), 1);
    }

    #[test]
    fn per_cell_confidence_takes_the_lower_of_model_and_rules() {
        let answer = serde_json::json!({
            "rows": [
                // 模型自报 0.4（规则给 0.9）→ 取 0.4，来源 model
                {"obs_id": "OB-1", "sample_id": "S-1", "value_num": "62.3",
                 "confidence": {"value_num": 0.4}},
                // 模型没报 → 只看规则：数字格式对 → 0.9
                {"obs_id": "OB-2", "sample_id": "S-1", "value_num": "33.3"},
                // 模型自报很高，但值里有"可能"→ 规则 0.4 更低，取规则
                {"obs_id": "OB-3", "sample_id": "S-1", "value_num": "可能是64",
                 "confidence": {"value_num": 0.99}},
            ]
        });
        let p = parse_answer(&answer, &fields()).unwrap();
        assert_eq!(p.confidence.len(), 3);
        let c0 = &p.confidence[0]["value_num"];
        assert!((c0.score - 0.4).abs() < 0.001 && c0.source == "model", "{c0:?}");
        let c1 = &p.confidence[1]["value_num"];
        assert!((c1.score - 0.9).abs() < 0.001 && c1.source == "heuristic", "{c1:?}");
        let c2 = &p.confidence[2]["value_num"];
        assert!(
            (c2.score - 0.3).abs() < 0.001,
            "看不清是不是数字 → 0.3（比模型的 0.99 更保守）：{c2:?}"
        );

        // 低置信度清单：上面三个 value_num 都低于 0.7（0.4 / 0.9 / 0.3）→ 只有 0.4 与 0.3 两个
        let low = low_confidence_cells(&p.rows, &p.confidence, LOW_CONFIDENCE);
        assert_eq!(low.len(), 2, "{low:?}");
        assert!(low.iter().all(|u| u.kind == "low_confidence"));
        assert!(low[0].reason.contains("置信度"), "{}", low[0].reason);
    }

    #[test]
    fn confidence_json_is_machine_readable() {
        let answer = serde_json::json!({
            "rows": [{"obs_id": "OB-1", "sample_id": "S-1", "value_num": "1.5"}]
        });
        let p = parse_answer(&answer, &fields()).unwrap();
        let v = confidence_json(&p.rows, &p.confidence);
        assert_eq!(v["format"], "cercis-scribe-confidence/1");
        assert_eq!(v["rows"][0]["index"], 1);
        assert_eq!(v["rows"][0]["cells"]["value_num"]["value"], "1.5");
        assert!(v["rows"][0]["cells"]["value_num"]["score"].as_f64().unwrap() > 0.5);
    }

    #[test]
    fn bare_array_is_accepted_and_missing_required_is_flagged() {
        let answer = serde_json::json!([{"obs_id": "OB-1", "value_num": "12.5"}]);
        let p = parse_answer(&answer, &fields()).unwrap();
        assert_eq!(p.rows.len(), 1);
        assert!(
            p.uncertain
                .iter()
                .any(|u| u.field == "sample_id" && u.kind == "required" && u.must_confirm),
            "{:?}",
            p.uncertain
        );
    }

    #[test]
    fn unknown_keys_are_dropped_and_listed() {
        let answer = serde_json::json!({
            "rows": [{"obs_id": "OB-1", "sample_id": "S-1", "remark": "多出来的"}]
        });
        let p = parse_answer(&answer, &fields()).unwrap();
        assert!(p.rows[0].get("remark").is_none());
        assert_eq!(p.ignored_keys, vec!["remark".to_string()]);
    }

    #[test]
    fn uncertain_pointing_at_missing_row_is_ignored() {
        let answer = serde_json::json!({
            "rows": [{"obs_id": "OB-1", "sample_id": "S-1"}],
            "uncertain": [{"index": 9, "field": "value_num", "reason": "不存在的一行"}]
        });
        let p = parse_answer(&answer, &fields()).unwrap();
        assert!(p.uncertain.iter().all(|u| u.index == 1));
    }

    #[test]
    fn extracts_json_from_noisy_output() {
        let noisy = "好的，这是结果：\n```json\n{\"rows\": [{\"obs_id\": \"OB-1\"}]}\n```\n还需要吗？";
        let v = extract_json(noisy).unwrap();
        assert!(v.get("rows").is_some());
        assert!(extract_json("完全不是 JSON").is_err());
    }
}
