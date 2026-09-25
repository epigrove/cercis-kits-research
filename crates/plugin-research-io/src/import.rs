//! Excel / CSV 记录表导入（FR-E01）。
//!
//! 流程：读源文件 → 认表 → 列映射（自动 + `--map` 手工覆盖）→
//! **问内核每列该是什么类型** → 试运行（内核判错）→ 入库（一次事务、一条流水）。
//!
//! 为什么每一步都要先"试运行"：真实记录表里 90% 的问题（错列、空行、单位写进数值列、
//! 编号多打一个空格）都能在这一步看出来，而**写进去再改要留更正记录**——不如先看清楚。

use std::path::Path;

use serde_json::{json, Value};

use crate::kernel::Kernel;
use crate::row::{read_any, Row};
use crate::{fail, R};
use crate::ImportArgs;

/// 模板里的一个字段（只取导入需要的部分）。
#[derive(Debug, Clone)]
struct Field {
    name: String,
    label_cn: String,
    label_en: String,
    field_type: String,
    required: bool,
}

/// 一张表的定义。
#[derive(Debug, Clone)]
struct TableDef {
    name: String,
    label_cn: String,
    primary_key: String,
    fields: Vec<Field>,
}

/// 列映射结果。
struct Mapping {
    /// (源列, 目标列)
    pairs: Vec<(String, String)>,
    /// 源文件里被忽略的列
    skipped: Vec<String>,
    /// 模板里没能填上的列
    unfilled: Vec<String>,
}

pub fn import(kernel: &Kernel, args: &ImportArgs) -> R<Value> {
    if !args.file.is_file() {
        return fail(
            format!("导入文件不存在：{}", args.file.display()),
            "确认路径；Excel 请给 .xlsx/.xls/.ods，逗号分隔给 .csv/.tsv",
        );
    }
    let input = read_any(&args.file, args.sheet.as_deref())?;
    if input.rows.is_empty() {
        return fail(
            format!("{} 里没有数据行", args.file.display()),
            "首行应为列名，其后是数据",
        );
    }

    let template = kernel.template(&args.project)?;
    let tables = parse_tables(&template);
    let table = match &args.table {
        Some(name) => tables
            .iter()
            .find(|t| &t.name == name)
            .cloned()
            .ok_or_else(|| {
                crate::Fail::new(
                    format!("模板里没有表「{name}」"),
                    format!(
                        "可用表：{}",
                        tables
                            .iter()
                            .map(|t| t.name.as_str())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    ),
                )
            })?,
        None => guess_table(&tables, &input.headers)?,
    };

    let mapping = build_mapping(
        &input.headers,
        &table,
        &args.maps,
        args.skip_unknown,
        args.generate_id.is_some(),
    )?;
    let mut rows = map_rows(&input.rows, &mapping);
    let mut generated: Vec<String> = Vec::new();

    // 手写记录表通常没有记录编号：允许按前缀自动生成（避免与已有编号撞车）
    if let Some(prefix) = &args.generate_id {
        let key = table.primary_key.clone();
        if key.is_empty() {
            return fail(
                format!("表 {} 没有主键，--generate-id 用不上", table.name),
                "去掉 --generate-id，或给源文件补上主键列",
            );
        }
        if !mapping.pairs.iter().any(|(_, to)| to == &key) {
            let start = next_id_start(kernel, &args.project, &table.name, &key, prefix);
            for (i, r) in rows.iter_mut().enumerate() {
                let id = format!("{prefix}-{:04}", start + i);
                r.insert(key.clone(), id.clone());
                generated.push(id);
            }
        }
    }
    let headers: Vec<String> = mapping.pairs.iter().map(|(_, to)| to.clone()).collect();
    let mut headers = headers;
    if !generated.is_empty() && !headers.contains(&table.primary_key) {
        headers.insert(0, table.primary_key.clone());
    }

    // 类型建议：把映射后的行交给内核（插件不自己判，见 §5.9 / D36）
    let suggestions = kernel.infer_types(&headers, &rows)?;

    // 试运行：内核按模板校验这批行（结构错误会列出来）
    let summary = format!(
        "导入 {} 行到表 {}（源：{}）",
        rows.len(),
        table.name,
        args.file.display()
    );
    let source_ref = args.file.display().to_string();
    let dry = kernel.append_many(
        &args.project,
        &table.name,
        &rows,
        "import",
        &summary,
        &source_ref,
        args.allow_new_columns,
        true,
    )?;
    let errors = str_list(&dry, "errors");
    let warnings = str_list(&dry, "warnings");

    let mut batch_id = String::new();
    if !args.dry_run {
        if !errors.is_empty() {
            return fail(
                format!(
                    "试运行发现 {} 处错误，未写入任何数据：\n- {}",
                    errors.len(),
                    errors.join("\n- ")
                ),
                "按上面的行号修正源文件，或用 --skip-unknown / --map 调整映射后重试",
            );
        }
        let written = kernel.append_many(
            &args.project,
            &table.name,
            &rows,
            "import",
            &summary,
            &source_ref,
            args.allow_new_columns,
            false,
        )?;
        batch_id = written
            .get("batch_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }

    // 模板声明的类型 vs 数据看起来的类型：只在真的对不上时提示（整数是数值的子集，不算冲突）
    let mismatches = type_mismatches(&table, &suggestions);

    let out = json!({
        "schema": "cercis.plugin/1",
        "ok": errors.is_empty(),
        "action": "research.import-xlsx",
        "table": table.name,
        "label_cn": table.label_cn,
        "primary_key": table.primary_key,
        "source": input.source,
        "sheet": input.sheet,
        "dry_run": args.dry_run,
        "rows_in_file": input.rows.len(),
        "rows_mapped": rows.len(),
        "mapping": mapping.pairs.iter().map(|(f, t)| json!({"from": f, "to": t})).collect::<Vec<_>>(),
        "skipped_columns": mapping.skipped,
        "unfilled_columns": mapping.unfilled,
        "type_suggestions": suggestions,
        "type_mismatches": mismatches,
        "generated_ids": generated,
        "errors": errors,
        "warnings": warnings,
        "batch_id": batch_id,
    });

    if let Some(path) = &args.report {
        std::fs::write(path, report_markdown(&out))?;
    }
    Ok(out)
}

/// 人读输出。
pub fn human(out: &Value) -> String {
    let mut s = String::new();
    let table = out.get("table").and_then(|v| v.as_str()).unwrap_or("");
    let label = out.get("label_cn").and_then(|v| v.as_str()).unwrap_or("");
    let dry = out.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(true);
    s.push_str(&format!(
        "{}表 {table}（{label}）｜读入 {} 行，映射后 {} 行\n",
        if dry { "试运行：" } else { "导入：" },
        out.get("rows_in_file").and_then(|v| v.as_u64()).unwrap_or(0),
        out.get("rows_mapped").and_then(|v| v.as_u64()).unwrap_or(0),
    ));

    s.push_str("\n列映射：\n");
    if let Some(m) = out.get("mapping").and_then(|v| v.as_array()) {
        for pair in m {
            s.push_str(&format!(
                "  {} → {}\n",
                pair.get("from").and_then(|v| v.as_str()).unwrap_or(""),
                pair.get("to").and_then(|v| v.as_str()).unwrap_or("")
            ));
        }
    }
    for key in ["skipped_columns", "unfilled_columns"] {
        if let Some(list) = out.get(key).and_then(|v| v.as_array()) {
            if !list.is_empty() {
                let names: Vec<&str> = list.iter().filter_map(|v| v.as_str()).collect();
                s.push_str(&format!(
                    "{}：{}\n",
                    if key == "skipped_columns" {
                        "忽略的源列"
                    } else {
                        "模板里没填的列"
                    },
                    names.join("、")
                ));
            }
        }
    }
    if let Some(ids) = out.get("generated_ids").and_then(|v| v.as_array()) {
        if !ids.is_empty() {
            s.push_str(&format!(
                "自动生成的记录编号：{} 到 {}（共 {} 个）\n",
                ids.first().and_then(|v| v.as_str()).unwrap_or(""),
                ids.last().and_then(|v| v.as_str()).unwrap_or(""),
                ids.len()
            ));
        }
    }

    if let Some(sug) = out.get("type_suggestions").and_then(|v| v.as_array()) {
        if !sug.is_empty() {
            s.push_str("\n类型建议（内核算的；只建议、不改模板）：\n");
            for c in sug {
                let col = c.get("column").and_then(|v| v.as_str()).unwrap_or("");
                let rec = c
                    .get("profile")
                    .and_then(|p| p.get("recommended"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("（无强建议）");
                let notes = c
                    .get("profile")
                    .and_then(|p| p.get("notes"))
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join("；")
                    })
                    .unwrap_or_default();
                s.push_str(&format!("  {col} → {rec}"));
                if !notes.is_empty() {
                    s.push_str(&format!("（{notes}）"));
                }
                s.push('\n');
            }
        }
    }

    let errors = out.get("errors").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let warnings = out.get("warnings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if let Some(mm) = out.get("type_mismatches").and_then(|v| v.as_array()) {
        if !mm.is_empty() {
            s.push_str("\n类型对账（模板声明 vs 数据看起来）：\n");
            for m in mm {
                s.push_str(&format!(
                    "  {}：模板说 {}，数据像 {}（照模板校验；要改先 `cercis set-field-type … --dry-run`）\n",
                    m.get("column").and_then(|v| v.as_str()).unwrap_or(""),
                    m.get("declared").and_then(|v| v.as_str()).unwrap_or(""),
                    m.get("suggested").and_then(|v| v.as_str()).unwrap_or("")
                ));
            }
        }
    }
    if errors.is_empty() {
        s.push_str("\n校验：没有结构错误");
        if warnings.is_empty() {
            s.push_str("，也没有提醒。\n");
        } else {
            s.push_str(&format!("（{} 处提醒）\n", warnings.len()));
        }
    } else {
        s.push_str(&format!("\n校验：{} 处错误（这些会让整批写不进去）\n", errors.len()));
        for e in errors.iter().take(10) {
            s.push_str(&format!("  错误 {e}\n"));
        }
        if errors.len() > 10 {
            s.push_str(&format!("  ……还有 {} 处\n", errors.len() - 10));
        }
    }
    for w in warnings.iter().take(10) {
        s.push_str(&format!("  提醒 {w}\n"));
    }

    if dry {
        s.push_str("\n（未写入任何数据；去掉 --dry-run 即正式导入）\n");
    } else if let Some(id) = out.get("batch_id").and_then(|v| v.as_str()) {
        if !id.is_empty() {
            s.push_str(&format!("\n已写入（一次事务、一条流水；批次 {id}）\n"));
        }
    }
    s
}

fn report_markdown(out: &Value) -> String {
    let mut s = String::from("# 导入报告\n\n");
    s.push_str(&format!(
        "- 来源：{}\n- 目标表：{}\n- 时间：{}\n",
        out.get("source").and_then(|v| v.as_str()).unwrap_or(""),
        out.get("table").and_then(|v| v.as_str()).unwrap_or(""),
        chrono::Utc::now().to_rfc3339(),
    ));
    s.push_str(&format!(
        "- 读入 {} 行，映射后 {} 行{}\n",
        out.get("rows_in_file").and_then(|v| v.as_u64()).unwrap_or(0),
        out.get("rows_mapped").and_then(|v| v.as_u64()).unwrap_or(0),
        if out.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(true) {
            "（试运行，未写入）"
        } else {
            "（已写入）"
        }
    ));
    s.push_str("\n## 列映射\n\n| 源列 | 目标列 |\n|---|---|\n");
    if let Some(m) = out.get("mapping").and_then(|v| v.as_array()) {
        for pair in m {
            s.push_str(&format!(
                "| {} | {} |\n",
                pair.get("from").and_then(|v| v.as_str()).unwrap_or(""),
                pair.get("to").and_then(|v| v.as_str()).unwrap_or("")
            ));
        }
    }
    let errors = out.get("errors").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if !errors.is_empty() {
        s.push_str("\n## 未通过校验的格子（这是**没写进去**的原因）\n\n");
        for e in &errors {
            s.push_str(&format!("- {}\n", e.as_str().unwrap_or("")));
        }
    }
    s
}

/// 模板 JSON → 表定义。
fn parse_tables(template: &Value) -> Vec<TableDef> {
    template
        .get("tables")
        .and_then(|v| v.as_array())
        .map(|tables| {
            tables
                .iter()
                .map(|t| TableDef {
                    name: str_of(t, "name"),
                    label_cn: str_of(t, "label_cn"),
                    primary_key: str_of(t, "primary_key"),
                    fields: t
                        .get("fields")
                        .and_then(|v| v.as_array())
                        .map(|fields| {
                            fields
                                .iter()
                                .map(|f| Field {
                                    name: str_of(f, "name"),
                                    label_cn: str_of(f, "label_cn"),
                                    label_en: str_of(f, "label_en"),
                                    field_type: str_of(f, "type"),
                                    required: f
                                        .get("required")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 没指定表时按列名猜一张最像的（源列命中数最多的那张）。
fn guess_table(tables: &[TableDef], headers: &[String]) -> R<TableDef> {
    let mut best: Option<(usize, &TableDef)> = None;
    for t in tables {
        let hits = headers
            .iter()
            .filter(|h| t.fields.iter().any(|f| matches_header(h, f)))
            .count();
        if best.map(|(n, _)| hits > n).unwrap_or(true) {
            best = Some((hits, t));
        }
    }
    match best {
        Some((hits, t)) if hits > 0 => Ok(t.clone()),
        _ => fail(
            "认不出这些列属于哪张表".to_string(),
            format!(
                "用 --table 指定表（可用：{}），或用 --map 源列=目标列 手工映射",
                tables
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        ),
    }
}

/// 源列名是否对得上某个字段（列名、中文标签、英文标签；忽略大小写与空格）。
fn matches_header(header: &str, f: &Field) -> bool {
    let norm = |s: &str| s.trim().to_lowercase().replace([' ', '_', '-'], "");
    let h = norm(header);
    !h.is_empty() && (h == norm(&f.name) || h == norm(&f.label_cn) || h == norm(&f.label_en))
}

fn build_mapping(
    headers: &[String],
    table: &TableDef,
    maps: &[String],
    skip_unknown: bool,
    generating_primary_key: bool,
) -> R<Mapping> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let manual: Vec<(String, String)> = maps
        .iter()
        .map(|m| {
            m.split_once('=')
                .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                .ok_or_else(|| {
                    crate::Fail::new(
                        format!("--map 要写成 源列=目标列，实际：{m}"),
                        "例如 --map 株高cm=plant_height_cm",
                    )
                })
        })
        .collect::<R<Vec<_>>>()?;

    let mut unknown: Vec<String> = Vec::new();
    for h in headers {
        if let Some((_, to)) = manual.iter().find(|(from, _)| from == h) {
            if !table.fields.iter().any(|f| &f.name == to) {
                return fail(
                    format!("映射目标「{to}」不在表 {} 里", table.name),
                    format!(
                        "可用列：{}",
                        table
                            .fields
                            .iter()
                            .map(|f| f.name.as_str())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    ),
                );
            }
            pairs.push((h.clone(), to.clone()));
            continue;
        }
        match table.fields.iter().find(|f| matches_header(h, f)) {
            Some(f) => pairs.push((h.clone(), f.name.clone())),
            None => {
                unknown.push(h.clone());
                skipped.push(h.clone());
            }
        }
    }

    let missing_required: Vec<&Field> = table
        .fields
        .iter()
        .filter(|f| f.required && !pairs.iter().any(|(_, to)| to == &f.name))
        .collect();
    let unfilled_required: Vec<&str> = missing_required
        .iter()
        .filter(|f| !(generating_primary_key && f.name == table.primary_key))
        .map(|f| f.name.as_str())
        .collect();
    if !unfilled_required.is_empty() {
        let hint = if missing_required.iter().any(|f| f.name == table.primary_key)
            && !generating_primary_key
        {
            "用 --map 源列=目标列 指过去；手写记录表没有编号的话，加 --generate-id 前缀（如 --generate-id OB）自动生成"
        } else {
            "用 --map 源列=目标列 指过去，或先在源文件里补上这些列"
        };
        return fail(
            format!(
                "必填列没有数据来源：{}",
                unfilled_required.join("、")
            ),
            hint,
        );
    }

    if !unknown.is_empty() && !skip_unknown {
        return fail(
            format!(
                "源文件里有 {} 列在表 {} 里找不到对应：{}",
                unknown.len(),
                table.name,
                unknown.join("、")
            ),
            "确认这些列要不要；不要就用 --skip-unknown 忽略，要就先把列加进模板",
        );
    }

    let unfilled: Vec<String> = table
        .fields
        .iter()
        .filter(|f| !pairs.iter().any(|(_, to)| to == &f.name))
        .map(|f| f.name.clone())
        .collect();
    Ok(Mapping {
        pairs,
        skipped,
        unfilled,
    })
}

fn map_rows(rows: &[Row], mapping: &Mapping) -> Vec<Row> {
    rows.iter()
        .map(|row| {
            let mut out = Row::new();
            for (from, to) in &mapping.pairs {
                let v = row.get(from).cloned().unwrap_or_default();
                out.insert(to.clone(), v.trim().to_string());
            }
            out
        })
        .filter(|r| r.values().any(|v| !v.trim().is_empty()))
        .collect()
}

fn str_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|i| {
                    i.as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| i.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 模板声明的类型 vs 内核推断出来的类型：对不上就列出来（**只提示，不改模板**）。
///
/// 整数是数值的子集（声明 number、数据全是整数）不算冲突。
/// 自动生成主键时的起点：`PREFIX-0007` 已存在就从 8 开始（不撞已有编号）。
fn next_id_start(kernel: &Kernel, project: &Path, table: &str, key: &str, prefix: &str) -> usize {
    let existing = kernel.rows(project, table).unwrap_or_default();
    let mut max = 0usize;
    for r in existing {
        let v = r.get(key).map(|s| s.trim()).unwrap_or("");
        let Some(rest) = v.strip_prefix(&format!("{prefix}-")) else {
            continue;
        };
        if let Ok(n) = rest.parse::<usize>() {
            max = max.max(n);
        }
    }
    max + 1
}

fn type_mismatches(table: &TableDef, suggestions: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for s in suggestions {
        let column = str_of(s, "column");
        let Some(f) = table.fields.iter().find(|f| f.name == column) else {
            continue; // 模板外的列由 --allow-new-columns 决定，不在类型对账范围
        };
        let Some(recommended) = s
            .get("profile")
            .and_then(|p| p.get("recommended"))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        if recommended == f.field_type {
            continue;
        }
        if f.field_type == "number" && recommended == "integer" {
            continue;
        }
        out.push(json!({
            "column": column,
            "declared": f.field_type,
            "suggested": recommended,
            "note": "模板声明的类型与数据看起来的类型不一致：数据仍会按模板类型校验",
        }));
    }
    out
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// 项目里有哪些表（给 `tables` 命令用）。
#[allow(dead_code)]
fn project_tables(template: &Value) -> Vec<String> {
    parse_tables(template).iter().map(|t| t.name.clone()).collect()
}

/// 表定义里的列名（给导出用）。
#[allow(dead_code)]
pub fn field_names(template: &Value, table: &str) -> Vec<String> {
    parse_tables(template)
        .iter()
        .find(|t| t.name == table)
        .map(|t| t.fields.iter().map(|f| f.name.clone()).collect())
        .unwrap_or_default()
}

/// 单元测试用：模板解析与列匹配。
#[allow(dead_code)]
pub fn parse_tables_for_test(template: &Value) -> usize {
    parse_tables(template).len()
}

/// 导出目录默认位置（`<项目>/exports/<时间戳>-<类型>`）。
pub fn default_out_dir(project: &Path, kind: &str) -> std::path::PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    project.join("exports").join(format!("{stamp}-{kind}"))
}
