//! S7 验收：**加载科研模板后字段齐全；换术语包不影响数据**。
//!
//! 这个测试跨进程运行真实的插件与内核二进制：
//! 建项目 → 装田间试验模板（9 张表）→ 写一行数据 → 切农业术语 → 切医学术语，
//! 全程比对**业务数据表的字节**，并用模板标签证明"显示名确实换了"。
//!
//! 数据全部为合成数据。若内核或插件二进制未构建，测试会打印说明并跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn repo_root() -> PathBuf {
    // crates/plugin-research-template → kits/research
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn workspace_root() -> PathBuf {
    // kits/research → Cercis
    repo_root().join("../..").canonicalize().unwrap_or_else(|_| repo_root().join("../.."))
}

fn kernel_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CERCIS_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let dev = workspace_root().join("kernel/target/debug/cercis");
    dev.is_file().then(|| dev.canonicalize().unwrap_or(dev))
}

fn plugin_bin() -> Option<PathBuf> {
    let p = workspace_root().join("kits/research/target/debug/cercis-plugin-research-template");
    p.is_file().then(|| p.canonicalize().unwrap_or(p))
}

fn is_stale(bin: &Path) -> bool {
    let Ok(bin_time) = std::fs::metadata(bin).and_then(|m| m.modified()) else {
        return true;
    };
    newest_source_time(&repo_root().join("crates")).is_some_and(|t| t > bin_time)
}

fn newest_source_time(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(t) = newest_source_time(&path) {
                newest = Some(newest.map_or(t, |n: std::time::SystemTime| n.max(t)));
            }
            continue;
        }
        let is_source = path.extension().map(|e| e == "rs").unwrap_or(false)
            || path.file_name().map(|n| n == "Cargo.toml").unwrap_or(false);
        if !is_source {
            continue;
        }
        if let Ok(t) = std::fs::metadata(&path).and_then(|m| m.modified()) {
            newest = Some(newest.map_or(t, |n: std::time::SystemTime| n.max(t)));
        }
    }
    newest
}

fn run(bin: &Path, args: &[String], envs: &[(&str, String)]) -> (i32, String) {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap_or_else(|e| panic!("无法运行 {}：{e}", bin.display()));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

const TABLES: [&str; 9] = [
    "studies",
    "factors",
    "treatments",
    "units",
    "events",
    "samples",
    "observations",
    "methods",
    "media",
];

fn business_digest(project: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for t in TABLES {
        let bytes = std::fs::read(project.join("tables").join(format!("{t}.csv")))
            .unwrap_or_else(|e| panic!("读不到 {t}.csv：{e}"));
        hasher.update(&bytes);
    }
    hex::encode(hasher.finalize())
}

#[test]
fn research_template_and_term_packs() {
    let (Some(kernel), Some(plugin)) = (kernel_bin(), plugin_bin()) else {
        eprintln!("[跳过] 内核或科研插件二进制未构建（先 cargo build）");
        return;
    };
    if is_stale(&plugin) {
        eprintln!("[跳过] 插件二进制比源码旧：先 cargo build");
        return;
    }

    let work = std::env::temp_dir().join("cercis-s7-flow");
    let _ = std::fs::remove_dir_all(&work);
    let project = work.join("proj");

    let envs: Vec<(&str, String)> = vec![
        ("CERCIS_BIN", kernel.display().to_string()),
        ("CERCIS_RESEARCH_DIR", repo_root().display().to_string()),
    ];

    // 1. 建项目
    let (code, out) = run(
        &kernel,
        &[
            "init".into(),
            project.display().to_string(),
            "--name".into(),
            "S7 验收（合成数据）".into(),
            "--json".into(),
        ],
        &envs,
    );
    assert_eq!(code, 0, "建项目失败：{out}");

    // 2. 装模板 + 术语包（走插件，自带 --allow-write 权限闸门）
    let (code, out) = run(
        &plugin,
        &[
            "install".into(),
            "--project".into(),
            project.display().to_string(),
            "--template".into(),
            "field-trial".into(),
            "--terms".into(),
            "agriculture".into(),
            "--allow-write".into(),
            "--json".into(),
        ],
        &envs,
    );
    assert_eq!(code, 0, "装模板失败：{out}");
    let installed: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(installed["template"], "field-trial");
    assert_eq!(installed["terms_profile"], "agriculture");
    let created: Vec<&str> = installed["created_tables"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for t in TABLES {
        assert!(created.contains(&t), "缺少表 {t}：{created:?}");
    }

    // 3. 写一行数据（研究 → 小区），让"数据表"真的非空
    for (table, sets) in [
        ("studies", vec!["study_id=ST-2026-001", "name=合成试验"]),
        ("units", vec!["unit_id=U-001", "study_id=ST-2026-001", "block=B1"]),
    ] {
        let mut args = vec![
            "append".into(),
            project.display().to_string(),
            "--table".into(),
            table.into(),
            "--json".into(),
        ];
        for s in sets {
            args.push("--set".into());
            args.push(s.into());
        }
        let (code, out) = run(&kernel, &args, &envs);
        assert_eq!(code, 0, "写 {table} 失败：{out}");
    }

    // 4. 农业术语下：表名是"小区"
    let (code, out) = run(
        &kernel,
        &[
            "template".into(),
            project.display().to_string(),
            "--json".into(),
        ],
        &envs,
    );
    assert_eq!(code, 0);
    let t1: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(t1["terms_profile"], "agriculture");
    let label1 = t1["template"]["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "units")
        .unwrap()["label_cn"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(label1, "小区");
    let digest_agri = business_digest(&project);

    // 5. 切到医学术语：标签变了，数据没变
    let (code, out) = run(
        &plugin,
        &[
            "terms".into(),
            "--project".into(),
            project.display().to_string(),
            "--pack".into(),
            "medicine".into(),
            "--allow-write".into(),
            "--json".into(),
        ],
        &envs,
    );
    assert_eq!(code, 0, "切术语包失败：{out}");
    let (_, out) = run(
        &kernel,
        &[
            "template".into(),
            project.display().to_string(),
            "--json".into(),
        ],
        &envs,
    );
    let t2: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(t2["terms_profile"], "medicine");
    let label2 = t2["template"]["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "units")
        .unwrap()["label_cn"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(label2, "受试单元");

    let digest_med = business_digest(&project);
    assert_eq!(
        digest_agri, digest_med,
        "换术语包不得改动任何业务数据表（列名、数据都算）"
    );

    // 6. 列名永远不变（数据契约不受术语影响）
    let units_csv = std::fs::read_to_string(project.join("tables/units.csv")).unwrap();
    let header = units_csv.lines().next().unwrap();
    assert_eq!(header, "unit_id,study_id,block,treatment_id,area_m2,notes");

    // 7. 切换术语包有留痕（但不写业务数据）
    let log = std::fs::read_to_string(project.join("tables/change_log.csv")).unwrap();
    let terms_entries = log.lines().filter(|l| l.contains(",terms,")).count();
    assert!(terms_entries >= 2, "两次切换都该留痕：{terms_entries}");

    // 8. 装了模板的项目应通过校验（引用完整性 + 字段规则）
    let (code, out) = run(
        &kernel,
        &[
            "validate".into(),
            project.display().to_string(),
            "--json".into(),
        ],
        &envs,
    );
    assert_eq!(code, 0, "校验不通过：{out}");

    let _ = std::fs::remove_dir_all(&work);
}
