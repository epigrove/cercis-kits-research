//! S9 验收：田间录入员走完整链路（跨进程跑真内核 + 真插件）。
//!
//! 场景（全部为合成数据）：
//! 1. 建项目、装田间试验模板、导入前置数据（研究 / 事件 / 样本）
//! 2. `brief`：提示词由模板自动渲染（不用人写 prompt）
//! 3. `voice`：口述转写 + 假模型命令 → 草稿表（**主表一个字节都没动**）
//! 4. `review`：数值项与必填缺失进待确认清单；内核试运行一起判
//! 5. `commit`：没确认 → 拒绝；`--confirm` → 经内核入库，可查流水
//! 6. `photo`：手稿照片路径通过 `{image}` 占位符交给多模态命令（FR-R02 第三种输入）
//! 7. `draft --from-json`：在别的 AI 宿主里识别完，把 JSON 贴回来

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn workspace_root() -> PathBuf {
    // crates/plugin-research-scribe → kits/research
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn kernel_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CERCIS_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let dev = workspace_root().join("../../kernel/target/debug/cercis");
    dev.is_file().then(|| dev.canonicalize().unwrap_or(dev))
}

fn plugin_bin(name: &str) -> Option<PathBuf> {
    let p = workspace_root().join("target/debug").join(name);
    p.is_file().then(|| p.canonicalize().unwrap_or(p))
}

fn skip(reason: &str) -> bool {
    eprintln!("跳过：{reason}");
    true
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cercis-s9-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn run(bin: &Path, args: &[String]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("跑不起来 {}：{e}", bin.display()));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn json_of(stdout: &str) -> Value {
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("不是 JSON：{e}\n{stdout}"))
}

fn kernel_json(kernel: &Path, args: &[&str]) -> Value {
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (code, stdout, stderr) = run(kernel, &owned);
    assert!(code == 0 || code == 2, "内核命令失败 {args:?}\n{stderr}");
    json_of(&stdout)
}

fn run_scribe(bin: &Path, kernel: &Path, args: &[&str]) -> (i32, Value, String) {
    let mut owned: Vec<String> = vec!["--cercis".into(), kernel.display().to_string()];
    owned.extend(args.iter().map(|s| s.to_string()));
    let (code, stdout, stderr) = run(bin, &owned);
    (code, json_of(&stdout), stderr)
}

/// 写一个"假模型"：读掉 stdin，按参数把收到的图片路径记到文件里，然后输出固定 JSON。
fn fake_model(dir: &Path, answer: &Value, arg_log: &Path) -> String {
    let script = dir.join("fake-model.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{log}'\ncat > /dev/null\ncat <<'JSON'\n{answer}\nJSON\n",
            log = arg_log.display(),
            answer = serde_json::to_string_pretty(answer).unwrap()
        ),
    )
    .unwrap();
    format!("sh {}", script.display())
}

#[test]
fn s9_scribe_flow() {
    let Some(kernel) = kernel_bin() else {
        skip("内核二进制未构建（先在 kernel 目录 cargo build）");
        return;
    };
    let Some(scribe) = plugin_bin("cercis-plugin-research-scribe") else {
        skip("录入员插件未构建（先在 kits/research 目录 cargo build）");
        return;
    };
    let Some(template_plugin) = plugin_bin("cercis-plugin-research-template") else {
        skip("科研模板插件未构建");
        return;
    };

    let dir = tmp("flow");
    let project = dir.join("proj");
    let p = project.display().to_string();
    kernel_json(&kernel, &["init", &p, "--name", "S9 验收", "--json"]);

    // 装模板（走工具包插件）
    let args: Vec<String> = vec![
        "--cercis".into(),
        kernel.display().to_string(),
        "install".into(),
        "--project".into(),
        p.clone(),
        "--template".into(),
        "field-trial".into(),
        "--terms".into(),
        "agriculture".into(),
        "--allow-write".into(),
        "--json".into(),
    ];
    let (code, _, stderr) = run(&template_plugin, &args);
    assert_eq!(code, 0, "装模板失败：{stderr}");

    // 前置数据：研究 → 事件 → 样本（观测要引用的都在）
    let studies = dir.join("studies.csv");
    std::fs::write(
        &studies,
        "study_id,name,crop,design,location,start_date,end_date,notes\n\
         ST-2026-001,S9 合成试验,水稻,随机区组,合成站,2026-06-01,2026-09-30,\n",
    )
    .unwrap();
    let events = dir.join("events.csv");
    std::fs::write(
        &events,
        "event_id,study_id,date,stage,event_type,title,operator,notes\n\
         EV-001,ST-2026-001,2026-06-01,播种,田间操作,播种,测定员1,\n\
         EV-002,ST-2026-001,2026-06-15,分蘖期,取样,取样,测定员2,\n",
    )
    .unwrap();
    let samples = dir.join("samples.csv");
    std::fs::write(
        &samples,
        "sample_id,event_id,unit_id,parent_sample_id,sample_type,depth_cm,fresh_weight_g,notes\n\
         S-001,EV-002,,,植株,,184.5,\n\
         S-002,EV-002,,,植株,,189.0,\n",
    )
    .unwrap();
    for (file, table) in [
        (&studies, "studies"),
        (&events, "events"),
        (&samples, "samples"),
    ] {
        let out = kernel_json(
            &kernel,
            &[
                "append-many",
                &p,
                "--table",
                table,
                "--from",
                &file.display().to_string(),
                "--source-ref",
                &file.display().to_string(),
                "--json",
            ],
        );
        assert_eq!(out["ok"], true, "{out}");
    }

    // 1. 提示词由模板自动渲染
    let (code, out, stderr) = run_scribe(
        &scribe,
        &kernel,
        &["brief", &p, "--table", "observations", "--json"],
    );
    assert_eq!(code, 0, "生成提示词失败：{stderr}");
    let prompt = out["prompt"].as_str().unwrap_or("");
    assert!(prompt.contains("不得编造"), "{prompt}");
    assert!(prompt.contains("sample_id"), "{prompt}");
    assert!(prompt.contains("samples.sample_id"), "引用约束要写进提示词");
    assert!(prompt.contains("OB-\\d{4}") || prompt.contains("格式必须匹配"), "{prompt}");
    assert!(out["template_fingerprint"].as_str().unwrap_or("").len() == 16);
    let script = out["script"].as_str().unwrap_or("");
    assert!(script.contains("照着念"), "念读脚本要给人看：{script}");

    // 2. 语音：转写（带「下一条」口令）+ 假模型
    let transcript = dir.join("transcript.txt");
    std::fs::write(
        &transcript,
        "小区S-001 株高62.3厘米 六月十五日\n下一条\nS-001 SPAD 33.3\n下一条\nS-002 株高，听不太清，可能是64\n",
    )
    .unwrap();
    let answer = serde_json::json!({
        "rows": [
            {"obs_id": "OB-0001", "sample_id": "S-001", "trait_id": "plant_height_cm", "value_num": "62.3", "unit": "cm", "measured_at": "2026-06-15"},
            {"obs_id": "OB-0002", "sample_id": "S-001", "trait_id": "spad", "value_num": "33.3", "unit": "SPAD", "measured_at": "2026-06-15"},
            {"obs_id": "OB-0003", "sample_id": "S-002", "trait_id": "plant_height_cm", "value_num": "", "measured_at": "2026-06-16", "remark": "多出来的键"}
        ],
        "uncertain": [
            {"index": 3, "field": "value_num", "reason": "听不清 64 还是 60"}
        ],
        "notes": ["用户说有一株倒伏，没说要记"]
    });
    let arg_log = dir.join("model-args.txt");
    let model_cmd = fake_model(&dir, &answer, &arg_log);
    let (code, out, stderr) = run_scribe(
        &scribe,
        &kernel,
        &[
            "voice",
            &p,
            "--transcript",
            &transcript.display().to_string(),
            "--model-cmd",
            &model_cmd,
            "--table",
            "observations",
            "--label",
            "第一次试录",
            "--json",
        ],
    );
    assert_eq!(code, 0, "语音录入失败：{stderr}\n{out}");
    let session = out["session"].as_str().unwrap().to_string();
    assert_eq!(out["rows"].as_u64(), Some(3));
    assert_eq!(out["must_confirm"].as_u64(), Some(1), "数值项要进清单：{out}");
    assert_eq!(out["ignored_keys"][0], "remark", "模板外的键要丢掉并列出");

    // 会话目录里该有的东西
    let sdir = project.join("_labnote/scribe").join(&session);
    for f in [
        "session.json",
        "prompt.md",
        "script.md",
        "transcript.txt",
        "raw.txt",
        "raw.json",
        "to-model.txt",
        "fields.json",
        "待确认.md",
        "observations.draft.csv",
    ] {
        assert!(sdir.join(f).is_file(), "会话目录缺 {f}");
    }

    // 3. 确认前主表零变化
    let before = kernel_json(&kernel, &["query", &p, "--table", "observations", "--json"]);
    assert_eq!(
        before["rows"].as_array().map(|a| a.len()),
        Some(0),
        "确认前不该有任何数据进主表"
    );

    // 4. 复核：内核试运行 + 待确认清单
    let (code, out, _) = run_scribe(
        &scribe,
        &kernel,
        &["review", &p, "--session", &session, "--json"],
    );
    assert_eq!(code, 0, "复核不该报错：{out}");
    assert_eq!(out["rows"].as_u64(), Some(3));
    assert!(
        out["must_confirm"].as_u64().unwrap_or(0) >= 1,
        "数值项必须要求确认：{out}"
    );

    // 5. 没确认不让入库
    let (code, out, _) = run_scribe(
        &scribe,
        &kernel,
        &["commit", &p, "--session", &session, "--json"],
    );
    assert_ne!(code, 0, "没 --confirm 应当拒绝入库");
    assert!(
        out["error"].as_str().unwrap_or("").contains("人工确认"),
        "{out}"
    );

    // 6. 确认后入库（走内核）
    let (code, out, stderr) = run_scribe(
        &scribe,
        &kernel,
        &[
            "commit",
            &p,
            "--session",
            &session,
            "--confirm",
            "--note",
            "第一次试录，数值已核对",
            "--json",
        ],
    );
    assert_eq!(code, 0, "入库失败：{stderr}\n{out}");
    assert_eq!(out["rows_written"].as_u64(), Some(3));
    assert!(!out["batch_id"].as_str().unwrap_or("").is_empty());

    // 入库后：主表有数据、体检无错误、流水里能看到会话来源
    let after = kernel_json(&kernel, &["query", &p, "--table", "observations", "--json"]);
    assert_eq!(after["rows"].as_array().map(|a| a.len()), Some(3));
    let report = kernel_json(&kernel, &["validate", &p, "--json"]);
    assert_eq!(
        report["errors"].as_array().map(|a| a.len()),
        Some(0),
        "入库后不该有校验错误：{report}"
    );
    let log = kernel_json(&kernel, &["log", &p, "--limit", "50", "--json"]);
    assert!(
        log.to_string().contains(&format!("scribe:{session}")),
        "流水里应能追溯到会话：{log}"
    );

    // 会话元数据记录了"是谁识别的"与状态（FR-R09）
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(sdir.join("session.json")).unwrap()).unwrap();
    assert_eq!(meta["status"], "committed");
    assert_eq!(meta["template_fingerprint"].as_str().unwrap().len(), 16);
    assert!(meta["model"]["cmd"].as_str().unwrap().contains("fake-model.sh"));

    // 7. 手稿照片：{image} 占位符要真把路径交给命令
    let photo = dir.join("手稿.png");
    std::fs::write(&photo, b"PNG-fake-image-for-path-test").unwrap();
    let photo_answer = serde_json::json!({
        "rows": [{"obs_id": "OB-0004", "sample_id": "S-002", "trait_id": "spad", "value_num": "34.0", "measured_at": "2026-06-16"}],
        "uncertain": []
    });
    let photo_log = dir.join("photo-args.txt");
    let photo_cmd = fake_model(&dir, &photo_answer, &photo_log);
    let (code, out, stderr) = run_scribe(
        &scribe,
        &kernel,
        &[
            "photo",
            &p,
            "--image",
            &photo.display().to_string(),
            "--model-cmd",
            &format!("{photo_cmd} {{image}}"),
            "--table",
            "observations",
            "--label",
            "手稿补录",
            "--json",
        ],
    );
    assert_eq!(code, 0, "手稿录入失败：{stderr}\n{out}");
    let args_seen = std::fs::read_to_string(&photo_log).unwrap_or_default();
    assert!(
        args_seen.contains("手稿.png"),
        "命令应当收到照片路径，实际：{args_seen}"
    );
    let photo_session = out["session"].as_str().unwrap();
    let psdir = project.join("_labnote/scribe").join(photo_session);
    assert!(psdir.join("原图/手稿.png").is_file(), "原图应被存进会话目录");

    // 8. 在别的 AI 宿主里识别完，把 JSON 贴回来
    let json_file = dir.join("answer.json");
    std::fs::write(
        &json_file,
        serde_json::to_string(&serde_json::json!({
            "rows": [{"obs_id": "OB-0005", "sample_id": "S-001", "trait_id": "spad", "value_num": "35.5", "measured_at": "2026-06-16"}]
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, out, stderr) = run_scribe(
        &scribe,
        &kernel,
        &[
            "draft",
            &p,
            "--from-json",
            &json_file.display().to_string(),
            "--session",
            photo_session,
            "--json",
        ],
    );
    assert_eq!(code, 0, "收外部 JSON 失败：{stderr}\n{out}");
    assert_eq!(out["rows"].as_u64(), Some(1));

    // 会话列表能列出来
    let (code, out, _) = run_scribe(&scribe, &kernel, &["sessions", &p, "--json"]);
    assert_eq!(code, 0);
    assert_eq!(out["count"].as_u64(), Some(2), "应当有两个会话：{out}");

    let _ = std::fs::remove_dir_all(&dir);
}
