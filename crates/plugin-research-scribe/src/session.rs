//! 会话与草稿区（FR-R01）：每次录入是一个会话，产物都在
//! `<项目>/_labnote/scribe/<会话ID>/`，**确认前绝不触碰主表**。
//!
//! 一个会话目录里有什么：
//!
//! ```text
//! _labnote/scribe/20260925-101530-9f3a/
//! ├── session.json      会话元数据（输入、用的识别/模型、指纹、状态）
//! ├── prompt.md         自动渲染的提示词（含模板指纹 → 可复现）
//! ├── script.md         给人念的提示脚本
//! ├── transcript.txt    转写文本（语音输入）
//! ├── raw.json          模型返回的原始 JSON（一字不改，便于追责）
//! ├── observations.draft.csv   草稿表（普通 CSV，能用 Excel 改）
//! ├── 待确认.md          待确认清单（低置信、数值、必填缺失、编号冲突）
//! └── 原图/ 原音/        原始材料（复制进来，飞行模式下也不丢）
//! ```

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::{Fail, R};

/// 会话目录名（相对项目根）。
pub const SCRIBE_DIR: &str = "_labnote/scribe";

pub struct Session {
    pub id: String,
    pub dir: PathBuf,
}

impl Session {
    /// 新建会话（ID = 本地时间 + 4 位后缀，避免同一秒撞车）。
    pub fn create(project: &Path, label: &str) -> R<Self> {
        let now = chrono::Local::now();
        let nanos = now.timestamp_subsec_nanos();
        let id = format!("{}-{:04x}", now.format("%Y%m%d-%H%M%S"), (nanos ^ 0x5a5a) & 0xffff);
        let dir = project.join(SCRIBE_DIR).join(&id);
        if dir.exists() {
            return Err(Fail::new(
                format!("会话 {id} 已存在"),
                "稍等一秒再试，或改用 --session 指定已有的会话",
            ));
        }
        std::fs::create_dir_all(dir.join("原图"))?;
        std::fs::create_dir_all(dir.join("原音"))?;
        let s = Session { id, dir };
        s.write_meta(&json!({
            "schema": "cercis.scribe_session/1",
            "session_id": s.id,
            "label": label,
            "created_at": chrono::Utc::now().to_rfc3339(),
            "status": "started",
            "mode": "offline",
            "inputs": [],
        }))?;
        Ok(s)
    }

    /// 打开已有会话。
    pub fn open(project: &Path, id: &str) -> R<Self> {
        let dir = project.join(SCRIBE_DIR).join(id);
        if !dir.is_dir() {
            return Err(Fail::new(
                format!("找不到会话 {id}"),
                format!(
                    "看看有哪些会话：{}",
                    list(project).join(" / ")
                ),
            ));
        }
        Ok(Session {
            id: id.to_string(),
            dir,
        })
    }

    /// 当前最新会话（不给 --session 时用）。
    pub fn latest(project: &Path) -> R<Self> {
        let all = list(project);
        let Some(last) = all.last() else {
            return Err(Fail::new(
                "这个项目还没有录入会话",
                "先跑一次 `scribe voice`（语音）或 `scribe photo`（手稿照片）",
            ));
        };
        Self::open(project, last)
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn write(&self, name: &str, text: &str) -> R<PathBuf> {
        let p = self.path(name);
        std::fs::write(&p, text)?;
        Ok(p)
    }

    pub fn read_meta(&self) -> R<Value> {
        let text = std::fs::read_to_string(self.path("session.json"))?;
        serde_json::from_str(&text)
            .map_err(|e| Fail::new(format!("session.json 读不出来：{e}"), "会话目录可能被手工改坏了"))
    }

    pub fn write_meta(&self, meta: &Value) -> R<()> {
        std::fs::write(
            self.path("session.json"),
            serde_json::to_string_pretty(meta).unwrap_or_default(),
        )?;
        Ok(())
    }

    /// 在元数据上打补丁（浅合并顶层键）。
    pub fn patch_meta(&self, patch: Value) -> R<Value> {
        let mut meta = self.read_meta()?;
        if let (Some(dst), Some(src)) = (meta.as_object_mut(), patch.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        self.write_meta(&meta)?;
        Ok(meta)
    }
}

/// 列出项目里的会话 ID（按名字排序 = 按时间排序）。
pub fn list(project: &Path) -> Vec<String> {
    let dir = project.join(SCRIBE_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    ids.sort();
    ids
}

/// 把输入文件复制进会话目录（飞行模式下原始材料也不会丢）。
pub fn stash(session: &Session, src: &Path, subdir: &str) -> R<PathBuf> {
    let name = src
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "input".into());
    let dest = session.dir.join(subdir).join(&name);
    if src.is_file() && src != &dest {
        std::fs::copy(src, &dest)?;
    }
    Ok(dest)
}

/// 文件指纹（原始材料的 sha256，便于"这条记录来自哪张照片"）。
pub fn file_hash(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(hex::encode(hasher.finalize())[..16].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cercis-scribe-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_open_and_list_sessions() {
        let project = tmp("session");
        let s = Session::create(&project, "第一次试录").unwrap();
        assert!(s.dir.is_dir());
        assert!(s.path("session.json").is_file());
        assert!(s.path("原图").is_dir());
        let ids = list(&project);
        assert_eq!(ids, vec![s.id.clone()]);
        let reopened = Session::open(&project, &s.id).unwrap();
        let meta = reopened.read_meta().unwrap();
        assert_eq!(meta["label"], "第一次试录");
        assert_eq!(meta["status"], "started");
    }

    #[test]
    fn patch_meta_merges_top_level_keys() {
        let project = tmp("session-patch");
        let s = Session::create(&project, "补丁").unwrap();
        let meta = s
            .patch_meta(serde_json::json!({"status": "draft", "rows": 3}))
            .unwrap();
        assert_eq!(meta["status"], "draft");
        assert_eq!(meta["rows"], 3);
        assert_eq!(meta["label"], "补丁", "原有键应保留");
    }
}
