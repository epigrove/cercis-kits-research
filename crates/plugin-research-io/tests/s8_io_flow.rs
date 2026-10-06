//! S8 验收：科研导入导出走完整链路（跨进程跑真内核 + 真插件）。
//!
//! 路线：建项目 → 装田间试验模板 → 导入前置表（研究/事件/样本）→
//! **导入 Excel 手写记录表** → 生成数据说明书 → 导出 SPSS → 导出 R（含宽表）。
//!
//! 数据全部为合成数据；Excel 夹具**在测试运行时生成**（仓库里不放二进制：
//! 发布守卫对 .xlsx 一律按"可能是真实数据"处理，运行时生成就不必开例外）。
//!
//! 二进制没构建时跳过并说明原因，不假装通过。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn workspace_root() -> PathBuf {
    // crates/plugin-research-io → kits/research
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

/// 写一个最小可读的 .xlsx（zip + 内联字符串）。
///
/// 仓库里不放二进制夹具：`cercis guard` 对 `.xlsx` 一律按"可能是真实记录"拒绝，
/// 而测试只需要"一个能被 calamine 读到的表"，运行时造出来最干净。
fn write_minimal_xlsx(path: &Path, rows: &[Vec<&str>]) {
    let mut sheet = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>",
    );
    for (r, row) in rows.iter().enumerate() {
        sheet.push_str(&format!("<row r=\"{}\">", r + 1));
        for (c, cell) in row.iter().enumerate() {
            let col = (b'A' + c as u8) as char;
            sheet.push_str(&format!(
                "<c r=\"{col}{}\" t=\"inlineStr\"><is><t>{}</t></is></c>",
                r + 1,
                xml_escape(cell)
            ));
        }
        sheet.push_str("</row>");
    }
    sheet.push_str("</sheetData></worksheet>");

    let parts: Vec<(&str, String)> = vec![
        (
            "[Content_Types].xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
             <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
             <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
             <Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
             <Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>\
             </Types>"
                .to_string(),
        ),
        (
            "_rels/.rels",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
             <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
             </Relationships>"
                .to_string(),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
             <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/>\
             </Relationships>"
                .to_string(),
        ),
        (
            "xl/workbook.xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
             xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
             <sheets><sheet name=\"观测记录\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
                .to_string(),
        ),
        ("xl/worksheets/sheet1.xml", sheet),
    ];
    std::fs::write(path, zip_stored(&parts)).expect("写 xlsx 夹具");
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// 最简 zip（只用 stored，不压缩）：局部头 + 数据 + 中央目录 + 结束记录。
fn zip_stored(parts: &[(&str, String)]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, body) in parts {
        let data = body.as_bytes();
        let crc = crc32(data);
        let offset = out.len() as u32;
        // 局部文件头
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        // 中央目录项
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    // 结束记录
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // disk
    out.extend_from_slice(&0u16.to_le_bytes()); // disk with cd
    out.extend_from_slice(&(parts.len() as u16).to_le_bytes());
    out.extend_from_slice(&(parts.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cercis-s8-{name}"));
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

/// 内核命令（必须成功）。
fn kernel_json(kernel: &Path, args: &[&str]) -> Value {
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (code, stdout, stderr) = run(kernel, &owned);
    assert_eq!(code, 0, "内核命令失败 {args:?}\n{stderr}");
    json_of(&stdout)
}

/// 插件命令。
fn plugin(bin: &Path, kernel: &Path, args: &[&str]) -> (i32, Value) {
    let mut owned: Vec<String> = vec!["--cercis".into(), kernel.display().to_string()];
    owned.extend(args.iter().map(|s| s.to_string()));
    let (code, stdout, _stderr) = run(bin, &owned);
    (code, json_of(&stdout))
}

#[test]
fn s8_import_codebook_and_exports() {
    let Some(kernel) = kernel_bin() else {
        skip("内核二进制未构建（先在 kernel 目录 cargo build）");
        return;
    };
    let Some(io) = plugin_bin("cercis-plugin-research-io") else {
        skip("插件二进制未构建（先在 kits/research 目录 cargo build）");
        return;
    };
    let Some(template_plugin) = plugin_bin("cercis-plugin-research-template") else {
        skip("科研模板插件未构建");
        return;
    };

    let dir = tmp("flow");
    let project = dir.join("proj");
    let p = project.display().to_string();
    kernel_json(&kernel, &["init", &p, "--name", "S8 验收", "--json"]);

    // 装模板：走工具包的模板插件（它会调内核的 install-template --file）
    let (code, out) = plugin(
        &template_plugin,
        &kernel,
        &[
            "install",
            "--project",
            &p,
            "--template",
            "field-trial",
            "--terms",
            "agriculture",
            "--allow-write",
            "--json",
        ],
    );
    assert_eq!(code, 0, "装模板失败：{out}");

    // 前置数据：研究 → 事件 → 样本（观测要引用的都在）
    let studies = dir.join("studies.csv");
    std::fs::write(
        &studies,
        "study_id,name,crop,design,location,start_date,end_date,notes\n\
         ST-2026-001,S8 合成试验,水稻,随机区组,合成站,2026-06-01,2026-09-30,\n",
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
        assert_eq!(out.get("ok").and_then(|v| v.as_bool()), Some(true), "{out}");
    }

    // 1a. 手写表没有记录编号：应提示用 --generate-id 自动生成
    // 夹具在**运行时生成**（仓库里不放二进制：发布守卫对 .xlsx 一律按"可能是真实数据"处理）
    let fixture = dir.join("手写记录表.xlsx");
    write_minimal_xlsx(&fixture, &[
        vec!["关联样本", "指标编号", "测定值", "计量单位", "测定日期", "天气", "手写备注"],
        vec!["S-001", "plant_height_cm", "62.3", "cm", "2026-06-15", "晴", ""],
        vec!["S-001", "spad", "33.3", "SPAD", "2026-06-15", "晴", "叶片有点黄"],
        vec!["S-002", "plant_height_cm", "64", "cm", "2026-06-16", "阴", "有虫斑"],
    ]);
    let (code, out) = plugin(
        &io,
        &kernel,
        &[
            "import-xlsx",
            &p,
            "--file",
            &fixture.display().to_string(),
            "--table",
            "observations",
            "--dry-run",
            "--json",
        ],
    );
    assert_ne!(code, 0, "缺记录编号应被拦下");
    let message = out.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        message.contains("必填列") && message.contains("obs_id"),
        "错误信息应指出缺哪一列：{message}"
    );
    let hint = out.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        hint.contains("generate-id"),
        "应提示 --generate-id 自动生成编号：{hint}"
    );

    // 1b. 多余的列：默认应拒绝并指出是哪几列
    let (code, out) = plugin(
        &io,
        &kernel,
        &[
            "import-xlsx",
            &p,
            "--file",
            &fixture.display().to_string(),
            "--table",
            "observations",
            "--generate-id",
            "OB",
            "--dry-run",
            "--json",
        ],
    );
    assert_ne!(code, 0, "多余列应被拦下");
    let message = out.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        message.contains("找不到对应") && message.contains("天气"),
        "错误信息应指出哪几列对不上：{message}"
    );

    // 2. 试运行：--skip-unknown 之后应当通过，并给出类型建议
    let (code, out) = plugin(
        &io,
        &kernel,
        &[
            "import-xlsx",
            &p,
            "--file",
            &fixture.display().to_string(),
            "--table",
            "observations",
            "--skip-unknown",
            "--generate-id",
            "OB",
            "--dry-run",
            "--json",
        ],
    );
    assert_eq!(code, 0, "试运行失败：{out}");
    assert_eq!(out.get("dry_run").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(out.get("rows_mapped").and_then(|v| v.as_u64()), Some(3));
    assert_eq!(
        out.get("errors").and_then(|v| v.as_array()).map(|a| a.len()),
        Some(0),
        "合成数据不该有结构错误：{out}"
    );
    let skipped = out
        .get("skipped_columns")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(skipped, 2, "天气与手写备注应被跳过：{out}");
    let ids = out
        .get("generated_ids")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(ids.len(), 3, "手写表没有记录编号，应自动生成 3 个：{out}");
    assert_eq!(ids[0].as_str(), Some("OB-0001"), "{out}");
    let suggestions = out
        .get("type_suggestions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(!suggestions.is_empty(), "应带上内核的类型建议：{out}");

    // 3. 真导入
    let (code, out) = plugin(
        &io,
        &kernel,
        &[
            "import-xlsx",
            &p,
            "--file",
            &fixture.display().to_string(),
            "--table",
            "observations",
            "--skip-unknown",
            "--generate-id",
            "OB",
            "--json",
        ],
    );
    assert_eq!(code, 0, "导入失败：{out}");
    assert!(
        out.get("batch_id")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "应返回批次号：{out}"
    );

    // 导入后项目体检应无错误（validate 发现问题时退出码是 2，那不表示命令失败）
    let (code, stdout, stderr) = run(
        &kernel,
        &["validate".into(), p.clone(), "--json".into()],
    );
    assert!(code == 0 || code == 2, "体检没跑起来：{stderr}");
    let report = json_of(&stdout);
    assert_eq!(
        report.get("errors").and_then(|v| v.as_array()).map(|a| a.len()),
        Some(0),
        "导入后不该有校验错误：{report}"
    );

    // 4. 数据说明书
    let (code, out) = plugin(
        &io,
        &kernel,
        &["codebook", &p, "--table", "observations", "--json"],
    );
    assert_eq!(code, 0, "生成说明书失败：{out}");
    let md = out.get("markdown").and_then(|v| v.as_str()).unwrap_or("");
    assert!(md.contains("数据说明书"), "{md}");
    assert!(md.contains("sha256"), "说明书里要有数据指纹：{md}");
    assert!(md.contains("sample_id"), "说明书要列出字段：{md}");

    // 5. 导出 SPSS（长表）
    let spss_dir = dir.join("spss");
    let (code, out) = plugin(
        &io,
        &kernel,
        &[
            "export-spss",
            &p,
            "--table",
            "observations",
            "--out",
            &spss_dir.display().to_string(),
            "--codebook",
            "--json",
        ],
    );
    assert_eq!(code, 0, "导出 SPSS 失败：{out}");
    let sps = std::fs::read_to_string(spss_dir.join("observations.sps")).expect("应有 .sps");
    assert!(sps.contains("VARIABLE LABELS"), "{sps}");
    assert!(sps.contains("GET DATA"), "{sps}");
    assert!(sps.contains("F8.1"), "数值列应带 1 位小数：{sps}");
    let csv = std::fs::read_to_string(spss_dir.join("observations.csv")).expect("应有 CSV");
    assert_eq!(csv.lines().count(), 4, "表头 + 3 行：\n{csv}");
    assert!(spss_dir.join("导出说明.md").is_file(), "应写导出说明");

    // 值编码：给了 --encode-values 就把分类列按模板允许值顺序落成 1/2/3，
    // 并在 .sps 里写全 VALUE LABELS（导出的表还是原来的表——分类列在 samples 上）
    let encoded_dir = dir.join("spss-encoded");
    let (code, out) = plugin(
        &io,
        &kernel,
        &[
            "export-spss",
            &p,
            "--table",
            "samples",
            "--encode-values",
            "--out",
            &encoded_dir.display().to_string(),
            "--json",
        ],
    );
    assert_eq!(code, 0, "编码导出失败：{out}");
    let samples_csv =
        std::fs::read_to_string(encoded_dir.join("samples.csv")).expect("应有 samples.csv");
    // 列顺序是模板顺序：sample_id,event_id,unit_id,parent_sample_id,sample_type,…
    // 允许值 "土壤;植株;籽粒;水;其他" → 植株 = 2
    let first = samples_csv.lines().nth(1).unwrap_or("");
    let sample_type = first.split(',').nth(4).unwrap_or("");
    assert_eq!(
        sample_type, "2",
        "sample_type=植株 应按允许值顺序编码成 2：{first}"
    );
    let samples_sps =
        std::fs::read_to_string(encoded_dir.join("samples.sps")).expect("应有 .sps");
    assert!(samples_sps.contains("VALUE LABELS"), "{samples_sps}");
    assert!(samples_sps.contains("'植株'"), "标签里要有原文：{samples_sps}");
    assert!(samples_sps.contains("--encode-values"), "{samples_sps}");

    // 6. 导出 R + 宽表
    let r_dir = dir.join("r");
    let (code, out) = plugin(
        &io,
        &kernel,
        &[
            "export-r",
            &p,
            "--table",
            "observations",
            "--index",
            "sample_id",
            "--name-from",
            "trait_id",
            "--value",
            "value_num",
            "--out",
            &r_dir.display().to_string(),
            "--json",
        ],
    );
    assert_eq!(code, 0, "导出 R 失败：{out}");
    let script = std::fs::read_to_string(r_dir.join("read-observations.R")).expect("应有 R 脚本");
    assert!(script.contains("read.csv"), "{script}");
    let wide = std::fs::read_to_string(r_dir.join("observations.csv")).expect("应有宽表 CSV");
    let header = wide.lines().next().unwrap_or("");
    assert!(header.starts_with("sample_id"), "宽表首列应是行身份：{header}");
    assert!(header.contains("plant_height_cm") && header.contains("spad"), "{header}");

    // 7. 留痕：导出与导入都应在项目流水里
    let log = kernel_json(&kernel, &["log", &p, "--limit", "50", "--json"]);
    let text = log.to_string();
    assert!(text.contains("import"), "流水里应有导入：{text}");
    assert!(text.contains("export"), "流水里应有导出：{text}");

    let _ = std::fs::remove_dir_all(&dir);
}
