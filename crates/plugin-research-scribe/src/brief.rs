//! 自动提示词（FR-R03 / FR-R06）：**用户不写 prompt**，提示词由项目模板自动渲染。
//!
//! 渲染出来的东西有两份用途：
//! 1. **给模型看的提取提示词**（`prompt.md`）——列有哪些、类型是什么、允许值有哪些、
//!    输出什么 JSON、什么不许做（不得编造、缺失留空并标记）；
//! 2. **给人念的提示脚本**（`script.md`）——"这一条该念哪些字段、单位是什么"，
//!    新手照着念就能录完（FR-R06）。
//!
//! 两者都随模板变化：换模板、改标签、加约束，提示词自动跟着变，
//! 并在会话里记下**模板指纹**，保证"这张草稿是按哪一版模板识别的"说得清。

use std::collections::BTreeMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

/// 渲染结果。
pub struct Brief {
    /// 给模型的提示词
    pub prompt: String,
    /// 给人念的提示脚本
    pub script: String,
    /// 目标表名
    pub table: String,
    /// 模板指纹（sha256，取规范化 JSON 文本）
    pub template_fingerprint: String,
}

/// 模板 JSON + 类型清单 → 提示词与念读脚本。
pub fn render(template: &Value, value_types: &Value, table: Option<&str>) -> Result<Brief, String> {
    let tables = template
        .get("tables")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "模板里没有 tables".to_string())?;
    let chosen = match table {
        Some(want) => tables
            .iter()
            .find(|t| str_of(t, "name") == want)
            .ok_or_else(|| {
                format!(
                    "模板里没有表「{want}」（可用：{}）",
                    tables
                        .iter()
                        .map(|t| str_of(t, "name"))
                        .collect::<Vec<_>>()
                        .join(" / ")
                )
            })?,
        None => tables
            .first()
            .ok_or_else(|| "模板里没有任何表".to_string())?,
    };
    let table_name = str_of(chosen, "name");
    let fields = chosen
        .get("fields")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let types = domain_rules(value_types);
    let fingerprint = fingerprint_of(template);

    let mut p = String::new();
    p.push_str("你是 Cercis 的田间录入员：把用户口述或手写记录整理成结构化数据。\n\n");
    p.push_str(&format!(
        "目标表：{}（{}）\n主键：{}\n",
        table_name,
        str_of(chosen, "label_cn"),
        if str_of(chosen, "primary_key").is_empty() {
            "（无）".to_string()
        } else {
            str_of(chosen, "primary_key")
        }
    ));
    if !str_of(chosen, "notes").is_empty() {
        p.push_str(&format!("表说明：{}\n", str_of(chosen, "notes")));
    }
    p.push_str(&format!("模板指纹：{fingerprint}\n\n"));

    p.push_str("## 字段（只能出现这些键）\n");
    for f in &fields {
        let name = str_of(f, "name");
        let label = str_of(f, "label_cn");
        let ty = str_of(f, "type");
        let mut bits: Vec<String> = Vec::new();
        bits.push(ty.clone());
        // text 不用解释"怎么写"（就是原样文本），其余类型把写法与例子说清楚
        if ty != "text" {
            if let Some(rule) = types.get(&ty) {
                bits.push(rule.clone());
            }
        }
        if !str_of(f, "unit").is_empty() {
            bits.push(format!("单位 {}", str_of(f, "unit")));
        }
        if f.get("required").and_then(|v| v.as_bool()).unwrap_or(false) {
            bits.push("必填".into());
        }
        if let Some(r) = f.get("reference").and_then(|v| v.as_str()) {
            bits.push(format!("必须已存在于 {r}"));
        }
        if !str_of(f, "allowed_values").is_empty() {
            bits.push(format!("只能取：{}", str_of(f, "allowed_values")));
        }
        if let Some(range) = f.get("range").and_then(|v| v.as_array()) {
            let lo = num_text(range.first().and_then(|v| v.as_f64()));
            let hi = num_text(range.get(1).and_then(|v| v.as_f64()));
            bits.push(format!("建议范围 [{lo}, {hi}]"));
        }
        if !str_of(f, "pattern").is_empty() {
            bits.push(format!("格式必须匹配 /{}/", str_of(f, "pattern")));
        }
        if let Some(prec) = f.get("precision").and_then(|v| v.as_u64()) {
            bits.push(format!("最多 {prec} 位小数"));
        }
        if !str_of(f, "notes").is_empty() {
            bits.push(str_of(f, "notes"));
        }
        p.push_str(&format!("- {name}（{label}）：{}\n", bits.join("；")));
    }

    p.push_str("\n## 规则（必须遵守）\n");
    p.push_str("1. **不得编造**：用户没说、手稿上没写的字段，一律留空字符串，不要推测。\n");
    p.push_str("2. **听不清 / 看不清**：该格留空，并在 uncertain 里写清是第几条、哪个字段、为什么。\n");
    p.push_str("3. 数值按原话写（不要换算单位、不要补单位、不要四舍五入）。\n");
    p.push_str("4. 日期写 ISO 8601（YYYY-MM-DD）；时间写 HH:MM。\n");
    p.push_str("5. 分隔与分段：用户说「下一条 / 换行 / 换另一条」表示**一条记录结束**；\n");
    p.push_str("   每条记录对应输出数组里的一项，顺序与用户说的一致。\n");
    p.push_str("6. 只输出 JSON，不要解释、不要 Markdown 代码围栏。\n\n");
    p.push_str("## 输出格式（严格 JSON）\n");
    p.push_str("{\n  \"rows\": [ { \"字段名\": \"值\", ... } ],\n");
    p.push_str("  \"uncertain\": [ { \"index\": 1, \"field\": \"value_num\", \"reason\": \"听不清是 62.3 还是 60.3\" } ],\n");
    p.push_str("  \"notes\": [ \"这一批里用户提到过两株异常的，没说要记\" ]\n}\n");
    p.push_str("\nindex 从 1 开始，对应 rows 里的第几条。\n");

    // 人念的脚本（FR-R06）
    let mut s = String::new();
    s.push_str(&format!(
        "# 念读提示（表：{} / {}）\n\n",
        table_name,
        str_of(chosen, "label_cn")
    ));
    s.push_str("照着念，系统会一条一条记下来。两条之间说「下一条」。\n\n");
    let mut n = 0;
    for f in &fields {
        if f.get("required").and_then(|v| v.as_bool()).unwrap_or(false) {
            n += 1;
            let unit = str_of(f, "unit");
            let unit_hint = if unit.is_empty() {
                String::new()
            } else {
                format!("（{}）", unit)
            };
            s.push_str(&format!(
                "{n}. {}：念 {} 的值{}\n",
                str_of(f, "label_cn"),
                str_of(f, "name"),
                unit_hint
            ));
            if !str_of(f, "allowed_values").is_empty() {
                s.push_str(&format!(
                    "   （只能说：{}）\n",
                    str_of(f, "allowed_values").replace(';', " / ")
                ));
            }
        }
    }
    s.push_str("\n可选字段（有就念，没有就跳过）：\n");
    for f in &fields {
        if !f.get("required").and_then(|v| v.as_bool()).unwrap_or(false) {
            s.push_str(&format!(
                "- {}（{}）\n",
                str_of(f, "label_cn"),
                str_of(f, "name")
            ));
        }
    }
    s.push_str("\n念完一条说「下一条」；说错了说「重念这一条」。\n");

    Ok(Brief {
        prompt: p,
        script: s,
        table: table_name,
        template_fingerprint: fingerprint,
    })
}

/// 模板里的一列（草稿整理与复核共用）。
#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub label_cn: String,
    pub field_type: String,
    pub required: bool,
    pub reference: String,
}

/// 取某张表的字段清单。
pub fn table_fields(template: &Value, table: &str) -> Vec<FieldInfo> {
    template
        .get("tables")
        .and_then(|v| v.as_array())
        .and_then(|ts| ts.iter().find(|t| str_of(t, "name") == table))
        .and_then(|t| t.get("fields"))
        .and_then(|v| v.as_array())
        .map(|fs| {
            fs.iter()
                .map(|f| FieldInfo {
                    name: str_of(f, "name"),
                    label_cn: str_of(f, "label_cn"),
                    field_type: str_of(f, "type"),
                    required: f.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
                    reference: str_of(f, "reference"),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 类型清单 → 类型名到"怎么写"的映射（提示词里说清 number/date 该长什么样）。
fn domain_rules(value_types: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(list) = value_types.get("domains").and_then(|v| v.as_array()) {
        for d in list {
            let name = str_of(d, "name");
            let form = str_of(d, "csv_form");
            let example = d
                .get("examples_ok")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .map(|e| format!("，如 {e}"))
                .unwrap_or_default();
            if !name.is_empty() {
                out.insert(name, format!("{form}{example}"));
            }
        }
    }
    out
}

/// 模板指纹：对规范化 JSON 取 sha256（用于"按哪一版模板识别"）。
pub fn fingerprint_of(template: &Value) -> String {
    let text = canonical(template);
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())[..16].to_string()
}

/// 规范化 JSON：键排序（serde_json 的 Map 默认按 BTreeMap 排，稳定）。
fn canonical(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// 把口述/转写按"下一条 / 换行"口令切段（FR-R04 的第一优先级）。
///
/// 停顿（静音阈值）由语音识别那一侧负责（whisper 等自带 VAD）；
/// 这里只处理**说出来的**分段口令，切完的每一段按顺序交给模型。
pub fn split_segments(transcript: &str) -> Vec<String> {
    const MARKERS: [&str; 6] = ["下一条", "换一行", "换行", "换另一条", "next", "Next"];
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in transcript.lines() {
        let mut rest = line.to_string();
        loop {
            let hit = MARKERS
                .iter()
                .filter_map(|m| rest.find(m).map(|i| (i, m.len())))
                .min_by_key(|(i, _)| *i);
            match hit {
                Some((idx, len)) => {
                    current.push_str(&rest[..idx]);
                    let done = std::mem::take(&mut current);
                    if !done.trim().is_empty() {
                        out.push(done.trim().to_string());
                    }
                    rest = rest[idx + len..].to_string();
                }
                None => {
                    current.push_str(&rest);
                    current.push('\n');
                    break;
                }
            }
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn num_text(v: Option<f64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Value, Value) {
        let template = serde_json::json!({
            "name": "field-trial",
            "tables": [
                {
                    "name": "observations",
                    "label_cn": "测定记录",
                    "primary_key": "obs_id",
                    "notes": "一行 = 一次测定",
                    "fields": [
                        {"name": "obs_id", "label_cn": "记录编号", "type": "text", "required": true},
                        {"name": "sample_id", "label_cn": "关联样本", "type": "text", "required": true, "reference": "samples.sample_id"},
                        {"name": "value_num", "label_cn": "测定值", "type": "number", "precision": 1, "unit": "cm"},
                        {"name": "measured_at", "label_cn": "测定日期", "type": "date"}
                    ]
                }
            ]
        });
        let types = serde_json::json!({
            "domains": [
                {"name": "number", "csv_form": "规范数字写法", "examples_ok": ["12.5"]},
                {"name": "date", "csv_form": "ISO 8601：YYYY-MM-DD", "examples_ok": ["2026-06-15"]}
            ]
        });
        (template, types)
    }

    #[test]
    fn prompt_contains_fields_rules_and_fingerprint() {
        let (t, ty) = fixture();
        let b = render(&t, &ty, None).unwrap();
        assert_eq!(b.table, "observations");
        for needle in [
            "不得编造",
            "uncertain",
            "value_num",
            "最多 1 位小数",
            "必须已存在于 samples.sample_id",
            "规范数字写法",
            "模板指纹",
        ] {
            assert!(b.prompt.contains(needle), "提示词缺 {needle}：\n{}", b.prompt);
        }
        assert!(b.script.contains("照着念"), "{}", b.script);
        assert!(b.script.contains("记录编号"));
        assert_eq!(b.template_fingerprint.len(), 16);
    }

    #[test]
    fn prompt_changes_with_template() {
        let (t, ty) = fixture();
        let a = render(&t, &ty, None).unwrap();
        let mut t2 = t.clone();
        t2["tables"][0]["fields"][2]["label_cn"] = serde_json::json!("株高");
        let b = render(&t2, &ty, None).unwrap();
        assert_ne!(a.prompt, b.prompt, "换模板后提示词应变化");
        assert_ne!(a.template_fingerprint, b.template_fingerprint);
    }

    #[test]
    fn segments_split_on_voice_commands() {
        let text = "小区U-001 株高62.3 下一条 小区U-002 株高64，还有一株60.3 换行 小区U-003 株高61.1";
        let seg = split_segments(text);
        assert_eq!(seg.len(), 3, "{seg:?}");
        assert!(seg[0].contains("U-001"));
        assert!(seg[1].contains("U-002"));
        assert!(seg[2].contains("U-003"));
    }

    #[test]
    fn empty_transcript_gives_no_segments() {
        assert!(split_segments("").is_empty());
        assert!(split_segments("  \n ").is_empty());
    }
}
