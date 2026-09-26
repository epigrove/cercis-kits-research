//! 科研导入导出插件（`cercis-plugin-research-io`）。
//!
//! 四件事（都属科研工具包，对应 FR-E01 / FR-F04 / FR-F05 / FR-F06）：
//! 1. `import-xlsx` Excel/CSV 记录表导入：列映射 → 类型建议 → 试运行 → 入库
//! 2. `codebook`    数据说明书：字段、类型、单位、取值标签、缺失情况、指纹
//! 3. `export-spss` SPSS 用的 CSV + `.sps` 语法（变量标签、值标签、格式）
//! 4. `export-r`    R 读取脚本 + CSV（含因子水平顺序与中文标签）
//!
//! 边界（架构硬规则）：
//! - 读写数据一律走内核通道（`cercis … --json`），插件**不碰** `tables/*.csv`；
//! - 类型与校验规则来自内核（`cercis value-types --json`、`cercis infer-types`），
//!   插件不自己写一套判断（§5.9 / D36）。

mod codebook;
mod export;
mod import;
mod kernel;
mod row;

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

pub fn fail<T>(message: impl Into<String>, hint: impl Into<String>) -> R<T> {
    Err(Fail {
        message: message.into(),
        hint: hint.into(),
    })
}

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

impl From<csv::Error> for Fail {
    fn from(e: csv::Error) -> Self {
        Fail::new(format!("表格读写失败：{e}"), "确认文件没被占用、格式没问题")
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "cercis-plugin-research-io",
    version,
    about = "Cercis 科研工具包 · 科研导入导出（Excel 导入 / 说明书 / SPSS / R）"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// 内核可执行文件（默认自动查找）
    #[arg(long, global = true)]
    cercis: Option<PathBuf>,
    /// 机器可读输出
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 导入 Excel / CSV 记录表（列映射 → 类型建议 → 试运行 → 入库）
    ImportXlsx(ImportArgs),
    /// 生成数据说明书（codebook）
    Codebook(CodebookArgs),
    /// 导出 SPSS 用的 CSV + .sps 语法
    ExportSpss(ExportArgs),
    /// 导出 R 读取脚本 + CSV
    ExportR(ExportArgs),
    /// 列出项目里的表与列（看看有什么可导）
    Tables(TablesArgs),
}

#[derive(Args, Debug)]
struct ImportArgs {
    /// 项目目录
    project: PathBuf,
    /// 要导入的文件（.xlsx/.xls/.ods/.csv/.tsv）
    #[arg(long)]
    file: PathBuf,
    /// 目标表（默认按列名自动匹配）
    #[arg(long)]
    table: Option<String>,
    /// Excel 工作表名（默认第一个）
    #[arg(long)]
    sheet: Option<String>,
    /// 列映射：源列=目标列（可重复；默认按列名与中英文标签自动匹配）
    #[arg(long = "map", value_name = "SOURCE=TARGET")]
    maps: Vec<String>,
    /// 忽略源文件里模板没有的列（默认报错）
    #[arg(long)]
    skip_unknown: bool,
    /// 允许写入模板未定义的列（新列的类型建议见输出里的"建议类型"）
    #[arg(long)]
    allow_new_columns: bool,
    /// 记录表里没写编号时，用它自动生成主键（如 `--generate-id OB` → OB-0001、OB-0002…）
    #[arg(long, value_name = "PREFIX")]
    generate_id: Option<String>,
    /// 只试运行、不写数据
    #[arg(long)]
    dry_run: bool,
    /// 生成导入报告（Markdown）
    #[arg(long)]
    report: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct CodebookArgs {
    /// 项目目录
    project: PathBuf,
    /// 输出文件（默认打印到屏幕）
    #[arg(long)]
    out: Option<PathBuf>,
    /// 只看某张表
    #[arg(long)]
    table: Option<String>,
}

#[derive(Args, Debug)]
struct ExportArgs {
    /// 项目目录
    project: PathBuf,
    /// 要导出的表
    #[arg(long)]
    table: String,
    /// 输出目录（默认 <项目>/exports/<时间戳>-<类型>）
    #[arg(long)]
    out: Option<PathBuf>,
    /// 宽表：用这一列作为行身份（如 plot_id）
    #[arg(long)]
    index: Option<String>,
    /// 宽表：用这一列的值当列名（如 trait_id）
    #[arg(long = "name-from")]
    name_from: Option<String>,
    /// 宽表：取值列（如 value_num）
    #[arg(long = "value")]
    value: Option<String>,
    /// 同时把数据说明书放进导出目录
    #[arg(long)]
    codebook: bool,
    /// 分类列按 1/2/3 编码落盘（SPSS 更常用；默认保留原文，标签仍会写进 .sps）
    #[arg(long)]
    encode_values: bool,
}

#[derive(Args, Debug)]
struct TablesArgs {
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
        Command::ImportXlsx(a) => {
            let out = import::import(&kernel, a)?;
            emit(cli.json, &out, || import::human(&out))
        }
        Command::Codebook(a) => {
            let (text, out) = codebook::build(&kernel, &a.project, a.table.as_deref())?;
            if let Some(path) = &a.out {
                std::fs::write(path, &text)?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "schema": "cercis.plugin/1",
                            "ok": true,
                            "action": "research.codebook",
                            "out": path.display().to_string(),
                            "tables": out,
                        }))
                        .unwrap_or_default()
                    );
                } else {
                    println!("已写出数据说明书：{}", path.display());
                }
            } else if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema": "cercis.plugin/1",
                        "ok": true,
                        "action": "research.codebook",
                        "markdown": text,
                        "tables": out,
                    }))
                    .unwrap_or_default()
                );
            } else {
                print!("{text}");
            }
            Ok(())
        }
        Command::ExportSpss(a) => {
            let out = export::spss(&kernel, a)?;
            emit(cli.json, &out, || export::human(&out))
        }
        Command::ExportR(a) => {
            let out = export::r(&kernel, a)?;
            emit(cli.json, &out, || export::human(&out))
        }
        Command::Tables(a) => {
            let template = kernel.template(&a.project)?;
            let tables = export::list_tables(&template);
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema": "cercis.plugin/1",
                        "ok": true,
                        "action": "research.tables",
                        "tables": tables,
                    }))
                    .unwrap_or_default()
                );
            } else {
                for t in &tables {
                    println!(
                        "{}（{}）｜{} 列｜主键 {}",
                        t.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        t.get("label_cn").and_then(|v| v.as_str()).unwrap_or(""),
                        t.get("columns").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                        t.get("primary_key").and_then(|v| v.as_str()).unwrap_or("（无）")
                    );
                }
            }
            Ok(())
        }
    }
}

/// 输出：JSON 或人读文本二选一。
fn emit(json_out: bool, value: &Value, human: impl FnOnce() -> String) -> R<()> {
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into())
        );
    } else {
        print!("{}", human());
    }
    Ok(())
}
