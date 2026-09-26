//! 内核桥：打印插件只通过内核命令读项目（模板 / 名字 / 编号 / 数据）。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use std::collections::BTreeMap;

type Row = BTreeMap<String, String>;
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

    /// 调内核命令（自动追加 `--json`）。
    pub fn json(&self, args: &[String]) -> R<Value> {
        let mut all = args.to_vec();
        all.push("--json".to_string());
        let out = Command::new(&self.bin)
            .args(&all)
            .output()
            .map_err(|e| Fail::new(format!("无法运行内核：{e}"), "确认内核可执行"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let value: Value = serde_json::from_str(stdout.trim())
            .map_err(|e| Fail::new(format!("内核没有返回可解析的 JSON：{e}"), ""))?;
        if !out.status.success() {
            let detail = value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("（内核没有给出原因）");
            return Err(Fail::new(format!("内核命令失败：{detail}"), ""));
        }
        Ok(value)
    }

    /// 模板（术语包已生效：中文标签就是界面与打印件上的词）。
    pub fn template(&self, project: &Path) -> R<Value> {
        let out = self.json(&["template".into(), project.display().to_string()])?;
        out.get("template")
            .cloned()
            .ok_or_else(|| Fail::new("内核未返回模板", ""))
    }

    /// 项目名。
    pub fn project_name(&self, project: &Path) -> R<String> {
        let out = self.json(&["manifest-cmd".into(), project.display().to_string()])?;
        Ok(out
            .get("manifest")
            .and_then(|m| m.get("project_name"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// 项目编号（二维码里带上它，扫码就知道是哪份数据）。
    pub fn project_id(&self, project: &Path) -> R<String> {
        let out = self.json(&["manifest-cmd".into(), project.display().to_string()])?;
        Ok(out
            .get("manifest")
            .and_then(|m| m.get("project_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// 读整张表（打印标签用；自动分页）。
    pub fn rows(&self, project: &Path, table: &str) -> R<Vec<Row>> {
        const PAGE: usize = 2000;
        let mut rows = Vec::new();
        let mut offset = 0usize;
        loop {
            let out = self.json(&[
                "query".into(),
                project.display().to_string(),
                "--table".into(),
                table.into(),
                "--limit".into(),
                PAGE.to_string(),
                "--offset".into(),
                offset.to_string(),
            ])?;
            let page = out
                .get("rows")
                .and_then(|v| v.as_array())
                .ok_or_else(|| Fail::new("内核未返回 rows", ""))?;
            for item in page {
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
                rows.push(row);
            }
            if page.len() < PAGE {
                break;
            }
            offset += page.len();
        }
        Ok(rows)
    }
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
