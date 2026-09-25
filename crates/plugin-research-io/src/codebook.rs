//! 数据说明书（codebook，FR-F06）：**半年后不看代码也能读懂这批数据**。
//!
//! 内容：
//! - 项目与导出时刻、内核版本、类型规则版本（"当时按哪一版规则判的"）
//! - 每张表：主键、每列的显示名、类型、单位、必填、引用、允许值、范围、模式、小数位数
//! - **数据现状**：非空格数、空格数、占位符写法数；数值列给最小/最大，日期列给区间，
//!   分类列给取值标签（≤20 个时列全）
//! - 每张表的 sha256 指纹（来自内核清单，便于把论文图表对回确切版本）

use std::path::Path;

use serde_json::{json, Value};

use crate::kernel::Kernel;
use crate::{fail, R};

/// 生成说明书：返回 (Markdown, 参与的表清单)
pub fn build(kernel: &Kernel, project: &Path, only: Option<&str>) -> R<(String, Vec<Value>)> {
    let template = kernel.template(project)?;
    let contract = kernel.value_types()?;
    let manifest = kernel.manifest(project).unwrap_or(Value::Null);

    let mut md = String::new();
    md.push_str("# 数据说明书（codebook）\n\n");
    md.push_str(&format!(
        "- 项目：{}\n- 生成时间：{}\n- 内核版本：{}\n- 类型规则版本：{}\n",
        manifest
            .get("project_name")
            .and_then(|v| v.as_str())
            .unwrap_or("（未知）"),
        chrono::Utc::now().to_rfc3339(),
        contract
            .get("core_version")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        contract
            .get("schema")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    ));
    if let Some(rule) = contract.get("missing_value_rule").and_then(|v| v.as_str()) {
        md.push_str(&format!("- 缺失值规则：{rule}\n"));
    }
    md.push_str(
        "\n> 类型与校验规则由内核统一定义（见 `cercis value-types --json`），本说明书照它生成。\n",
    );

    let tables = template
        .get("tables")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut used: Vec<Value> = Vec::new();

    for t in &tables {
        let name = str_of(t, "name");
        if let Some(want) = only {
            if &name != want {
                continue;
            }
        }
        let rows = kernel.rows(project, &name).unwrap_or_default();
        let fields = t
            .get("fields")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let fingerprint = manifest
            .get("tables")
            .and_then(|v| v.as_array())
            .and_then(|a| a.iter().find(|x| str_of(x, "name") == name))
            .map(|x| str_of(x, "sha256"))
            .unwrap_or_default();

        md.push_str(&format!(
            "\n---\n\n## 表：{}（{}）\n\n",
            name,
            str_of(t, "label_cn")
        ));
        md.push_str(&format!(
            "- 行数：{}（不含表头）\n- 主键：{}\n- 内容指纹（sha256）：{}\n",
            rows.len(),
            if str_of(t, "primary_key").is_empty() {
                "（无）".into()
            } else {
                str_of(t, "primary_key")
            },
            if fingerprint.is_empty() {
                "（未取到）"
            } else {
                &fingerprint
            }
        ));
        if !str_of(t, "notes").is_empty() {
            md.push_str(&format!("- 说明：{}\n", str_of(t, "notes")));
        }

        md.push_str("\n| 列名 | 中文名 | 类型 | 单位 | 必填 | 约束 | 数据现状 |\n");
        md.push_str("|---|---|---|---|---|---|---|\n");
        for f in &fields {
            let col = str_of(f, "name");
            let stats = column_stats(&rows, &col, &str_of(f, "type"));
            let mut constraints: Vec<String> = Vec::new();
            if let Some(r) = f.get("reference").and_then(|v| v.as_str()) {
                constraints.push(format!("引用 {r}"));
            }
            if !str_of(f, "allowed_values").is_empty() {
                constraints.push(format!("允许值：{}", str_of(f, "allowed_values")));
            }
            if let Some(range) = f.get("range").and_then(|v| v.as_array()) {
                let lo = num_text(range.first().and_then(|v| v.as_f64()));
                let hi = num_text(range.get(1).and_then(|v| v.as_f64()));
                constraints.push(format!("建议范围 [{lo}, {hi}]"));
            }
            if !str_of(f, "pattern").is_empty() {
                constraints.push(format!("格式 /{}/", str_of(f, "pattern")));
            }
            if let Some(p) = f.get("precision").and_then(|v| v.as_u64()) {
                constraints.push(format!("{p} 位小数"));
            }
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                col,
                str_of(f, "label_cn"),
                str_of(f, "type"),
                str_of(f, "unit"),
                if f.get("required").and_then(|v| v.as_bool()).unwrap_or(false) {
                    "是"
                } else {
                    ""
                },
                constraints.join("；"),
                stats.summary,
            ));
        }

        used.push(json!({
            "name": name,
            "label_cn": str_of(t, "label_cn"),
            "rows": rows.len(),
            "sha256": fingerprint,
            "columns": fields.len(),
        }));
    }

    if used.is_empty() {
        return fail(
            format!(
                "没有可写入说明书的表（模板里有 {} 张）",
                tables.len()
            ),
            "确认 --table 写的是模板里的表名",
        );
    }
    Ok((md, used))
}

struct ColumnStats {
    summary: String,
}

/// 一列的数据现状：非空/空/占位符 + 数值区间、日期区间、取值标签。
fn column_stats(rows: &[crate::row::Row], column: &str, field_type: &str) -> ColumnStats {
    let mut non_empty = 0usize;
    let mut missing_markers = 0usize;
    let mut values: Vec<&str> = Vec::new();
    let mut distinct: Vec<String> = Vec::new();
    for r in rows {
        let v = r.get(column).map(|s| s.as_str()).unwrap_or("").trim();
        if v.is_empty() {
            continue;
        }
        non_empty += 1;
        if matches!(v, "NA" | "N/A" | "na" | "null" | "NULL" | "NaN" | "-" | "--" | "?" | ".") {
            missing_markers += 1;
        }
        values.push(v);
        if distinct.len() <= 20 {
            let owned = v.to_string();
            if !distinct.contains(&owned) {
                distinct.push(owned);
            }
        }
    }
    let empty = rows.len() - non_empty;
    let mut parts: Vec<String> = vec![format!("填了 {non_empty}／{empty} 空")];
    if missing_markers > 0 {
        parts.push(format!("占位符写法 {missing_markers}"));
    }

    match field_type {
        "number" | "integer" => {
            let nums: Vec<f64> = values.iter().filter_map(|v| v.parse::<f64>().ok()).collect();
            if let (Some(min), Some(max)) = (
                nums.iter().cloned().reduce(f64::min),
                nums.iter().cloned().reduce(f64::max),
            ) {
                parts.push(format!("范围 {min}–{max}"));
            }
        }
        "date" => {
            let mut ds: Vec<&str> = values
                .iter()
                .filter(|v| chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok())
                .cloned()
                .collect();
            ds.sort();
            if let (Some(first), Some(last)) = (ds.first(), ds.last()) {
                parts.push(format!("{first} 至 {last}"));
            }
        }
        _ => {
            if distinct.len() <= 20 && !values.is_empty() {
                if distinct.len() <= 8 {
                    parts.push(format!("取值：{}", distinct.join(" / ")));
                } else {
                    parts.push(format!(
                        "取值 {} 种：{} …",
                        distinct.len(),
                        distinct.iter().take(5).cloned().collect::<Vec<_>>().join(" / ")
                    ));
                }
            } else if !values.is_empty() {
                parts.push(format!("取值 {} 种以上", distinct.len()));
            }
        }
    }

    ColumnStats {
        summary: parts.join("；"),
    }
}

fn num_text(v: Option<f64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "".into())
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, &str)]) -> crate::row::Row {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn stats_count_empty_and_missing_markers() {
        let rows = vec![
            row(&[("x", "1")]),
            row(&[("x", "")]),
            row(&[("x", "NA")]),
        ];
        let s = column_stats(&rows, "x", "number");
        assert!(s.summary.contains("填了 2／1 空"), "{}", s.summary);
        assert!(s.summary.contains("占位符"), "{}", s.summary);
    }

    #[test]
    fn numeric_and_date_ranges() {
        let rows = vec![
            row(&[("v", "1.5")]),
            row(&[("v", "3")]),
            row(&[("d", "2026-06-15")]),
            row(&[("d", "2026-06-01")]),
        ];
        assert!(column_stats(&rows, "v", "number").summary.contains("范围 1.5–3"));
        assert!(column_stats(&rows, "d", "date")
            .summary
            .contains("2026-06-01 至 2026-06-15"));
    }

    #[test]
    fn text_shows_value_labels() {
        let rows = vec![row(&[("t", "是")]), row(&[("t", "否")]), row(&[("t", "是")])];
        let s = column_stats(&rows, "t", "bool");
        assert!(s.summary.contains("是") && s.summary.contains("否"), "{}", s.summary);
    }
}
