//! 复核与入库（FR-R05 / FR-R08）：**草稿永远先过一遍内核，数值先经人眼**。
//!
//! 复核做三件事：
//! 1. 把模型报的"听不清"、必填缺失、数值项列成待确认清单；
//! 2. 把草稿交给内核**试运行**（类型、必填、引用、允许值、主键），
//!    内核说不行的地方也进清单——插件不自己判类型（§5.9 / D36）；
//! 3. 拿主表已有数据对账：编号是否重复（FR-R05）。

use std::path::Path;

use serde_json::{json, Value};

use crate::kernel::Kernel;
use crate::model::{review_markdown, uncertain_json, Uncertain};
use crate::row::{self, Row};
use crate::{Fail, R};

/// 复核结果。
pub struct Review {
    pub rows: Vec<Row>,
    pub columns: Vec<String>,
    pub uncertain: Vec<Uncertain>,
    pub blocking: Vec<String>,
    pub warnings: Vec<String>,
    pub markdown: String,
    pub draft_path: std::path::PathBuf,
}

/// 复核一个会话的草稿。
pub fn review(
    kernel: &Kernel,
    project: &Path,
    workspace: &crate::session::Session,
    table: &str,
) -> R<Review> {
    let session_id = workspace.id.as_str();
    let draft_path = &workspace.path(&format!("{table}.draft.csv"));
    let (columns, rows) = row::read_csv(draft_path).map_err(|e| {
        Fail::new(
            format!("读不到草稿表 {}：{e}", draft_path.display()),
            "草稿是普通 CSV；用 Excel 改过也能读，但别改列名",
        )
    })?;
    if rows.is_empty() {
        return Err(Fail::new(
            "草稿表里没有数据行",
            "先做一次识别（voice / photo），或把模型返回的 JSON 用 --from-json 补进来",
        ));
    }

    // 1) 内核试运行：类型/必填/允许值/引用/主键，一次说清
    let (code, dry) = kernel.append_many(
        project,
        table,
        &rows,
        &format!("草稿复核（会话 {session_id}）"),
        &format!("scribe:{session_id}"),
        true,
    )?;
    let mut blocking: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut from_kernel: Vec<Uncertain> = Vec::new();
    if code == 0 {
        for e in dry.get("errors").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            let row_no = e.get("row").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let field = e
                .get("field")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let message = e
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let kind = match e.get("kind").and_then(|v| v.as_str()).unwrap_or("") {
                "reference" => "reference",
                "primary_key" => "duplicate",
                _ => "other",
            };
            blocking.push(format!("第 {row_no} 条 {field}：{message}"));
            from_kernel.push(Uncertain {
                index: row_no,
                field,
                reason: message,
                kind: kind.into(),
                must_confirm: true,
            });
        }
        for w in dry.get("warnings").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            warnings.push(
                w.get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
        }
    } else {
        blocking.push(format!(
            "内核试运行失败：{}",
            dry.get("error").and_then(|v| v.as_str()).unwrap_or("原因不明")
        ));
    }

    // 2) 模型报的待确认项（草稿旁边那份 JSON；丢了也不报错，只是清单变短）
    let uncertain = load_uncertain(&workspace.path(&format!("{table}.uncertain.json")))?;

    // 3) 与主表对账：编号重复 + 主表指纹（论文要能对回确切版本）
    let mut extra: Vec<String> = Vec::new();
    let manifest_template = kernel.template(project)?;
    let primary_key = primary_key_of(&manifest_template, table);
    if !primary_key.is_empty() {
        let existing = kernel.rows(project, table).unwrap_or_default();
        let existing_keys: Vec<String> = existing
            .iter()
            .filter_map(|r| r.get(&primary_key).cloned())
            .collect();
        let mut dup: Vec<String> = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            let key = r.get(&primary_key).cloned().unwrap_or_default();
            if key.trim().is_empty() {
                continue;
            }
            if existing_keys.contains(&key) && !dup.contains(&format!("第 {} 条 {key}", i + 1)) {
                dup.push(format!("第 {} 条 {key}", i + 1));
            }
        }
        if !dup.is_empty() {
            extra.push(format!(
                "编号与主表重复：{}（数据只增不改，主表里已有的编号不能再用）",
                dup.join("、")
            ));
        }
        let fingerprint = kernel
            .manifest(project)
            .ok()
            .and_then(|m| {
                m.get("tables").and_then(|v| v.as_array()).and_then(|a| {
                    a.iter()
                        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(table))
                        .map(|t| {
                            t.get("sha256")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .chars()
                                .take(16)
                                .collect::<String>()
                        })
                })
            })
            .unwrap_or_default();
        extra.push(format!(
            "主表 {table} 现有 {} 条记录（指纹 {}）",
            existing.len(),
            if fingerprint.is_empty() {
                "未取到"
            } else {
                &fingerprint
            }
        ));
    }

    let mut all_uncertain = uncertain;
    all_uncertain.extend(from_kernel);

    let markdown = review_markdown(
        session_id,
        table,
        rows.len(),
        &all_uncertain,
        &[],
        &[],
        &extra,
    );
    Ok(Review {
        rows,
        columns,
        uncertain: all_uncertain,
        blocking,
        warnings,
        markdown,
        draft_path: draft_path.to_path_buf(),
    })
}

/// 会话里记着的待确认清单（模型返回时的 uncertain 部分）。
fn load_uncertain(side: &std::path::Path) -> R<Vec<Uncertain>> {
    let Ok(text) = std::fs::read_to_string(side) else {
        return Ok(Vec::new());
    };
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let mut out = Vec::new();
    if let Some(list) = v.as_array() {
        for u in list {
            out.push(Uncertain {
                index: u.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
                field: u
                    .get("field")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                reason: u
                    .get("reason")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                kind: u
                    .get("kind")
                    .and_then(|x| x.as_str())
                    .unwrap_or("low_confidence")
                    .to_string(),
                must_confirm: u
                    .get("must_confirm")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false),
            });
        }
    }
    Ok(out)
}

/// 复核结果 → JSON。
pub fn review_json(r: &Review, session_id: &str, table: &str) -> Value {
    json!({
        "schema": "cercis.plugin/1",
        "ok": r.blocking.is_empty(),
        "action": "research.scribe.review",
        "session": session_id,
        "table": table,
        "draft": r.draft_path.display().to_string(),
        "rows": r.rows.len(),
        "columns": r.columns.len(),
        "must_confirm": r.uncertain.iter().filter(|u| u.must_confirm).count(),
        "low_confidence": r.uncertain.iter().filter(|u| u.kind == "low_confidence").count(),
        "uncertain": uncertain_json(&r.uncertain),
        "blocking": r.blocking,
        "warnings": r.warnings,
    })
}

/// 复核结果 → 人读文本。
pub fn review_human(r: &Review, session_id: &str, table: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "会话 {session_id}｜草稿表 {table}｜{} 条记录\n",
        r.rows.len()
    ));
    let must = r.uncertain.iter().filter(|u| u.must_confirm).count();
    let low = r
        .uncertain
        .iter()
        .filter(|u| u.kind == "low_confidence")
        .count();
    if r.uncertain.is_empty() {
        s.push_str("待确认清单：空（模型没报不确定项，内核也没报错）\n");
    } else {
        s.push_str(&format!(
            "待确认 {} 项（其中必须人工确认 {} 项，低置信度 {} 个格子）：\n",
            r.uncertain.len(),
            must,
            low
        ));
        for u in &r.uncertain {
            s.push_str(&format!(
                "  {}第 {} 条 {}：{}\n",
                if u.must_confirm { "⚠ " } else { "  " },
                u.index,
                u.field,
                u.reason
            ));
        }
    }
    if !r.blocking.is_empty() {
        s.push_str("\n内核说这些格子进不去（改完再入库）：\n");
        for b in &r.blocking {
            s.push_str(&format!("  ✗ {b}\n"));
        }
    }
    for w in &r.warnings {
        s.push_str(&format!("  ! {w}\n"));
    }
    s.push_str(&format!(
        "\n草稿：{}\n确认无误后：scribe commit --session {session_id} --confirm\n",
        r.draft_path.display()
    ));
    s
}

/// 入库（FR-R08）：先复核，再经内核 append-many 写入。
pub fn commit(
    kernel: &Kernel,
    project: &Path,
    workspace: &crate::session::Session,
    table: &str,
    note: &str,
) -> R<Value> {
    let session_id = workspace.id.as_str();
    let r = review(kernel, project, workspace, table)?;
    if !r.blocking.is_empty() {
        return Err(Fail::new(
            format!(
                "内核试运行没通过，未写入任何数据：\n- {}",
                r.blocking.join("\n- ")
            ),
            "在草稿表里改掉这些格子（或让用户重念），然后再 commit",
        ));
    }
    let summary = format!(
        "田间录入员入库（会话 {session_id}）：{} 条{}",
        r.rows.len(),
        if note.trim().is_empty() {
            String::new()
        } else {
            format!("；说明：{}", note.trim())
        }
    );
    let (code, out) = kernel.append_many(
        project,
        table,
        &r.rows,
        &summary,
        &format!("scribe:{session_id}"),
        false,
    )?;
    if code != 0 {
        return Err(Fail::new(
            format!(
                "内核拒绝写入：{}",
                out.get("error").and_then(|v| v.as_str()).unwrap_or("原因不明")
            ),
            "看内核的报错逐条改草稿；数据没有写进去",
        ));
    }
    let batch = out
        .get("batch_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    workspace.patch_meta(json!({
        "status": "committed",
        "committed_at": chrono::Utc::now().to_rfc3339(),
        "committed_rows": r.rows.len(),
        "batch_id": batch,
        "confirm_note": note,
        "must_confirm_at_commit": r.uncertain.iter().filter(|u| u.must_confirm).count(),
    }))?;
    Ok(json!({
        "schema": "cercis.plugin/1",
        "ok": true,
        "action": "research.scribe.commit",
        "session": session_id,
        "table": table,
        "rows_written": r.rows.len(),
        "batch_id": batch,
        "confirmed": r.uncertain.iter().filter(|u| u.must_confirm).count(),
    }))
}

fn primary_key_of(template: &Value, table: &str) -> String {
    template
        .get("tables")
        .and_then(|v| v.as_array())
        .and_then(|ts| ts.iter().find(|t| t.get("name").and_then(|v| v.as_str()) == Some(table)))
        .and_then(|t| t.get("primary_key"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}
