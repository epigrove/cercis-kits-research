//! 内核桥：科研导入导出插件只通过内核命令读写项目。
//!
//! - 读：`query --json` / `template --json`（模板已应用术语包，标签就是界面上的词）
//! - 写：`append-many --from -`（JSON 从标准输入进来，一次事务、一条流水）

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::row::Row;
use crate::{Fail, R};

pub struct Kernel {
    pub bin: PathBuf,
}

impl Kernel {
    pub fn locate(explicit: Option<&Path>) -> R<Self> {
        if let Some(p) = explicit {
            if p.is_file() {
                return Ok(Self {
                    bin: p.to_path_buf(),
                });
            }
            return Err(Fail::new(
                format!("指定的内核不存在：{}", p.display()),
                "检查 --cercis 指向的可执行文件",
            ));
        }
        if let Some(p) = std::env::var_os("CERCIS_BIN") {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Ok(Self { bin: p });
            }
        }
        if let Some(p) = find_on_path("cercis") {
            return Ok(Self { bin: p });
        }
        let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../kernel/target/debug")
            .join(exe_name("cercis"));
        if dev.is_file() {
            return Ok(Self { bin: dev });
        }
        Err(Fail::new(
            "找不到 Cercis 内核（cercis 命令）",
            "用 --cercis <路径> 指定，或设置 CERCIS_BIN",
        ))
    }

    /// 调用内核并把 JSON 输出解析出来；可选把一段文本喂给标准输入。
    pub fn call(&self, args: &[String], stdin: Option<&str>) -> R<Value> {
        let mut all = args.to_vec();
        all.push("--json".to_string());

        let mut cmd = Command::new(&self.bin);
        cmd.args(&all)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            Fail::new(
                format!("无法运行内核 {}：{e}", self.bin.display()),
                "确认内核可执行且有执行权限",
            )
        })?;

        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(text.as_bytes()).map_err(|e| {
                Fail::new(format!("把数据交给内核时出错：{e}"), "数据可能过大或内核提前退出")
            })?;
        }

        let out = child.wait_with_output().map_err(|e| {
            Fail::new(format!("等待内核返回时出错：{e}"), "")
        })?;

        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let value: Option<Value> = serde_json::from_str(stdout.trim()).ok();

        if !out.status.success() {
            let detail = value
                .as_ref()
                .and_then(|v| v.get("error"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string());
            return Err(Fail::new(format!("内核命令失败：{detail}"), ""));
        }
        value.ok_or_else(|| Fail::new("内核没有返回可解析的 JSON", "可能是内核版本不匹配"))
    }

    /// 读整张表（自动分页）。
    pub fn rows(&self, project: &Path, table: &str) -> R<Vec<Row>> {
        const PAGE: usize = 2000;
        let mut rows = Vec::new();
        let mut offset = 0usize;
        loop {
            let out = self.call(
                &[
                    "query".into(),
                    project.display().to_string(),
                    "--table".into(),
                    table.into(),
                    "--limit".into(),
                    PAGE.to_string(),
                    "--offset".into(),
                    offset.to_string(),
                ],
                None,
            )?;
            let page = out
                .get("rows")
                .and_then(|v| v.as_array())
                .ok_or_else(|| Fail::new("内核未返回 rows", ""))?;
            for item in page {
                let obj = item
                    .as_object()
                    .ok_or_else(|| Fail::new("表格行不是 JSON 对象", ""))?;
                let mut row = Row::new();
                for (k, v) in obj {
                    let cell = match v {
                        Value::String(s) => s.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    row.insert(k.clone(), cell);
                }
                rows.push(row);
            }
            if page.len() < PAGE {
                break;
            }
            offset += page.len();
        }
        Ok(rows)
    }

    /// 读模板（术语包已生效）。
    pub fn template(&self, project: &Path) -> R<Value> {
        let out = self.call(
            &["template".into(), project.display().to_string()],
            None,
        )?;
        out.get("template")
            .cloned()
            .ok_or_else(|| Fail::new("内核未返回模板", ""))
    }

    /// 类型清单（内核是类型的唯一权威；插件照它写 codebook 与导出语法）。
    pub fn value_types(&self) -> R<Value> {
        let out = self.call(&["value-types".into()], None)?;
        Ok(out.get("value_types").cloned().unwrap_or(out))
    }

    /// 机器清单（含每张表的 sha256 指纹，导出清单要引用它）。
    pub fn manifest(&self, project: &Path) -> R<Value> {
        let out = self.call(
            &[
                "manifest-cmd".into(),
                project.display().to_string(),
            ],
            None,
        )?;
        Ok(out.get("manifest").cloned().unwrap_or(out))
    }

    /// 拿"一批还没入库的行"去问内核：每一列该是什么类型。
    ///
    /// 做法：把行写成临时 CSV，调 `infer-types --file`。**规则与结论都由内核给**，
    /// 插件只负责把表格递过去（§5.9 / D36：插件不许自己写一套判断）。
    pub fn infer_types(&self, headers: &[String], rows: &[Row]) -> R<Vec<Value>> {
        if headers.is_empty() || rows.is_empty() {
            return Ok(Vec::new());
        }
        let dir = std::env::temp_dir().join(format!(
            "cercis-infer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("cells.csv");
        let text = rows_to_csv(headers, rows)?;
        std::fs::write(&path, text)?;

        let out = self.call(
            &[
                "infer-types".into(),
                "--file".into(),
                path.display().to_string(),
            ],
            None,
        );
        let _ = std::fs::remove_dir_all(&dir);
        let out = out?;
        Ok(out
            .get("columns")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// 批量写入（一次事务、一条流水）。
    #[allow(clippy::too_many_arguments)]
    pub fn append_many(
        &self,
        project: &Path,
        table: &str,
        rows: &[Row],
        action: &str,
        summary: &str,
        source_ref: &str,
        allow_new_columns: bool,
        dry_run: bool,
    ) -> R<Value> {
        let payload: Vec<Value> = rows
            .iter()
            .map(|r| {
                let mut obj = serde_json::Map::new();
                for (k, v) in r {
                    obj.insert(k.clone(), Value::String(v.clone()));
                }
                Value::Object(obj)
            })
            .collect();
        let json = serde_json::to_string(&payload)
            .map_err(|e| Fail::new(format!("行数据无法序列化：{e}"), ""))?;

        let mut args = vec![
            "append-many".to_string(),
            project.display().to_string(),
            "--table".to_string(),
            table.to_string(),
            "--from".to_string(),
            "-".to_string(),
            "--action".to_string(),
            action.to_string(),
            "--summary".to_string(),
            summary.to_string(),
        ];
        if !source_ref.is_empty() {
            args.push("--source-ref".into());
            args.push(source_ref.to_string());
        }
        if allow_new_columns {
            args.push("--allow-new-columns".into());
        }
        if dry_run {
            args.push("--dry-run".into());
        }
        self.call(&args, Some(&json))
    }
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// 行 → CSV 文本（给内核做类型推断用；写临时文件，不碰项目里的东西）。
fn rows_to_csv(headers: &[String], rows: &[Row]) -> R<String> {
    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    wtr.write_record(headers)
        .map_err(|e| Fail::new(format!("写临时 CSV 失败：{e}"), ""))?;
    for r in rows {
        let record: Vec<&str> = headers
            .iter()
            .map(|h| r.get(h).map(|s| s.as_str()).unwrap_or(""))
            .collect();
        wtr.write_record(&record)
            .map_err(|e| Fail::new(format!("写临时 CSV 失败：{e}"), ""))?;
    }
    let bytes = wtr
        .into_inner()
        .map_err(|e| Fail::new(format!("写临时 CSV 失败：{e}"), ""))?;
    String::from_utf8(bytes).map_err(|e| Fail::new(format!("临时 CSV 不是 UTF-8：{e}"), ""))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}
