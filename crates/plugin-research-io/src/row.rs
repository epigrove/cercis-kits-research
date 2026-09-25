//! 行与输入读取：CSV/TSV 与 **Excel（.xlsx）**。
//!
//! Excel 读取用 `calamine`：真实用户的记录表多半是 Excel，
//! 表格里常见"首行标题 + 中间空行 + 合并单元格"，这里的策略是：
//! 取**第一个非空行**当表头，容忍空行与尾部空列，单元格统一转成文本。

use std::path::Path;

use calamine::{open_workbook_auto, Data, Reader};

use crate::{Fail, R};

pub type Row = std::collections::BTreeMap<String, String>;

/// 读进来的一张表（保留原始列顺序）。
pub struct InputTable {
    pub headers: Vec<String>,
    pub rows: Vec<Row>,
    pub source: String,
    pub sheet: String,
}

/// 读 CSV/TSV（首行表头）。
pub fn read_csv(path: &Path) -> R<InputTable> {
    let delimiter = if path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("tsv"))
        .unwrap_or(false)
    {
        b'\t'
    } else {
        b','
    };
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .delimiter(delimiter)
        .from_path(path)
        .map_err(|e| Fail::new(format!("读不到文件：{e}"), ""))?;

    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| Fail::new(format!("表头解析失败：{e}"), ""))?
        .iter()
        .map(|s| s.trim_start_matches('\u{feff}').trim().to_string())
        .collect();
    if headers.is_empty() || headers.iter().any(|h| h.is_empty()) {
        return Err(Fail::new(
            format!("{} 的表头为空或存在空列名", path.display()),
            "首行必须是列名",
        ));
    }

    let mut rows = Vec::new();
    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|e| Fail::new(format!("第 {} 行解析失败：{e}", i + 2), ""))?;
        let mut row = Row::new();
        for (j, col) in headers.iter().enumerate() {
            row.insert(col.clone(), rec.get(j).unwrap_or("").to_string());
        }
        rows.push(row);
    }
    Ok(InputTable {
        headers,
        rows,
        source: path.display().to_string(),
        sheet: String::new(),
    })
}

/// 读 Excel（.xlsx / .xls / .ods）：取第一个工作表，或指定的工作表。
pub fn read_xlsx(path: &Path, sheet: Option<&str>) -> R<InputTable> {
    let mut workbook = open_workbook_auto(path).map_err(|e| {
        Fail::new(
            format!("打不开 Excel 文件 {}：{e}", path.display()),
            "确认文件没坏、且不是加密文档",
        )
    })?;

    let names = workbook.sheet_names().to_vec();
    let chosen = match sheet {
        Some(want) => names
            .iter()
            .find(|n| n.as_str() == want)
            .cloned()
            .ok_or_else(|| {
                Fail::new(
                    format!("Excel 里没有工作表「{want}」"),
                    format!("可用工作表：{}", names.join(" / ")),
                )
            })?,
        None => names
            .first()
            .cloned()
            .ok_or_else(|| Fail::new(format!("{} 里没有工作表", path.display()), ""))?,
    };

    let range = workbook
        .worksheet_range(&chosen)
        .map_err(|e| Fail::new(format!("读工作表「{chosen}」失败：{e}"), ""))?;

    // 找第一个非空行当表头（Excel 里常有标题行、空行）
    let mut raw: Vec<Vec<String>> = Vec::new();
    for r in range.rows() {
        let cells: Vec<String> = r.iter().map(cell_text).collect();
        if cells.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        raw.push(cells);
    }
    if raw.is_empty() {
        return Err(Fail::new(
            format!("工作表「{chosen}」是空的"),
            "至少要有表头行与一行数据",
        ));
    }

    let header_row = raw.remove(0);
    let width = header_row.len();
    let headers: Vec<String> = header_row
        .iter()
        .map(|h| h.trim().to_string())
        .collect();
    if headers.iter().all(|h| h.is_empty()) {
        return Err(Fail::new(
            format!("工作表「{chosen}」的表头是空的"),
            "第一行（非空行）应当是列名",
        ));
    }

    let mut rows = Vec::new();
    for cells in raw {
        let mut row = Row::new();
        let mut all_empty = true;
        for (j, col) in headers.iter().enumerate() {
            if col.is_empty() {
                continue; // 跳过没有列名的空列
            }
            let text = cells.get(j).cloned().unwrap_or_default();
            if !text.trim().is_empty() {
                all_empty = false;
            }
            row.insert(col.clone(), text);
        }
        if !all_empty {
            rows.push(row);
        }
    }
    let _ = width;

    Ok(InputTable {
        headers: headers.into_iter().filter(|h| !h.is_empty()).collect(),
        rows,
        source: path.display().to_string(),
        sheet: chosen,
    })
}

/// 按扩展名自动选读取方式。
pub fn read_any(path: &Path, sheet: Option<&str>) -> R<InputTable> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "xlsx" | "xls" | "xlsm" | "ods" => read_xlsx(path, sheet),
        "csv" | "tsv" | "txt" => read_csv(path),
        other => Err(Fail::new(
            format!("不认识的表格格式：.{other}"),
            "支持 .xlsx / .xls / .ods / .csv / .tsv；其他格式请先另存为这些之一",
        )),
    }
}

/// 单元格 → 文本：整数不带小数点、日期按 ISO、布尔用 是/否。
fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.trim().to_string(),
        Data::Float(f) => {
            if (f.fract()).abs() < f64::EPSILON && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                let s = format!("{f}");
                s
            }
        }
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => {
            if *b {
                "是".into()
            } else {
                "否".into()
            }
        }
        Data::DateTime(dt) => {
            // Excel 日期转成 ISO 字符串（用 calamine 自带转换）
            match dt.as_datetime() {
                Some(d) => d.format("%Y-%m-%d").to_string(),
                None => format!("{}", dt.as_f64()),
            }
        }
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("{e:?}"),
    }
}
