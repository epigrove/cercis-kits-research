//! HTML 生成：记录表与标签页。
//!
//! 为什么用 HTML 而不是直接画 PDF：打印这件事的终点是"一张纸"——
//! 浏览器里 `Cmd+P` 就能出 PDF，样式（A4 边距、分页）用 CSS 控制最省事，
//! 而且生成的 HTML 本身可读、可改、可存档。本机装了 Chrome/Edge 时额外再给一份 PDF。

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Fail, R};

/// 记录表里的一列。
#[derive(Debug, Clone)]
pub struct PrintField {
    pub name: String,
    pub label: String,
    pub unit: String,
    pub allow: String,
    pub required: bool,
}

/// 记录表的输入。
pub struct SheetInput {
    pub project_name: String,
    pub title: String,
    pub table_label: String,
    /// 表头预填（如批次号）
    pub head: Vec<(String, String)>,
    pub fields: Vec<PrintField>,
    pub rows: usize,
    pub printed_at: String,
}

/// 生成可打印的记录表（A4，竖排）。
pub fn record_sheet(input: &SheetInput) -> String {
    let mut s = String::new();
    s.push_str(&doc_head(&input.title));
    s.push_str(&format!(
        "<h1>{}</h1>\n<p class=\"meta\">项目：{}｜表：{}｜打印：{}</p>\n",
        esc(&input.title),
        esc(&input.project_name),
        esc(&input.table_label),
        esc(&input.printed_at)
    ));
    if !input.head.is_empty() {
        s.push_str("<table class=\"head\">\n");
        for (k, v) in &input.head {
            s.push_str(&format!(
                "<tr><th>{}</th><td>{}</td></tr>\n",
                esc(k),
                if v.is_empty() {
                    "&nbsp;".to_string()
                } else {
                    esc(v)
                }
            ));
        }
        s.push_str("</table>\n");
    }
    // 表头两行：中文名（+单位） / 列名（便于回录时对应）
    s.push_str("<table class=\"grid\">\n<thead>\n<tr>");
    for f in &input.fields {
        let unit = if f.unit.is_empty() {
            String::new()
        } else {
            format!("（{}）", f.unit)
        };
        let req = if f.required { " *" } else { "" };
        s.push_str(&format!(
            "<th>{}<span class=\"unit\">{unit}</span>{req}</th>",
            esc(&f.label)
        ));
    }
    s.push_str("</tr>\n<tr class=\"cols\">");
    for f in &input.fields {
        s.push_str(&format!("<th>{}</th>", esc(&f.name)));
    }
    s.push_str("</tr>\n</thead>\n<tbody>\n");
    for _ in 0..input.rows {
        s.push_str("<tr>");
        for _ in &input.fields {
            s.push_str("<td>&nbsp;</td>");
        }
        s.push_str("</tr>\n");
    }
    s.push_str("</tbody>\n</table>\n");

    // 允许值提示（手写时对着看）
    let hints: Vec<String> = input
        .fields
        .iter()
        .filter(|f| !f.allow.is_empty())
        .map(|f| {
            format!(
                "{}：{}",
                esc(&f.label),
                esc(&f.allow.replace(';', " / "))
            )
        })
        .collect();
    if !hints.is_empty() {
        s.push_str("<div class=\"hints\"><strong>取值提示</strong>");
        for h in hints {
            s.push_str(&format!("<div>{h}</div>"));
        }
        s.push_str("</div>\n");
    }
    s.push_str("<p class=\"foot\">填写约定：缺失留空，不要写 NA / - / 0；数字不用千分位；日期写 2026-06-15。核对后交给 Cercis 回录（拍照或念读均可）。</p>\n");
    s.push_str("</body></html>\n");
    s
}

/// 标签页输入。
pub struct LabelInput {
    pub project_name: String,
    pub project_id: String,
    pub table: String,
    pub table_label: String,
    pub field: String,
    pub field_label: String,
    pub per_row: usize,
    /// (值, 小字备注)
    pub items: Vec<(String, String)>,
}

/// 生成二维码标签页（返回 HTML 与标签数）。
pub fn label_sheet(input: &LabelInput) -> R<(String, usize)> {
    let mut s = String::new();
    s.push_str(&doc_head("标签"));
    s.push_str(&format!(
        "<h1>{}标签</h1>\n<p class=\"meta\">项目：{}｜表：{}｜字段：{}（{}）｜共 {} 张</p>\n",
        esc(&input.table_label),
        esc(&input.project_name),
        esc(&input.table_label),
        esc(&input.field_label),
        esc(&input.field),
        input.items.len()
    ));
    s.push_str("<div class=\"labels\">\n");
    for (value, note) in &input.items {
        // 二维码内容：带上项目编号与表名，扫码就知道"这是哪份数据里的哪条记录"
        let payload = format!(
            "cercis:{}:{}:{}",
            input.project_id, input.table, value
        );
        let svg = qr_svg(&payload)?;
        s.push_str(&format!(
            "<div class=\"label\" style=\"width:{w}%\">\n  <div class=\"qr\">{svg}</div>\n  <div class=\"text\"><div class=\"id\">{}</div><div class=\"sub\">{}</div>{}\n    <div class=\"code\">{}</div></div>\n</div>\n",
            esc(value),
            esc(&input.field_label),
            if note.trim().is_empty() {
                String::new()
            } else {
                format!("<div class=\"sub\">{}</div>", esc(note))
            },
            esc(&payload),
            w = (100 / input.per_row.max(1)).saturating_sub(1)
        ));
    }
    s.push_str("</div>\n");
    s.push_str("<p class=\"foot\">打印到标签纸 / A4 不干胶后剪开；二维码内容形如 <code>cercis:项目编号:表:编号</code>。</p>\n");
    s.push_str("</body></html>\n");
    Ok((s, input.items.len()))
}

/// 把一段文本编成二维码 SVG（离线，不联网）。
fn qr_svg(payload: &str) -> R<String> {
    use qrcode::QrCode;
    let code = QrCode::new(payload.as_bytes())
        .map_err(|e| Fail::new(format!("二维码生成失败：{e}"), "内容太长时可只放编号"))?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(96, 96)
        .quiet_zone(true)
        .build();
    Ok(svg)
}

/// HTML 头（含打印样式：A4、边距、字号）。
fn doc_head(title: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>{}</title>
<style>
  :root {{ --line: #999; }}
  body {{ font-family: -apple-system, "PingFang SC", "Noto Sans CJK SC", sans-serif; color:#111; margin: 14mm; }}
  h1 {{ font-size: 16pt; margin: 0 0 4px; }}
  .meta {{ color:#555; font-size: 9pt; margin: 0 0 10px; }}
  table {{ border-collapse: collapse; width: 100%; }}
  table.head {{ width: auto; margin-bottom: 10px; font-size: 10pt; }}
  table.head th {{ text-align: left; padding: 2px 10px 2px 0; border: none; font-weight: 600; }}
  table.head td {{ border-bottom: 1px solid var(--line); min-width: 90px; padding: 2px 6px; }}
  table.grid {{ font-size: 10pt; }}
  table.grid th, table.grid td {{ border: 1px solid var(--line); padding: 6px 4px; text-align: center; }}
  table.grid thead th {{ background: #f4f4f4; }}
  tr.cols th {{ font-size: 8pt; color: #666; font-weight: 400; }}
  table.grid tbody tr {{ height: 26px; }}
  .unit {{ color:#666; font-size: 8pt; }}
  .hints {{ margin-top: 10px; font-size: 9pt; color:#333; }}
  .hints div {{ margin: 2px 0 2px 10px; }}
  .foot {{ margin-top: 10px; font-size: 8pt; color:#666; }}
  .labels {{ display: flex; flex-wrap: wrap; gap: 6px; }}
  .label {{ border: 1px dashed var(--line); padding: 6px; display: flex; gap: 8px; align-items: center; box-sizing: border-box; }}
  .label .qr svg {{ width: 84px; height: 84px; display: block; }}
  .label .id {{ font-size: 15pt; font-weight: 700; }}
  .label .sub {{ font-size: 8pt; color:#555; }}
  .label .code {{ font-size: 6pt; color:#888; word-break: break-all; }}
  @media print {{
    body {{ margin: 10mm; }}
    .foot {{ page-break-inside: avoid; }}
  }}
</style></head><body>
"#,
        esc(title)
    )
}

/// HTML 转义（表名、标签里的值都可能带特殊字符）。
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 用本机的 Chrome / Edge 把 HTML 打成 PDF。
///
/// 返回：
/// - `Ok(Some(path))` 成功
/// - `Ok(None)` 本机没有可用的浏览器（不是错误：HTML 一样能打印）
/// - `Err(原因)` 浏览器在，但没跑成（常见：没有图形会话、被沙箱挡住、首次启动慢）
///
/// 可用 `CERCIS_BROWSER` 指定浏览器可执行文件（也方便测试注入一个假的）。
pub fn to_pdf(html: &Path, pdf: &Path) -> Result<Option<PathBuf>, String> {
    let Some(browser) = find_headless_browser() else {
        return Ok(None);
    };
    // 给浏览器一个可写的临时 profile：否则它常因为默认 profile 被占用/不可写而直接退出
    let profile = std::env::temp_dir().join(format!(
        "cercis-print-profile-{}",
        std::process::id()
    ));
    let url = format!("file://{}", html.display());
    let out = Command::new(browser)
        .args([
            "--headless",
            "--disable-gpu",
            "--no-sandbox",
            "--no-pdf-header-footer",
            &format!("--user-data-dir={}", profile.display()),
            &format!("--print-to-pdf={}", pdf.display()),
            &url,
        ])
        .output()
        .map_err(|e| format!("调用浏览器失败：{e}"))?;
    let _ = std::fs::remove_dir_all(&profile);
    if out.status.success() && pdf.is_file() {
        return Ok(Some(pdf.to_path_buf()));
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let tail: String = err
        .lines()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("；");
    Err(format!(
        "浏览器没能生成 PDF（退出码 {}）{}",
        out.status.code().unwrap_or(-1),
        if tail.is_empty() {
            String::new()
        } else {
            format!("：{tail}")
        }
    ))
}

/// 找本机的无头浏览器：`CERCIS_BROWSER` 优先，其次常见安装路径。
fn find_headless_browser() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CERCIS_BROWSER") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/microsoft-edge",
        "C:/Program Files/Google/Chrome/Application/chrome.exe",
        "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, label: &str) -> PrintField {
        PrintField {
            name: name.into(),
            label: label.into(),
            unit: String::new(),
            allow: String::new(),
            required: false,
        }
    }

    #[test]
    fn record_sheet_has_rows_labels_and_hints() {
        let doc = record_sheet(&SheetInput {
            project_name: "合成试验".into(),
            title: "测定记录表".into(),
            table_label: "测定记录".into(),
            head: vec![("批次".into(), "BATCH-20260615".into())],
            fields: vec![
                PrintField {
                    required: true,
                    unit: "cm".into(),
                    ..field("plant_height_cm", "株高")
                },
                PrintField {
                    allow: "分蘖期;抽穗期;成熟期".into(),
                    ..field("stage", "生育期")
                },
            ],
            rows: 3,
            printed_at: "2026-09-26 10:00".into(),
        });
        assert!(doc.contains("测定记录表"));
        assert!(doc.contains("BATCH-20260615"), "表头预填要出现在纸上");
        assert!(doc.contains("株高") && doc.contains("（cm）"));
        assert!(doc.contains("plant_height_cm"), "列名要印出来，方便回录对应");
        assert_eq!(doc.matches("<td>&nbsp;</td>").count(), 6, "3 行 × 2 列的空格");
        assert!(doc.contains("分蘖期 / 抽穗期 / 成熟期"), "允许值提示");
        assert!(doc.contains("缺失留空"), "填写约定要印在纸上");
    }

    #[test]
    fn label_sheet_embeds_qr_svg() {
        let (doc, n) = label_sheet(&LabelInput {
            project_name: "合成试验".into(),
            project_id: "01ABCDEF".into(),
            table: "units".into(),
            table_label: "小区".into(),
            field: "unit_id".into(),
            field_label: "小区编号".into(),
            per_row: 3,
            items: vec![
                ("U-001".into(), "B1".into()),
                ("U-002".into(), String::new()),
            ],
        })
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(doc.matches("<svg").count(), 2, "每张标签一个二维码");
        assert!(doc.contains("U-001") && doc.contains("U-002"));
        assert!(doc.contains("B1"), "小字备注（区组）");
        assert!(doc.contains("cercis:01ABCDEF:units:U-001"), "扫码能定位到记录");
    }

    #[test]
    fn pdf_uses_the_configured_browser() {
        // 用一个假浏览器（shell 脚本）验证参数拼对了：它把 --print-to-pdf 指的文件写出来
        let dir = std::env::temp_dir().join("cercis-print-fake-browser");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake-browser.sh");
        std::fs::write(
            &fake,
            "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in --print-to-pdf=*) out=\"${a#--print-to-pdf=}\";; esac; done\nprintf '%%PDF-1.4 fake' > \"$out\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&fake).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&fake, perm).unwrap();
        }
        let html = dir.join("sheet.html");
        std::fs::write(&html, "<html></html>").unwrap();
        let pdf = dir.join("sheet.pdf");

        std::env::set_var("CERCIS_BROWSER", &fake);
        let got = to_pdf(&html, &pdf);
        std::env::remove_var("CERCIS_BROWSER");
        assert_eq!(got.unwrap(), Some(pdf.clone()), "应当调用我们指定的浏览器");
        assert!(pdf.is_file(), "PDF 应当被生成");
    }

    #[test]
    fn pdf_failure_is_reported_rather_than_panicking() {
        // 浏览器不可用时，to_pdf 必须**返回**结论（Ok(None) 或 Err(原因)），不能 panic——
        // 打印记录表不该因为"本机没有浏览器"就失败，HTML 永远可用。
        std::env::set_var("CERCIS_BROWSER", "/nonexistent/browser");
        let dir = std::env::temp_dir().join("cercis-print-none");
        let _ = std::fs::create_dir_all(&dir);
        let got = to_pdf(&dir.join("a.html"), &dir.join("a.pdf"));
        std::env::remove_var("CERCIS_BROWSER");
        match got {
            Ok(None) => {}
            Ok(Some(p)) => assert!(p.is_file(), "说成功就得真有文件"),
            Err(reason) => assert!(!reason.is_empty(), "失败也要说清原因"),
        }
    }

    #[test]
    fn html_is_escaped() {
        let doc = record_sheet(&SheetInput {
            project_name: "A & B <测试>".into(),
            title: "记录".into(),
            table_label: "表".into(),
            head: vec![],
            fields: vec![field("x", "列<x>")],
            rows: 1,
            printed_at: "t".into(),
        });
        assert!(doc.contains("A &amp; B &lt;测试&gt;"));
        assert!(doc.contains("列&lt;x&gt;"));
    }
}
