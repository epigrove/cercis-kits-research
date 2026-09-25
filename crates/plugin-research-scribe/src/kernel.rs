//! 内核桥：录入员只通过内核命令读写项目（读模板/类型清单，写走 append-many）。

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
            "用 --cercis <路径> 指定，或设置环境变量 CERCIS_BIN",
        ))
    }

    /// 调用内核命令（自动追加 `--json`），不因退出码非零而报错。
    pub fn call(&self, args: &[String], stdin: Option<&str>) -> R<(i32, Value)> {
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
            pipe.write_all(text.as_bytes())
                .map_err(|e| Fail::new(format!("把数据交给内核时出错：{e}"), ""))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| Fail::new(format!("等待内核返回时出错：{e}"), ""))?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let value: Value = serde_json::from_str(stdout.trim())
            .map_err(|e| Fail::new(format!("内核没有返回可解析的 JSON：{e}"), "可能是内核版本不匹配"))?;
        Ok((out.status.code().unwrap_or(-1), value))
    }

    /// 必须成功的内核调用。
    pub fn json(&self, args: &[String]) -> R<Value> {
        let (code, value) = self.call(args, None)?;
        if code != 0 {
            let detail = value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("（内核没有给出原因）");
            return Err(Fail::new(format!("内核命令失败：{detail}"), ""));
        }
        Ok(value)
    }

    /// 读模板（术语包已生效）。
    pub fn template(&self, project: &Path) -> R<Value> {
        let out = self.json(&["template".into(), project.display().to_string()])?;
        out.get("template")
            .cloned()
            .ok_or_else(|| Fail::new("内核未返回模板", ""))
    }

    /// 类型清单（内核是类型的唯一权威）。
    pub fn value_types(&self) -> R<Value> {
        let out = self.json(&["value-types".into()])?;
        Ok(out.get("value_types").cloned().unwrap_or(out))
    }

    /// 机器清单（拿模板指纹与行数）。
    pub fn manifest(&self, project: &Path) -> R<Value> {
        let out = self.json(&["manifest-cmd".into(), project.display().to_string()])?;
        Ok(out.get("manifest").cloned().unwrap_or(out))
    }

    /// 整表读取（草稿与主表对照用）。
    pub fn rows(&self, project: &Path, table: &str) -> R<Vec<Row>> {
        let out = self.json(&[
            "query".into(),
            project.display().to_string(),
            "--table".into(),
            table.into(),
            "--limit".into(),
            "50000".into(),
        ])?;
        let list = out
            .get("rows")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Fail::new("内核未返回 rows", ""))?;
        Ok(list.iter().map(row_from_json).collect())
    }

    /// 写入（一次事务、一条流水）。`dry_run` 先用内核算一遍，不行就整批不写。
    pub fn append_many(
        &self,
        project: &Path,
        table: &str,
        rows: &[Row],
        summary: &str,
        source_ref: &str,
        dry_run: bool,
    ) -> R<(i32, Value)> {
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
            "import".to_string(),
            "--summary".to_string(),
            summary.to_string(),
        ];
        if !source_ref.is_empty() {
            args.push("--source-ref".into());
            args.push(source_ref.to_string());
        }
        if dry_run {
            args.push("--dry-run".into());
        }
        self.call(&args, Some(&json))
    }

}

fn row_from_json(item: &Value) -> Row {
    let mut row = Row::new();
    if let Some(obj) = item.as_object() {
        for (k, v) in obj {
            row.insert(
                k.clone(),
                match v {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                },
            );
        }
    }
    row
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let target = exe_name(name);
    std::env::split_paths(&path)
        .map(|dir| dir.join(&target))
        .find(|p| p.is_file())
}
