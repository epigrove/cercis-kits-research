//! 打印与标签插件（`cercis-plugin-research-print`）：FR-G01 / FR-G02。
//!
//! 两件事：
//! 1. `sheet`  从模板生成**可打印记录表**：表头（项目 / 批次信息）+ 手写空行；
//!    浏览器里按打印即得 PDF（本机装了 Chrome/Edge 时也可以直接 `--pdf` 生成）
//! 2. `labels` 生成**二维码标签**（小区牌 / 样品袋 / 标签贴纸）：一行一张，文字 + 二维码
//!
//! 为什么放在科研工具包：打印哪种表、标签上放什么，是学科与课题的事；内核只管数据。
//! 数据一律经内核通道读（`template` / `query`），插件不直接碰 `tables/*.csv`。
//!
//! 「打印 → 手写 → 拍照 → 回录」这条闭环里，本插件负责**第一步**：
//! 生成的那张表就是田间拿在手上的纸；回录用 `plugin-research-scribe`（拍照/念读 → 草稿 → 入库）。

mod html;
mod kernel;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};

use kernel::Kernel;

/// 插件错误：一句说清发生了什么，再加一句怎么办。
#[derive(Debug)]
pub struct Fail {
    pub message: String,
    pub hint: String,
}

type R<T> = std::result::Result<T, Fail>;

impl Fail {
    pub fn new(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            hint: hint.into(),
        }
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        Fail::new(format!("文件操作失败：{e}"), "检查路径与读写权限")
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "cercis-plugin-research-print",
    version,
    about = "Cercis 科研工具包 · 打印记录表与二维码标签"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
    /// 内核可执行文件（默认自动查找）
    #[arg(long, global = true)]
    cercis: Option<PathBuf>,
    /// 机器可读输出
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 生成可打印记录表（HTML；装了 Chrome/Edge 可直接出 PDF）
    Sheet(SheetArgs),
    /// 生成二维码标签（HTML；一行若干张，可直接打印成贴纸）
    Labels(LabelsArgs),
    /// 列出可打印的表与字段（看看能印什么）
    List(ListArgs),
}

#[derive(Args, Debug)]
struct SheetArgs {
    /// 项目目录
    project: PathBuf,
    /// 要打印的表（如 observations）
    #[arg(long)]
    table: String,
    /// 输出文件（默认 <项目>/exports/记录表-<表>-<日期>.html）
    #[arg(long)]
    out: Option<PathBuf>,
    /// 手写空行数（默认 20）
    #[arg(long, default_value_t = 20)]
    rows: usize,
    /// 只印这些列（逗号分隔；默认印模板里除主键外的全部列）
    #[arg(long, value_name = "A,B,C")]
    fields: Option<String>,
    /// 表头预填（可重复）：--set batch_id=BATCH-20260615
    #[arg(long = "set", value_name = "COL=VALUE")]
    sets: Vec<String>,
    /// 标题（默认用表的显示名）
    #[arg(long, default_value = "")]
    title: String,
    /// 顺便生成 PDF（要本机有 Chrome / Edge；没有会给出提示，HTML 仍可用）
    #[arg(long)]
    pdf: bool,
}

#[derive(Args, Debug)]
struct LabelsArgs {
    /// 项目目录
    project: PathBuf,
    /// 数据来自哪张表（如 units / samples）
    #[arg(long)]
    table: String,
    /// 用哪一列做标签与二维码内容（如 unit_id）
    #[arg(long)]
    field: String,
    /// 输出文件（默认 <项目>/exports/标签-<表>-<日期>.html）
    #[arg(long)]
    out: Option<PathBuf>,
    /// 每行印几张（默认 3）
    #[arg(long, default_value_t = 3)]
    per_row: usize,
    /// 标签上再加一列小字（如 sample_type、block）
    #[arg(long, default_value = "")]
    note_field: String,
    /// 只印这些（可重复）：--only U-001
    #[arg(long = "only", value_name = "VALUE")]
    only: Vec<String>,
}

#[derive(Args, Debug)]
struct ListArgs {
    /// 项目目录
    project: PathBuf,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "schema": "cercis.plugin/1",
                    "ok": false,
                    "error": e.message,
                    "hint": e.hint,
                }))
                .unwrap_or_default()
            );
        } else {
            eprintln!("错误：{}", e.message);
            if !e.hint.is_empty() {
                eprintln!("怎么办：{}", e.hint);
            }
        }
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> R<()> {
    let kernel = Kernel::locate(cli.cercis.as_deref())?;
    match &cli.command {
        Cmd::Sheet(a) => sheet_cmd(&kernel, cli.json, a),
        Cmd::Labels(a) => labels_cmd(&kernel, cli.json, a),
        Cmd::List(a) => list_cmd(&kernel, cli.json, a),
    }
}

/// 生成打印记录表。
fn sheet_cmd(kernel: &Kernel, json_out: bool, a: &SheetArgs) -> R<()> {
    let template = kernel.template(&a.project)?;
    let table = find_table(&template, &a.table)?;
    let label = str_of(&table, "label_cn");
    let fields = pick_fields(&table, a.fields.as_deref())?;
    if fields.is_empty() {
        return Err(Fail::new(
            format!("表 {} 没有可打印的列", a.table),
            "用 --fields 指定要印哪几列",
        ));
    }
    let head: Vec<(String, String)> = a
        .sets
        .iter()
        .map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .ok_or_else(|| Fail::new(format!("--set 要写成 列=值，实际：{kv}"), ""))
        })
        .collect::<R<Vec<_>>>()?;
    let field_names = doc_fields(&fields);
    let doc = html::record_sheet(
        &html::SheetInput {
            project_name: kernel.project_name(&a.project)?,
            // 标题别叠字："测定记录" + "记录表" 会变成"测定记录记录表"
            title: if a.title.is_empty() {
                if label.ends_with("记录") || label.ends_with('表') {
                    format!("{label}表")
                } else {
                    format!("{label}记录表")
                }
            } else {
                a.title.clone()
            },
            table_label: label.clone(),
            head,
            fields,
            rows: a.rows,
            printed_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        },
    );
    let out = a
        .out
        .clone()
        .unwrap_or_else(|| default_out(&a.project, "记录表", &a.table, "html"));
    write_out(&out, &doc)?;

    let mut pdf_path: Option<PathBuf> = None;
    let mut pdf_note: Option<String> = None;
    if a.pdf {
        match html::to_pdf(&out, &out.with_extension("pdf")) {
            Ok(Some(p)) => pdf_path = Some(p),
            Ok(None) => {
                pdf_note = Some("本机没找到 Chrome / Edge".into());
            }
            Err(reason) => pdf_note = Some(reason),
        }
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema": "cercis.plugin/1",
                "ok": true,
                "action": "research.print.sheet",
                "table": a.table,
                "fields": field_names,
                "rows": a.rows,
                "html": out.display().to_string(),
                "pdf": pdf_path.as_ref().map(|p| p.display().to_string()),
                "pdf_note": pdf_note,
            }))
            .unwrap_or_default()
        );
    } else {
        println!("已生成记录表：{}（{} 行手写位）", out.display(), a.rows);
        println!("打印：用浏览器打开它，按打印（或 Cmd+P）——A4 已排好版。");
        if let Some(p) = pdf_path {
            println!("PDF：{}", p.display());
        } else if let Some(note) = pdf_note {
            println!("（没能生成 PDF：{note}；HTML 一样能打印——用浏览器打开后按 Cmd+P）");
        }
    }
    Ok(())
}

/// 生成二维码标签。
fn labels_cmd(kernel: &Kernel, json_out: bool, a: &LabelsArgs) -> R<()> {
    let template = kernel.template(&a.project)?;
    let table = find_table(&template, &a.table)?;
    let project_id = kernel.project_id(&a.project)?;
    let rows = kernel.rows(&a.project, &a.table)?;
    let mut items: Vec<(String, String)> = Vec::new();
    for r in &rows {
        let id = r.get(&a.field).map(|s| s.trim().to_string()).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        if !a.only.is_empty() && !a.only.iter().any(|o| o == &id) {
            continue;
        }
        let note = if a.note_field.is_empty() {
            String::new()
        } else {
            r.get(&a.note_field).cloned().unwrap_or_default()
        };
        items.push((id, note));
    }
    if items.is_empty() {
        return Err(Fail::new(
            format!("表 {} 里没有可打印的 {}（或都被 --only 过滤掉了）", a.table, a.field),
            "先导数据，或换 --field / --only",
        ));
    }
    let (doc, qr_count) = html::label_sheet(&html::LabelInput {
        project_name: kernel.project_name(&a.project)?,
        project_id,
        table: a.table.clone(),
        table_label: str_of(&table, "label_cn"),
        field: a.field.clone(),
        field_label: field_label(&table, &a.field),
        per_row: a.per_row.max(1),
        items,
    })?;
    let out = a
        .out
        .clone()
        .unwrap_or_else(|| default_out(&a.project, "标签", &a.table, "html"));
    write_out(&out, &doc)?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema": "cercis.plugin/1",
                "ok": true,
                "action": "research.print.labels",
                "table": a.table,
                "field": a.field,
                "count": qr_count,
                "per_row": a.per_row,
                "html": out.display().to_string(),
            }))
            .unwrap_or_default()
        );
    } else {
        println!(
            "已生成 {} 张标签：{}（每行 {} 张）",
            qr_count,
            out.display(),
            a.per_row
        );
        println!("二维码内容：cercis:<项目编号>:<表>:<编号>——扫码就能定位到这条记录。");
    }
    Ok(())
}

/// 列出可打印的表与字段。
fn list_cmd(kernel: &Kernel, json_out: bool, a: &ListArgs) -> R<()> {
    let template = kernel.template(&a.project)?;
    let tables = template
        .get("tables")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let out: Vec<Value> = tables
        .iter()
        .map(|t| {
            json!({
                "table": str_of(t, "name"),
                "label_cn": str_of(t, "label_cn"),
                "primary_key": str_of(t, "primary_key"),
                "fields": t.get("fields").and_then(|v| v.as_array()).map(|fs| {
                    fs.iter().map(|f| json!({
                        "name": str_of(f, "name"),
                        "label_cn": str_of(f, "label_cn"),
                        "type": str_of(f, "type"),
                        "unit": str_of(f, "unit"),
                    })).collect::<Vec<_>>()
                }).unwrap_or_default(),
            })
        })
        .collect();
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema": "cercis.plugin/1",
                "ok": true,
                "action": "research.print.list",
                "tables": out,
            }))
            .unwrap_or_default()
        );
    } else {
        for t in &out {
            println!(
                "{}（{}）｜主键 {}",
                t["table"].as_str().unwrap_or(""),
                t["label_cn"].as_str().unwrap_or(""),
                t["primary_key"].as_str().unwrap_or("（无）")
            );
            if let Some(fs) = t["fields"].as_array() {
                for f in fs {
                    let unit = f["unit"].as_str().unwrap_or("");
                    println!(
                        "   {:<20} {:<10} {}{}",
                        f["name"].as_str().unwrap_or(""),
                        f["type"].as_str().unwrap_or(""),
                        f["label_cn"].as_str().unwrap_or(""),
                        if unit.is_empty() {
                            String::new()
                        } else {
                            format!("（{unit}）")
                        }
                    );
                }
            }
        }
    }
    Ok(())
}

/// 模板里的表定义。
fn find_table<'a>(template: &'a Value, name: &str) -> R<Value> {
    template
        .get("tables")
        .and_then(|v| v.as_array())
        .and_then(|ts| {
            ts.iter()
                .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
        })
        .cloned()
        .ok_or_else(|| {
            Fail::new(
                format!("模板里没有表 {name}"),
                format!(
                    "可用表：{}",
                    template
                        .get("tables")
                        .and_then(|v| v.as_array())
                        .map(|ts| {
                            ts.iter()
                                .map(|t| str_of(t, "name"))
                                .collect::<Vec<_>>()
                                .join(" / ")
                        })
                        .unwrap_or_default()
                ),
            )
        })
}

/// 要印的列：显式给了就用给的，否则用模板里除主键外的全部列。
fn pick_fields(table: &Value, want: Option<&str>) -> R<Vec<html::PrintField>> {
    let all = table
        .get("fields")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let pk = str_of(table, "primary_key");
    let names: Vec<String> = match want {
        Some(list) => list
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        None => all
            .iter()
            .map(|f| str_of(f, "name"))
            .filter(|n| n != &pk)
            .collect(),
    };
    let mut out = Vec::new();
    for n in &names {
        let Some(f) = all.iter().find(|f| &str_of(f, "name") == n) else {
            return Err(Fail::new(
                format!("模板里没有列 {n}"),
                "用 `print list` 看这张表有哪些列",
            ));
        };
        out.push(html::PrintField {
            name: n.clone(),
            label: str_of(f, "label_cn"),
            unit: str_of(f, "unit"),
            allow: str_of(f, "allowed_values"),
            required: f.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
        });
    }
    Ok(out)
}

fn field_label(table: &Value, name: &str) -> String {
    table
        .get("fields")
        .and_then(|v| v.as_array())
        .and_then(|fs| fs.iter().find(|f| str_of(f, "name") == name))
        .map(|f| str_of(f, "label_cn"))
        .unwrap_or_else(|| name.to_string())
}

fn doc_fields(fields: &[html::PrintField]) -> Vec<String> {
    fields.iter().map(|f| f.name.clone()).collect()
}

fn default_out(project: &std::path::Path, kind: &str, table: &str, ext: &str) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    project
        .join("exports")
        .join(format!("{kind}-{table}-{stamp}.{ext}"))
}

fn write_out(path: &std::path::Path, text: &str) -> R<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}
