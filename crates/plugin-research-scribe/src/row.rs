//! 行与草稿 CSV：草稿是**普通 CSV**（能用 Excel 打开改），列顺序与模板一致。

use std::path::Path;

pub type Row = std::collections::BTreeMap<String, String>;

/// 按给定列顺序写 CSV（UTF-8 无 BOM、LF；与项目里的表一致）。
pub fn write_csv(path: &Path, columns: &[String], rows: &[Row]) -> std::io::Result<()> {
    let mut wtr = csv::WriterBuilder::new().from_path(path)?;
    wtr.write_record(columns)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    for r in rows {
        let record: Vec<&str> = columns
            .iter()
            .map(|c| r.get(c).map(|s| s.as_str()).unwrap_or(""))
            .collect();
        wtr.write_record(&record)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
    }
    wtr.flush()?;
    Ok(())
}

/// 读回草稿 CSV（复核/入库前用；也能吃用户用 Excel 改过的草稿）。
pub fn read_csv(path: &Path) -> std::io::Result<(Vec<String>, Vec<Row>)> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_path(path)?;
    let headers: Vec<String> = rdr
        .headers()?
        .iter()
        .map(|h| h.trim_start_matches('\u{feff}').trim().to_string())
        .collect();
    let mut rows = Vec::new();
    for rec in rdr.records() {
        let rec = rec?;
        let mut row = Row::new();
        let mut all_empty = true;
        for (i, col) in headers.iter().enumerate() {
            let v = rec.get(i).unwrap_or("").trim().to_string();
            if !v.is_empty() {
                all_empty = false;
            }
            row.insert(col.clone(), v);
        }
        if !all_empty {
            rows.push(row);
        }
    }
    Ok((headers, rows))
}
