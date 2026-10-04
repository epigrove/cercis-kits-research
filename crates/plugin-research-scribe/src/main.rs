//! 田间录入员（`cercis-plugin-research-scribe`）——语音 + 手稿照片 → 大模型 →
//! **待确认草稿表** → 人工确认 → 经内核入库（需求文档 §7.18 / FR-R01–R09）。
//!
//! 设计要点：
//! - **确认前不碰主表**：产物都在 `_labnote/scribe/<会话>/`（FR-R01）；
//! - **不用用户写 prompt**：提示词由项目模板自动渲染，并记下模板指纹（FR-R03）；
//! - **模型可替换**：语音识别与多模态都按"命令模板"调用，本地跑优先（FR-R07 / FR-R09）；
//! - **数值必须人工过眼**：待确认清单里数值项与必填缺失一律要求确认（FR-R05）；
//! - **入库走内核**：`append-many` 一次事务、一条流水，`source_ref` 指向会话（FR-R08）。

mod brief;
mod flow;
mod kernel;
mod model;
mod row;
mod session;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};

use brief::FieldInfo;
use kernel::Kernel;
use session::Session;

/// 插件错误：一句说清发生了什么，再加一句怎么办。
#[derive(Debug)]
pub struct Fail {
    pub message: String,
    pub hint: String,
}

type R<T> = std::result::Result<T, Fail>;

impl Fail {
    pub fn new(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            hint: hint.into(),
        }
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        Fail::new(format!("文件操作失败：{e}"), "检查路径与读写权限")
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "cercis-plugin-research-scribe",
    version,
    about = "Cercis 科研工具包 · 田间录入员（语音/手稿 → 待确认草稿 → 入库）"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
    /// 内核可执行文件（默认自动查找）
    #[arg(long, global = true)]
    cercis: Option<PathBuf>,
    /// 机器可读输出
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 生成提示词与念读脚本（提示词由模板自动渲染，不用自己写）
    Brief(BriefArgs),
    /// 语音录入：录音（+ 可选转写）→ 模型 → 草稿表
    Voice(VoiceArgs),
    /// 手稿录入：照片 → 多模态模型 → 草稿表
    Photo(PhotoArgs),
    /// 把模型返回的 JSON 收进来（在别的 AI 宿主里识别完再贴回来）
    Draft(DraftArgs),
    /// 复核草稿：待确认清单 + 内核试运行
    Review(ReviewArgs),
    /// 确认后入库（走内核 append-many）
    Commit(CommitArgs),
    /// 列出录入会话
    Sessions(SessionsArgs),
}

#[derive(Args, Debug)]
struct BriefArgs {
    /// 项目目录
    project: PathBuf,
    /// 目标表（默认模板里的第一张表）
    #[arg(long)]
    table: Option<String>,
    /// 提示词写到哪里（默认 `_labnote/scribe/提示词.md`，同时打印到屏幕）
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct VoiceArgs {
    /// 项目目录
    project: PathBuf,
    /// 录音文件（可多个，按顺序）
    #[arg(long = "audio", value_name = "FILE")]
    audio: Vec<PathBuf>,
    /// 已经转写好的文本（离线路径：手机 App 或别的工具转写后拿过来）
    #[arg(long, value_name = "FILE")]
    transcript: Option<PathBuf>,
    /// 语音识别命令模板（`{audio}` 占位符）；也可用环境变量 CERCIS_ASR_CMD
    #[arg(long, value_name = "CMD")]
    asr_cmd: Option<String>,
    /// 模型命令模板（提示词走 stdin）；也可用环境变量 CERCIS_MODEL_CMD
    #[arg(long, value_name = "CMD")]
    model_cmd: Option<String>,
    /// 模型名与版本（记进会话，便于复现）
    #[arg(long, default_value = "")]
    model_name: String,
    /// 目标表（默认模板里的第一张表）
    #[arg(long)]
    table: Option<String>,
    /// 写进已有会话（默认新建一个）
    #[arg(long)]
    session: Option<String>,
    /// 会话标签（便于日后找）
    #[arg(long, default_value = "")]
    label: String,
}

#[derive(Args, Debug)]
struct PhotoArgs {
    /// 项目目录
    project: PathBuf,
    /// 手稿 / 记录表照片（可多张，按顺序）
    #[arg(long = "image", value_name = "FILE")]
    image: Vec<PathBuf>,
    /// 模型命令模板（`{image}`、`{prompt_file}` 占位符；提示词走 stdin）
    #[arg(long, value_name = "CMD")]
    model_cmd: Option<String>,
    /// 模型名与版本
    #[arg(long, default_value = "")]
    model_name: String,
    /// 目标表
    #[arg(long)]
    table: Option<String>,
    /// 写进已有会话（先念后补拍的情形：FR-R02 第三种输入）
    #[arg(long)]
    session: Option<String>,
    /// 会话标签
    #[arg(long, default_value = "")]
    label: String,
}

#[derive(Args, Debug)]
struct DraftArgs {
    /// 项目目录
    project: PathBuf,
    /// 模型返回的 JSON 文件（`-` 读标准输入）
    #[arg(long, value_name = "FILE|-")]
    from_json: String,
    /// 写进哪个会话（默认最新会话）
    #[arg(long)]
    session: Option<String>,
    /// 目标表（默认会话里记着的表）
    #[arg(long)]
    table: Option<String>,
    /// 这次是谁识别的（记进会话）
    #[arg(long, default_value = "")]
    model_name: String,
}

#[derive(Args, Debug)]
struct ReviewArgs {
    /// 项目目录
    project: PathBuf,
    /// 会话（默认最新）
    #[arg(long)]
    session: Option<String>,
    /// 表（默认会话里记着的表）
    #[arg(long)]
    table: Option<String>,
}

#[derive(Args, Debug)]
struct CommitArgs {
    /// 项目目录
    project: PathBuf,
    /// 会话（默认最新）
    #[arg(long)]
    session: Option<String>,
    /// 表（默认会话里记着的表）
    #[arg(long)]
    table: Option<String>,
    /// 我已逐条看过待确认清单（数值与必填项必须人工确认，FR-R05）
    #[arg(long)]
    confirm: bool,
    /// 写进变更流水的说明
    #[arg(long, default_value = "")]
    note: String,
}

#[derive(Args, Debug)]
struct SessionsArgs {
    /// 项目目录
    project: PathBuf,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "schema": "cercis.plugin/1",
                    "ok": false,
                    "error": e.message,
                    "hint": e.hint,
                }))
                .unwrap_or_default()
            );
        } else {
            eprintln!("错误：{}", e.message);
            if !e.hint.is_empty() {
                eprintln!("怎么办：{}", e.hint);
            }
        }
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> R<()> {
    let kernel = Kernel::locate(cli.cercis.as_deref())?;
    match &cli.command {
        Cmd::Brief(a) => brief_cmd(&kernel, cli.json, a),
        Cmd::Voice(a) => voice_cmd(&kernel, cli.json, a),
        Cmd::Photo(a) => photo_cmd(&kernel, cli.json, a),
        Cmd::Draft(a) => draft_cmd(&kernel, cli.json, a),
        Cmd::Review(a) => review_cmd(&kernel, cli.json, a),
        Cmd::Commit(a) => commit_cmd(&kernel, cli.json, a),
        Cmd::Sessions(a) => sessions_cmd(cli.json, a),
    }
}

/// 生成提示词（也能单独用来打印给别的 AI 宿主）。
fn brief_cmd(kernel: &Kernel, json_out: bool, a: &BriefArgs) -> R<()> {
    let template = kernel.template(&a.project)?;
    let types = kernel.value_types()?;
    let b = brief::render(&template, &types, a.table.as_deref())
        .map_err(|e| Fail::new(e, "用 --table 指定表名；表清单见 `cercis template <项目>`"))?;

    let dir = a.project.join("_labnote").join("scribe");
    std::fs::create_dir_all(&dir)?;
    let prompt_path = a.out.clone().unwrap_or_else(|| dir.join("提示词.md"));
    std::fs::write(&prompt_path, &b.prompt)?;
    let script_path = dir.join("念读脚本.md");
    std::fs::write(&script_path, &b.script)?;

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema": "cercis.plugin/1",
                "ok": true,
                "action": "research.scribe.brief",
                "table": b.table,
                "template_fingerprint": b.template_fingerprint,
                "prompt": b.prompt,
                "script": b.script,
                "prompt_file": prompt_path.display().to_string(),
                "script_file": script_path.display().to_string(),
            }))
            .unwrap_or_default()
        );
    } else {
        println!("{}", b.prompt);
        println!("\n（提示词已写到 {}）", prompt_path.display());
        println!("（给人念的脚本：{}）", script_path.display());
    }
    Ok(())
}

/// 语音录入。
fn voice_cmd(kernel: &Kernel, json_out: bool, a: &VoiceArgs) -> R<()> {
    if a.audio.is_empty() && a.transcript.is_none() {
        return Err(Fail::new(
            "既没有录音也没有转写文本",
            "给 --audio 录音文件，或给 --transcript 已经转写好的文本",
        ));
    }
    let template = kernel.template(&a.project)?;
    let types = kernel.value_types()?;
    let b = brief::render(&template, &types, a.table.as_deref())
        .map_err(|e| Fail::new(e, "用 --table 指定表名"))?;
    let fields = brief::table_fields(&template, &b.table);

    let s = match &a.session {
        Some(id) => Session::open(&a.project, id)?,
        None => Session::create(
            &a.project,
            if a.label.is_empty() { "语音录入" } else { &a.label },
        )?,
    };
    s.write("prompt.md", &b.prompt)?;
    s.write("script.md", &b.script)?;
    write_field_snapshot(&s, &fields)?;

    let mut inputs: Vec<Value> = Vec::new();
    for f in &a.audio {
        if !f.is_file() {
            return Err(Fail::new(
                format!("录音文件不存在：{}", f.display()),
                "确认路径；手机录的 .m4a 也能用",
            ));
        }
        let stashed = session::stash(&s, f, "原音")?;
        inputs.push(json!({
            "kind": "audio",
            "file": stashed.display().to_string(),
            "sha256_16": session::file_hash(&stashed),
        }));
    }

    // 1) 拿到转写文本（三种来源：现成文本 / 识别命令 / 手工）
    let (transcript, asr_cmd) = match &a.transcript {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| {
                Fail::new(format!("读不到转写文本 {}：{e}", p.display()), "确认路径")
            })?;
            let stashed = session::stash(&s, p, "原音")?;
            inputs.push(json!({
                "kind": "transcript",
                "file": stashed.display().to_string(),
                "sha256_16": session::file_hash(&stashed),
            }));
            (text, None)
        }
        None => {
            let cmd = a
                .asr_cmd
                .clone()
                .or_else(|| std::env::var("CERCIS_ASR_CMD").ok());
            match cmd {
                Some(cmd) => {
                    let run =
                        model::run_command(&cmd, &a.audio, &[], &s.dir, &s.path("prompt.md"), None)?;
                    if run.code != 0 {
                        return Err(Fail::new(
                            format!(
                                "语音识别命令失败（退出码 {}）：{}",
                                run.code,
                                run.stderr.trim()
                            ),
                            "确认 --asr-cmd 写对了；也可以先用别的工具转写成文本，再用 --transcript 传进来",
                        ));
                    }
                    s.write("asr.stderr.txt", &run.stderr)?;
                    (run.stdout, Some(run.cmd))
                }
                None => {
                    s.patch_meta(json!({
                        "status": "waiting_asr",
                        "table": b.table,
                        "template_fingerprint": b.template_fingerprint,
                        "inputs": inputs,
                    }))?;
                    return Err(Fail::new(
                        "没有语音识别命令，也没有转写文本",
                        "三条路任选：① --transcript 传现成转写；② --asr-cmd 'whisper-cli -m tiny -f {audio}'；③ 设环境变量 CERCIS_ASR_CMD 一次配好（录音已存进会话目录，不会丢）",
                    ));
                }
            }
        }
    };
    s.write("transcript.txt", &transcript)?;

    // 2) 分段（口令优先，FR-R04）→ 拼进提示词
    let segments = brief::split_segments(&transcript);
    let stdin_text = build_model_input(&b, &segments);
    s.write("to-model.txt", &stdin_text)?;
    s.patch_meta(json!({
        "status": "transcribed",
        "mode": "offline",
        "table": b.table,
        "template_fingerprint": b.template_fingerprint,
        "inputs": inputs,
        "segments": segments.len(),
        "asr": asr_cmd.map(|c| json!({"cmd": c, "ran_at": chrono::Utc::now().to_rfc3339()})),
    }))?;

    // 3) 交给模型（没配模型命令就留成"待识别"：把提示词交给任何 AI 宿主都行）
    let model_cmd = a
        .model_cmd
        .clone()
        .or_else(|| std::env::var("CERCIS_MODEL_CMD").ok());
    match model_cmd {
        Some(cmd) => run_model_and_draft(&s, &b, &fields, &cmd, &a.model_name, &stdin_text, json_out, "voice"),
        None => {
            s.patch_meta(json!({"status": "waiting_model"}))?;
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema": "cercis.plugin/1",
                        "ok": true,
                        "action": "research.scribe.voice",
                        "session": s.id,
                        "status": "waiting_model",
                        "segments": segments.len(),
                        "to_model": s.path("to-model.txt").display().to_string(),
                    }))
                    .unwrap_or_default()
                );
            } else {
                println!("会话：{}", s.id);
                println!("已转写并分好段（{} 条）。", segments.len());
                println!("没有配模型命令，两种走法：");
                println!(
                    "  ① 把 {} 交给任何能看文本的 AI（Codex / Claude / 本地模型都行），要它按格式返回 JSON，然后跑：",
                    s.path("to-model.txt").display()
                );
                println!(
                    "     scribe draft {} --from-json 结果.json --session {}",
                    a.project.display(),
                    s.id
                );
                println!("  ② 配一个模型命令再来一次：--model-cmd 'ollama run qwen2.5'（提示词走 stdin）");
            }
            Ok(())
        }
    }
}

/// 把提示词与分段口述拼成模型输入（提示词在前，分段在后）。
fn build_model_input(b: &brief::Brief, segments: &[String]) -> String {
    let mut s = String::new();
    s.push_str(&b.prompt);
    s.push_str("\n\n## 待整理的口述（已按「下一条 / 换行」分段，共 ");
    s.push_str(&segments.len().to_string());
    s.push_str(" 条）\n");
    if segments.is_empty() {
        s.push_str("（没有内容）\n");
    }
    for (i, seg) in segments.iter().enumerate() {
        s.push_str(&format!("\n第 {} 条：{}\n", i + 1, seg));
    }
    s
}

/// 跑模型（文本输入：语音转写）：把提示词走 stdin，收 JSON 落草稿。
fn run_model_and_draft(
    s: &Session,
    b: &brief::Brief,
    fields: &[FieldInfo],
    cmd: &str,
    model_name: &str,
    stdin_text: &str,
    json_out: bool,
    action: &str,
) -> R<()> {
    let run = model::run_command(cmd, &[], &[], &s.dir, &s.path("prompt.md"), Some(stdin_text))?;
    finish_model_run(s, b, fields, run, model_name, json_out, action)
}

/// 模型跑完之后的公共收尾：存原始输出 → 抠 JSON → 落草稿。
fn finish_model_run(
    s: &Session,
    b: &brief::Brief,
    fields: &[FieldInfo],
    run: model::ModelRun,
    model_name: &str,
    json_out: bool,
    action: &str,
) -> R<()> {
    s.write("raw.txt", &run.stdout)?;
    s.write("model.stderr.txt", &run.stderr)?;
    s.patch_meta(json!({
        "model": {
            "cmd": run.cmd,
            "name": model_name,
            "ran_at": chrono::Utc::now().to_rfc3339(),
            "exit_code": run.code,
        },
    }))?;
    if run.code != 0 {
        return Err(Fail::new(
            format!("模型命令失败（退出码 {}）：{}", run.code, run.stderr.trim()),
            "看会话目录里的 model.stderr.txt；模型必须把 JSON 打到 stdout",
        ));
    }
    let answer = model::extract_json(&run.stdout)?;
    write_draft(s, b, fields, &answer, json_out, action, model_name)
}

/// 把模型答案整理成草稿（voice / photo / draft 三条路都汇到这里）。
fn write_draft(
    s: &Session,
    b: &brief::Brief,
    fields: &[FieldInfo],
    answer: &Value,
    json_out: bool,
    action: &str,
    model_name: &str,
) -> R<()> {
    let parsed = model::parse_answer(answer, fields)?;
    if parsed.rows.is_empty() {
        return Err(Fail::new(
            "识别结果里一条记录都没有",
            "看会话目录里的 raw.txt；提示词与输出格式可在 prompt.md 里核对",
        ));
    }
    let columns: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
    let draft_path = s.path(&format!("{}.draft.csv", b.table));
    row::write_csv(&draft_path, &columns, &parsed.rows)?;
    // 逐格置信度：模型自报与规则判定取较小值（离线也有底线）
    let low = model::low_confidence_cells(&parsed.rows, &parsed.confidence, model::LOW_CONFIDENCE);
    s.write(
        &format!("{}.confidence.json", b.table),
        &serde_json::to_string_pretty(&model::confidence_json(&parsed.rows, &parsed.confidence))
            .unwrap_or_default(),
    )?;
    // 低置信度的格子并进"待确认"清单（复核时逐个点名）
    let mut uncertain = parsed.uncertain.clone();
    for l in &low {
        if !uncertain
            .iter()
            .any(|u| u.index == l.index && u.field == l.field)
        {
            uncertain.push(l.clone());
        }
    }
    s.write(
        "raw.json",
        &serde_json::to_string_pretty(answer).unwrap_or_default(),
    )?;
    let must = uncertain.iter().filter(|u| u.must_confirm).count();
    s.write(
        "待确认.md",
        &model::review_markdown(
            &s.id,
            &b.table,
            parsed.rows.len(),
            &uncertain,
            &parsed.notes,
            &parsed.ignored_keys,
            &[],
        ),
    )?;
    s.write(
        &format!("{}.uncertain.json", b.table),
        &serde_json::to_string_pretty(&model::uncertain_json(&uncertain)).unwrap_or_default(),
    )?;
    s.patch_meta(json!({
        "status": "draft",
        "table": b.table,
        "template_fingerprint": b.template_fingerprint,
        "rows": parsed.rows.len(),
        "must_confirm": must,
        "low_confidence": low.len(),
        "confidence_file": format!("{}.confidence.json", b.table),
        "ignored_keys": parsed.ignored_keys,
        "model_name": model_name,
        "draft": draft_path.display().to_string(),
    }))?;

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema": "cercis.plugin/1",
                "ok": true,
                "action": format!("research.scribe.{action}"),
                "session": s.id,
                "table": b.table,
                "draft": draft_path.display().to_string(),
                "rows": parsed.rows.len(),
                "must_confirm": must,
                "low_confidence": low.len(),
                "confidence_file": format!("{}.confidence.json", b.table),
                "uncertain": model::uncertain_json(&uncertain),
                "ignored_keys": parsed.ignored_keys,
                "template_fingerprint": b.template_fingerprint,
            }))
            .unwrap_or_default()
        );
    } else {
        println!(
            "会话 {}：识别出 {} 条，草稿在 {}",
            s.id,
            parsed.rows.len(),
            draft_path.display()
        );
        println!(
            "待确认 {must} 项（数值与必填项必须人工过眼）｜低置信度格子 {} 个｜原始输出与照片/录音都在会话目录里",
            low.len()
        );
        println!("  逐格置信度：{}", format!("{}.confidence.json", b.table));
        println!("复核：scribe review --session {}", s.id);
        println!("入库：scribe commit --session {} --confirm", s.id);
    }
    Ok(())
}

/// 把别的 AI 宿主返回的 JSON 收进来（`--from-json`）。
fn draft_cmd(kernel: &Kernel, json_out: bool, a: &DraftArgs) -> R<()> {
    let text = if a.from_json == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        std::fs::read_to_string(&a.from_json).map_err(|e| {
            Fail::new(
                format!("读不到 {}：{e}", a.from_json),
                "把模型返回的 JSON 存成文件再传进来，或直接 `--from-json -` 从标准输入读",
            )
        })?
    };
    let answer = model::extract_json(&text)?;
    let s = match &a.session {
        Some(id) => Session::open(&a.project, id)?,
        None => Session::latest(&a.project)?,
    };
    let meta = s.read_meta()?;
    let table = a
        .table
        .clone()
        .or_else(|| meta.get("table").and_then(|v| v.as_str()).map(|x| x.to_string()))
        .ok_or_else(|| Fail::new("不知道要写进哪张表", "用 --table 指定"))?;

    // 字段清单优先用会话里那份；没有就问内核要模板（换模板后仍能对上）
    let fields = match read_field_snapshot(&s) {
        Some(f) if !f.is_empty() => f,
        _ => {
            let template = kernel.template(&a.project)?;
            let f = brief::table_fields(&template, &table);
            write_field_snapshot(&s, &f)?;
            f
        }
    };
    let b = brief::Brief {
        prompt: String::new(),
        script: String::new(),
        table: table.clone(),
        template_fingerprint: meta
            .get("template_fingerprint")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    };
    write_draft(&s, &b, &fields, &answer, json_out, "draft", &a.model_name)
}

fn review_cmd(kernel: &Kernel, json_out: bool, a: &ReviewArgs) -> R<()> {
    let s = match &a.session {
        Some(id) => Session::open(&a.project, id)?,
        None => Session::latest(&a.project)?,
    };
    let meta = s.read_meta()?;
    let table = a
        .table
        .clone()
        .or_else(|| meta.get("table").and_then(|v| v.as_str()).map(|x| x.to_string()))
        .ok_or_else(|| Fail::new("不知道复核哪张表", "用 --table 指定"))?;
    let r = flow::review(kernel, &a.project, &s, &table)?;
    s.write("待确认.md", &r.markdown)?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&flow::review_json(&r, &s.id, &table)).unwrap_or_default()
        );
    } else {
        print!("{}", flow::review_human(&r, &s.id, &table));
    }
    if !r.blocking.is_empty() {
        std::process::exit(2);
    }
    Ok(())
}

fn commit_cmd(kernel: &Kernel, json_out: bool, a: &CommitArgs) -> R<()> {
    let s = match &a.session {
        Some(id) => Session::open(&a.project, id)?,
        None => Session::latest(&a.project)?,
    };
    let meta = s.read_meta()?;
    let table = a
        .table
        .clone()
        .or_else(|| meta.get("table").and_then(|v| v.as_str()).map(|x| x.to_string()))
        .ok_or_else(|| Fail::new("不知道入库哪张表", "用 --table 指定"))?;
    let draft_path = s.path(&format!("{table}.draft.csv"));
    if !draft_path.is_file() {
        return Err(Fail::new(
            format!("这个会话还没有草稿表：{}", draft_path.display()),
            "先识别出草稿（voice / photo / draft），再入库",
        ));
    }
    if !a.confirm {
        let r = flow::review(kernel, &a.project, &s, &table)?;
        let must = r.uncertain.iter().filter(|u| u.must_confirm).count();
        return Err(Fail::new(
            format!(
                "还没有人工确认（待确认 {} 项，其中必须确认 {must} 项）",
                r.uncertain.len()
            ),
            format!(
                "先 `scribe review --session {}` 逐条看过，再加 --confirm 入库",
                s.id
            ),
        ));
    }
    let out = flow::commit(kernel, &a.project, &s, &table, &a.note)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        println!(
            "已入库 {} 条（会话 {}；批次 {}）",
            out.get("rows_written").and_then(|v| v.as_u64()).unwrap_or(0),
            s.id,
            out.get("batch_id").and_then(|v| v.as_str()).unwrap_or("")
        );
        println!("原始材料与草稿都留在会话目录里，可回放、可删（删了不影响主表）。");
    }
    Ok(())
}

fn sessions_cmd(json_out: bool, a: &SessionsArgs) -> R<()> {
    let ids = session::list(&a.project);
    let mut out: Vec<Value> = Vec::new();
    for id in &ids {
        let s = Session::open(&a.project, id)?;
        let meta = s.read_meta().unwrap_or(Value::Null);
        out.push(json!({
            "session": id,
            "status": meta.get("status").and_then(|v| v.as_str()).unwrap_or(""),
            "table": meta.get("table").and_then(|v| v.as_str()).unwrap_or(""),
            "rows": meta.get("rows").and_then(|v| v.as_u64()).unwrap_or(0),
            "label": meta.get("label").and_then(|v| v.as_str()).unwrap_or(""),
            "created_at": meta.get("created_at").and_then(|v| v.as_str()).unwrap_or(""),
            "dir": s.dir.display().to_string(),
        }));
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema": "cercis.plugin/1",
                "ok": true,
                "action": "research.scribe.sessions",
                "count": out.len(),
                "sessions": out,
            }))
            .unwrap_or_default()
        );
    } else if out.is_empty() {
        println!("这个项目还没有录入会话。");
    } else {
        for s in &out {
            println!(
                "{}｜{}｜表 {}｜{} 条｜{}",
                s.get("session").and_then(|v| v.as_str()).unwrap_or(""),
                s.get("status").and_then(|v| v.as_str()).unwrap_or(""),
                s.get("table").and_then(|v| v.as_str()).unwrap_or(""),
                s.get("rows").and_then(|v| v.as_u64()).unwrap_or(0),
                s.get("label").and_then(|v| v.as_str()).unwrap_or(""),
            );
        }
    }
    Ok(())
}

/// 字段快照落盘（会话里存一份，换模板后仍能对上当时的列）。
fn write_field_snapshot(s: &Session, fields: &[FieldInfo]) -> R<()> {
    let arr: Vec<Value> = fields
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "label_cn": f.label_cn,
                "type": f.field_type,
                "required": f.required,
                "reference": f.reference,
            })
        })
        .collect();
    s.write(
        "fields.json",
        &serde_json::to_string_pretty(&Value::Array(arr)).unwrap_or_default(),
    )?;
    Ok(())
}

fn read_field_snapshot(s: &Session) -> Option<Vec<FieldInfo>> {
    let text = std::fs::read_to_string(s.path("fields.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(
        v.as_array()?
            .iter()
            .map(|f| FieldInfo {
                name: str_of(f, "name"),
                label_cn: str_of(f, "label_cn"),
                field_type: {
                    let t = str_of(f, "type");
                    if t.is_empty() {
                        "text".into()
                    } else {
                        t
                    }
                },
                required: f.get("required").and_then(|x| x.as_bool()).unwrap_or(false),
                reference: str_of(f, "reference"),
            })
            .collect(),
    )
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// 手稿照片录入。
fn photo_cmd(kernel: &Kernel, json_out: bool, a: &PhotoArgs) -> R<()> {
    if a.image.is_empty() {
        return Err(Fail::new(
            "没有给照片",
            "用 --image 传记录表照片；多张按顺序传",
        ));
    }
    let template = kernel.template(&a.project)?;
    let types = kernel.value_types()?;
    let b = brief::render(&template, &types, a.table.as_deref())
        .map_err(|e| Fail::new(e, "用 --table 指定表名"))?;
    let fields = brief::table_fields(&template, &b.table);
    let s = match &a.session {
        Some(id) => Session::open(&a.project, id)?,
        None => Session::create(
            &a.project,
            if a.label.is_empty() { "手稿录入" } else { &a.label },
        )?,
    };
    s.write("prompt.md", &b.prompt)?;
    s.write("script.md", &b.script)?;
    write_field_snapshot(&s, &fields)?;

    let mut inputs: Vec<Value> = Vec::new();
    for f in &a.image {
        if !f.is_file() {
            return Err(Fail::new(
                format!("照片不存在：{}", f.display()),
                "确认路径；手机拍的 .heic / .jpg 都能用",
            ));
        }
        let stashed = session::stash(&s, f, "原图")?;
        inputs.push(json!({
            "kind": "image",
            "file": stashed.display().to_string(),
            "sha256_16": session::file_hash(&stashed),
        }));
    }

    let mut stdin_text = String::new();
    stdin_text.push_str(&b.prompt);
    stdin_text.push_str("\n\n## 待整理的记录\n");
    stdin_text.push_str(
        "记录在手稿照片里，可能有手写改动、划线、单位缩写；看不清的格子留空，并写进 uncertain。\n",
    );
    stdin_text.push_str(&format!("共 {} 张照片。\n", a.image.len()));
    s.write("to-model.txt", &stdin_text)?;
    s.patch_meta(json!({
        "status": "captured",
        "mode": "offline",
        "table": b.table,
        "template_fingerprint": b.template_fingerprint,
        "inputs": inputs,
    }))?;

    let model_cmd = a
        .model_cmd
        .clone()
        .or_else(|| std::env::var("CERCIS_MODEL_CMD").ok());
    match model_cmd {
        Some(cmd) => {
            let run = model::run_command(
                &cmd,
                &[],
                &a.image,
                &s.dir,
                &s.path("prompt.md"),
                Some(&stdin_text),
            )?;
            finish_model_run(&s, &b, &fields, run, &a.model_name, json_out, "photo")
        }
        None => {
            s.patch_meta(json!({"status": "waiting_model"}))?;
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema": "cercis.plugin/1",
                        "ok": true,
                        "action": "research.scribe.photo",
                        "session": s.id,
                        "status": "waiting_model",
                        "images": a.image.len(),
                        "to_model": s.path("to-model.txt").display().to_string(),
                    }))
                    .unwrap_or_default()
                );
            } else {
                println!("会话：{}（照片已存好，飞行模式也不丢）", s.id);
                println!("没有配模型命令，两种走法：");
                println!(
                    "  ① 把照片与 {} 里的提示词交给能看图的多模态 AI，把返回的 JSON 存下来，然后跑：",
                    s.path("prompt.md").display()
                );
                println!(
                    "     scribe draft {} --from-json 结果.json --session {}",
                    a.project.display(),
                    s.id
                );
                println!(
                    "  ② 配一个多模态命令再来一次：--model-cmd '你的多模态命令 {{image}}'（提示词走 stdin）"
                );
            }
            Ok(())
        }
    }
}
