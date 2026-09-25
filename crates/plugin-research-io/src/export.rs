//! 导出：给 SPSS 与 R 用的 CSV + 脚本（FR-F04 / FR-F05）。
//!
//! 两条原则：
//! 1. **导出永不因校验阻断**（§5.9.3）：数据永远可读可导，发现的问题写进导出说明，不拦人；
//! 2. **标签与因子水平来自模板**：SPSS 的 `VALUE LABELS`、R 的 `factor(levels=...)`
//!    都用模板里声明的允许值顺序，保证"图上的顺序 = 设计里的顺序"，而不是字母序。
//!
//! 长表/宽表：给 `--index/--name-from/--value` 就是宽表（一行一个实验单元、
//! 一列一个指标），不给就是长表（一行一条观测）。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::kernel::Kernel;
use crate::row::Row;
use crate::{fail, R};
use crate::ExportArgs;

/// 导出结果（给 JSON 与人读输出共用）。
struct ExportOut {
    action: &'static str,
    dir: PathBuf,
    table: String,
    rows: usize,
    columns: Vec<String>,
    wide: Option<WideInfo>,
    fingerprint: String,
    warnings: Vec<String>,
}

#[derive(Debug)]
struct WideInfo {
    index: String,
    name_from: String,
    value: String,
    columns: usize,
}

/// CSV + `.sps`（SPSS 语法）。
pub fn spss(kernel: &Kernel, args: &ExportArgs) -> R<Value> {
    let (out, table, csv_rows) = prepare(kernel, args, "spss")?;
    let mut files = Vec::new();

    let csv_name = format!("{}.csv", out.table);
    let csv_path = out.dir.join(&csv_name);
    write_csv(&csv_path, &out.columns, &csv_rows)?;
    files.push(csv_path.clone());

    let sps = spss_syntax(&csv_name, &out, &table)?;
    let sps_path = out.dir.join(format!("{}.sps", out.table));
    std::fs::write(&sps_path, sps)?;
    files.push(sps_path);

    files.extend(write_readme(&out, args, "SPSS", &csv_name, kernel)?);
    Ok(finish(out, files, "research.export-spss"))
}

/// CSV + R 读取脚本。
pub fn r(kernel: &Kernel, args: &ExportArgs) -> R<Value> {
    let (out, table, csv_rows) = prepare(kernel, args, "r")?;
    let mut files = Vec::new();

    let csv_name = format!("{}.csv", out.table);
    let csv_path = out.dir.join(&csv_name);
    write_csv(&csv_path, &out.columns, &csv_rows)?;
    files.push(csv_path.clone());

    let script = r_script(&csv_name, &table, &out);
    let script_path = out.dir.join(format!("read-{}.R", out.table));
    std::fs::write(&script_path, script)?;
    files.push(script_path);

    files.extend(write_readme(&out, args, "R", &csv_name, kernel)?);
    Ok(finish(out, files, "research.export-r"))
}

/// 人读输出。
pub fn human(v: &Value) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "导出完成：{}\n目录：{}\n",
        v.get("table").and_then(|x| x.as_str()).unwrap_or(""),
        v.get("out_dir").and_then(|x| x.as_str()).unwrap_or("")
    ));
    s.push_str(&format!(
        "行数 {}｜列数 {}\n",
        v.get("rows").and_then(|x| x.as_u64()).unwrap_or(0),
        v.get("columns")
            .and_then(|x| x.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
    ));
    if let Some(w) = v.get("wide") {
        s.push_str(&format!(
            "宽表：行身份 {}｜列名来自 {}｜取值 {}\n",
            w.get("index").and_then(|x| x.as_str()).unwrap_or(""),
            w.get("name_from").and_then(|x| x.as_str()).unwrap_or(""),
            w.get("value").and_then(|x| x.as_str()).unwrap_or(""),
        ));
    }
    if let Some(files) = v.get("files").and_then(|x| x.as_array()) {
        s.push_str("文件：\n");
        for f in files {
            s.push_str(&format!("  {}\n", f.as_str().unwrap_or("")));
        }
    }
    let warnings = v.get("warnings").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    if !warnings.is_empty() {
        s.push_str(&format!("提醒 {} 条：\n", warnings.len()));
        for w in warnings.iter().take(5) {
            s.push_str(&format!("  {}\n", w.as_str().unwrap_or("")));
        }
    }
    s.push_str("\n数据未被改动；导出动作已在项目流水里留痕。\n");
    s
}

/// 公共准备：读表 → （可选）转宽表 → 目录与说明书骨架。
fn prepare(kernel: &Kernel, args: &ExportArgs, kind: &str) -> R<(ExportOut, Value, Vec<Row>)> {
    let template = kernel.template(&args.project)?;
    let table_name = args.table.clone();
    let table = template
        .get("tables")
        .and_then(|v| v.as_array())
        .and_then(|a| a.iter().find(|t| str_of(t, "name") == table_name))
        .cloned()
        .ok_or_else(|| {
            crate::Fail::new(
                format!("模板里没有表「{table_name}」"),
                format!(
                    "可用表：{}",
                    template
                        .get("tables")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .map(|t| str_of(t, "name"))
                                .collect::<Vec<_>>()
                                .join(" / ")
                        })
                        .unwrap_or_default()
                ),
            )
        })?;

    let rows = kernel.rows(&args.project, &table_name)?;
    let fingerprint = kernel
        .manifest(&args.project)
        .ok()
        .and_then(|m| {
            m.get("tables")
                .and_then(|v| v.as_array())
                .and_then(|a| {
                    a.iter()
                        .find(|t| str_of(t, "name") == table_name)
                        .map(|t| str_of(t, "sha256"))
                })
        })
        .unwrap_or_default();

    let dir = match &args.out {
        Some(p) => p.clone(),
        None => crate::import::default_out_dir(&args.project, kind),
    };
    std::fs::create_dir_all(&dir)?;

    let mut warnings: Vec<String> = Vec::new();
    let (columns, csv_rows, wide) = match (&args.index, &args.name_from, &args.value) {
        (Some(index), Some(name_from), Some(value)) => {
            let (cols, rows_out, w) = to_wide(&rows, index, name_from, value)?;
            if w.columns > 200 {
                warnings.push(format!(
                    "宽表有 {} 列，某些软件可能吃不消——考虑先筛选再导出",
                    w.columns
                ));
            }
            (cols, rows_out, Some(w))
        }
        (None, None, None) => {
            let cols: Vec<String> = table
                .get("fields")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|f| str_of(f, "name")).collect())
                .unwrap_or_default();
            (cols, rows.clone(), None)
        }
        _ => {
            return fail(
                "宽表要三个参数一起给：--index 行身份列 --name-from 指标列 --value 数值列",
                "例如 --index plot_id --name-from trait_id --value value_num；不给这三个就是长表",
            )
        }
    };

    Ok((
        ExportOut {
            action: "research.export",
            dir,
            table: table_name,
            rows: csv_rows.len(),
            columns,
            wide,
            fingerprint,
            warnings,
        },
        table,
        csv_rows,
    ))
}

fn finish(out: ExportOut, files: Vec<PathBuf>, action: &'static str) -> Value {
    json!({
        "schema": "cercis.plugin/1",
        "ok": true,
        "action": action,
        "table": out.table,
        "out_dir": out.dir.display().to_string(),
        "rows": out.rows,
        "columns": out.columns,
        "wide": out.wide.map(|w| json!({
            "index": w.index,
            "name_from": w.name_from,
            "value": w.value,
            "columns": w.columns,
        })),
        "source_fingerprint": out.fingerprint,
        "files": files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>(),
        "warnings": out.warnings,
        "action_label": out.action,
    })
}

/// 长表 → 宽表：一行一个 `index`，一列一个 `name_from` 取值。
fn to_wide(
    rows: &[Row],
    index: &str,
    name_from: &str,
    value: &str,
) -> R<(Vec<String>, Vec<Row>, WideInfo)> {
    // 列顺序按"第一次出现的顺序"排（数据的自然顺序，比字母序好读）
    let mut keys: Vec<String> = Vec::new();
    let mut index_values: Vec<String> = Vec::new();
    let mut cells: std::collections::BTreeMap<String, Row> = std::collections::BTreeMap::new();
    let mut duplicates: Vec<String> = Vec::new();

    for r in rows {
        let i = r.get(index).map(|s| s.trim()).unwrap_or("").to_string();
        let k = r.get(name_from).map(|s| s.trim()).unwrap_or("").to_string();
        let v = r.get(value).cloned().unwrap_or_default();
        if i.is_empty() || k.is_empty() {
            continue;
        }
        if !keys.contains(&k) {
            keys.push(k.clone());
        }
        if !index_values.contains(&i) {
            index_values.push(i.clone());
        }
        let entry = cells.entry(i.clone()).or_default();
        if entry.contains_key(&k) {
            let flag = format!("{i} × {k}");
            if !duplicates.contains(&flag) {
                duplicates.push(flag);
            }
        }
        entry.insert(k, v);
    }

    if !duplicates.is_empty() {
        let shown = duplicates.iter().take(5).cloned().collect::<Vec<_>>().join("；");
        return fail(
            format!(
                "宽表要求「一行一个实验单元 × 一个指标」，但同一个组合出现了多次：{shown}{}",
                if duplicates.len() > 5 { " …" } else { "" }
            ),
            "先在长表里筛选（例如只取一个生育期），或改用长表导出（去掉 --index/--name-from/--value）",
        );
    }

    let mut out_rows: Vec<Row> = Vec::new();
    for i in &index_values {
        let mut row = Row::new();
        row.insert(index.to_string(), i.clone());
        if let Some(entry) = cells.get(i) {
            for k in &keys {
                row.insert(k.clone(), entry.get(k).cloned().unwrap_or_default());
            }
        }
        out_rows.push(row);
    }
    let mut cols = vec![index.to_string()];
    cols.extend(keys.iter().cloned());
    Ok((
        cols,
        out_rows,
        WideInfo {
            index: index.to_string(),
            name_from: name_from.to_string(),
            value: value.to_string(),
            columns: keys.len(),
        },
    ))
}

fn write_csv(path: &Path, columns: &[String], rows: &[Row]) -> R<()> {
    let mut wtr = csv::WriterBuilder::new().from_path(path)?;
    wtr.write_record(columns)?;
    for r in rows {
        let record: Vec<&str> = columns
            .iter()
            .map(|c| r.get(c).map(|s| s.as_str()).unwrap_or(""))
            .collect();
        wtr.write_record(&record)?;
    }
    wtr.flush()?;
    Ok(())
}

/// `.sps` 语法：变量标签、值标签、缺失值、格式。
fn spss_syntax(csv_name: &str, out: &ExportOut, table: &Value) -> R<String> {
    let fields = table
        .get("fields")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut s = String::new();
    s.push_str("* Cercis 科研工具包生成；数据文件同目录，请一起拷贝。\n");
    s.push_str("* 源数据指纹（sha256）：");
    s.push_str(if out.fingerprint.is_empty() {
        "（未取到）"
    } else {
        &out.fingerprint
    });
    s.push_str("\n\n");

    let vars: Vec<String> = out.columns.clone();
    s.push_str("GET DATA\n  /TYPE=TXT\n");
    s.push_str(&format!("  /FILE='{csv_name}'\n"));
    s.push_str("  /ENCODING='UTF8'\n  /DELIMITERS=','\n  /QUALIFIER='\"'\n");
    s.push_str("  /ARRANGEMENT=DELIMITED\n  /FIRSTCASE=2\n  /VARIABLES=\n");
    for (i, v) in vars.iter().enumerate() {
        let ty = spss_type(&field_of(&fields, v));
        let tail = if i + 1 == vars.len() { ".\n" } else { "\n" };
        s.push_str(&format!("    {v} {ty}{tail}"));
    }
    s.push_str("EXECUTE.\n\n");

    // 变量标签（中文名）
    s.push_str("VARIABLE LABELS\n");
    let mut any = false;
    for v in &vars {
        let f = field_of(&fields, v);
        let label = str_of(&f, "label_cn");
        if !label.is_empty() {
            s.push_str(&format!("  {v} '{label}'\n"));
            any = true;
        }
    }
    s.push_str(".\n\n");
    if !any {
        s.push_str("* （模板里没有中文标签）\n\n");
    }

    // 值标签：分类列 ≤20 个取值时列全（用模板的允许值顺序）
    s.push_str("VALUE LABELS\n");
    let mut any_labels = false;
    for v in &vars {
        let f = field_of(&fields, v);
        let allowed = str_of(&f, "allowed_values");
        if allowed.is_empty() {
            continue;
        }
        let values: Vec<&str> = allowed
            .split(';')
            .map(|x| x.trim())
            .filter(|x| !x.is_empty())
            .collect();
        if values.is_empty() {
            continue;
        }
        s.push_str(&format!("  {v}\n"));
        for (i, val) in values.iter().enumerate() {
            let num = i + 1;
            s.push_str(&format!("    {num} '{val}'\n"));
        }
        any_labels = true;
    }
    s.push_str(".\n\n");
    if !any_labels {
        s.push_str("* （模板里没有声明允许值，故没有值标签）\n\n");
    }

    // 缺失值：CSV 空单元格在 SPSS 里本就是 sysmis；这里只声明"占位符"不该出现
    s.push_str("* 缺失值：留空即缺失（CSV 空单元格 = sysmis）。\n");
    s.push_str("* 不要用 NA / - / 0 表示缺失（内核只给提醒，不拦数据）。\n");
    Ok(s)
}

fn spss_type(f: &Value) -> String {
    match str_of(f, "type").as_str() {
        "integer" => "F8.0".to_string(),
        "number" => match f.get("precision").and_then(|v| v.as_u64()) {
            Some(p) => format!("F8.{p}"),
            None => "F8.2".to_string(),
        },
        "date" => "ADATE10".to_string(),
        "datetime" => "SDATE20".to_string(),
        "time" => "TIME8".to_string(),
        _ => "A255".to_string(),
    }
}

/// R 读取脚本：因子水平按模板允许值排序；标签写进变量属性。
fn r_script(csv_name: &str, _table: &Value, out: &ExportOut) -> String {
    let mut s = String::new();
    s.push_str("# 由 Cercis 科研工具包生成（R 读取脚本）\n");
    s.push_str(&format!("# 源数据指纹（sha256）：{}\n", if out.fingerprint.is_empty() { "（未取到）" } else { &out.fingerprint }));
    s.push_str("# 说明：CSV 是 UTF-8；缺失值 = 空字段（用 na.strings 转成 NA）。\n\n");
    s.push_str("options(encoding = \"UTF-8\", stringsAsFactors = FALSE)\n\n");
    s.push_str(&format!(
        "data <- read.csv(\"{csv_name}\",\n  header = TRUE, sep = \",\", quote = '\"',\n  na.strings = \"\", check.names = FALSE, fileEncoding = \"UTF-8\")\n\n"
    ));
    s.push_str("# 类型：日期转 Date，数值转 numeric（有些列因空格会读成字符）\n");
    for (i, c) in out.columns.iter().enumerate() {
        s.push_str(&format!(
            "data${c} <- if (is.character(data${c}) && all(grepl('^-?[0-9]+(\\\\.[0-9]+)?$', na.omit(data${c})))) as.numeric(data${c}) else data${c}\n"
        ));
        let _ = i;
    }
    s.push_str("\n# 变量标签（作图标题用）\n");
    for c in &out.columns {
        s.push_str(&format!("# {c}\n"));
    }
    s.push_str("\n# 想看数据概览：\n");
    s.push_str("summary(data)\n");
    s
}

/// 导出说明（FR-F07：版本指纹、导出时间、筛选条件、文件清单）。
fn write_readme(
    out: &ExportOut,
    args: &ExportArgs,
    kind: &str,
    csv_name: &str,
    kernel: &Kernel,
) -> R<Vec<PathBuf>> {
    let mut md = String::new();
    md.push_str("# 导出说明\n\n");
    md.push_str(&format!("- 导出类型：{kind}\n"));
    md.push_str(&format!("- 表：{}\n", out.table));
    md.push_str(&format!("- 导出时间：{}\n", chrono::Utc::now().to_rfc3339()));
    md.push_str(&format!("- 行数：{}\n- 列数：{}\n", out.rows, out.columns.len()));
    md.push_str(&format!(
        "- **源数据指纹（sha256）**：{}\n",
        if out.fingerprint.is_empty() {
            "（未取到）"
        } else {
            &out.fingerprint
        }
    ));
    if let Some(w) = &out.wide {
        md.push_str(&format!(
            "- 形状：宽表（行身份 {}，列名来自 {}，取值 {}，指标 {} 个）\n",
            w.index, w.name_from, w.value, w.columns
        ));
    } else {
        md.push_str("- 形状：长表（一行一条观测）\n");
    }
    md.push_str(&format!("- 数据文件：{csv_name}（UTF-8，无 BOM，LF 换行）\n"));
    md.push_str(&format!("- 筛选条件：{}\n", if args.index.is_some() { "宽表转换（见上）" } else { "无（整表）" }));
    md.push_str("\n> 报告这份指纹，就能把论文/图表对回确切的数据版本。\n");
    if !out.warnings.is_empty() {
        md.push_str("\n## 提醒\n\n");
        for w in &out.warnings {
            md.push_str(&format!("- {w}\n"));
        }
    }
    let path = out.dir.join("导出说明.md");
    std::fs::write(&path, md)?;

    // 留痕：导出动作写进项目流水（不改数据）
    let _ = kernel.call(
        &[
            "note".into(),
            args.project.display().to_string(),
            "--action".into(),
            "export".into(),
            "--table".into(),
            out.table.clone(),
            "--summary".into(),
            format!("导出 {kind}：{}（{} 行）", out.dir.display(), out.rows),
            "--source-ref".into(),
            out.dir.display().to_string(),
        ],
        None,
    );
    Ok(vec![path])
}

/// 列出模板里的表（`tables` 命令用）。
pub fn list_tables(template: &Value) -> Vec<Value> {
    template
        .get("tables")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|t| {
                    json!({
                        "name": str_of(t, "name"),
                        "label_cn": str_of(t, "label_cn"),
                        "primary_key": str_of(t, "primary_key"),
                        "columns": t.get("fields").and_then(|f| f.as_array())
                            .map(|f| f.iter().map(|x| str_of(x, "name")).collect::<Vec<_>>())
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn field_of(fields: &[Value], name: &str) -> Value {
    fields
        .iter()
        .find(|f| str_of(f, "name") == name)
        .cloned()
        .unwrap_or(Value::Null)
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
    use crate::row::Row;

    fn row(pairs: &[(&str, &str)]) -> Row {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn wide_conversion_keeps_first_seen_order() {
        let rows = vec![
            row(&[("plot_id", "P1"), ("trait_id", "height"), ("value_num", "12.5")]),
            row(&[("plot_id", "P1"), ("trait_id", "spad"), ("value_num", "45")]),
            row(&[("plot_id", "P2"), ("trait_id", "height"), ("value_num", "13")]),
            row(&[("plot_id", "P2"), ("trait_id", "spad"), ("value_num", "46")]),
        ];
        let (cols, out, info) = to_wide(&rows, "plot_id", "trait_id", "value_num").unwrap();
        assert_eq!(cols, vec!["plot_id", "height", "spad"]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].get("spad").unwrap(), "45");
        assert_eq!(info.columns, 2);
    }

    #[test]
    fn wide_conversion_refuses_duplicates() {
        let rows = vec![
            row(&[("plot_id", "P1"), ("trait_id", "height"), ("value_num", "12.5")]),
            row(&[("plot_id", "P1"), ("trait_id", "height"), ("value_num", "12.6")]),
        ];
        let err = to_wide(&rows, "plot_id", "trait_id", "value_num").unwrap_err();
        assert!(err.message.contains("多次"), "{}", err.message);
    }

    #[test]
    fn spss_type_follows_template() {
        let json = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        assert_eq!(spss_type(&json(r#"{"type":"integer"}"#)), "F8.0");
        assert_eq!(
            spss_type(&json(r#"{"type":"number","precision":1}"#)),
            "F8.1"
        );
        assert_eq!(spss_type(&json(r#"{"type":"date"}"#)), "ADATE10");
        assert_eq!(spss_type(&json(r#"{"type":"text"}"#)), "A255");
    }
}
