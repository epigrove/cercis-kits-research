//! 科研模板与术语包插件（`cercis-plugin-research-template`）。
//!
//! 做三件事：
//! 1. `list`    列出本工具包自带的研究模板与术语包
//! 2. `install` 把某个模板装进项目（走内核的 `install-template --file`）
//! 3. `terms`   给项目切换术语包（走内核的 `terms --file`）——**只改显示名，不动数据**
//!
//! 设计边界：本插件的"核心能力"只是**选择与调用**；真正写项目的是内核通道
//! （原子写入 + 逐次存档 + 变更流水），插件自己不碰任何数据文件。

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

/// 插件错误：说清发生了什么 + 怎么办。
struct Fail {
    message: String,
    hint: String,
}

type R<T> = std::result::Result<T, Fail>;

/// 本插件在项目里的 ID（登记台账里记的就是它）。
const PLUGIN_ID: &str = "research_template";

fn fail<T>(message: impl Into<String>, hint: impl Into<String>) -> R<T> {
    Err(Fail {
        message: message.into(),
        hint: hint.into(),
    })
}

/// 内核桥：只通过命令行 + `--json` 与内核交互（写入必然留痕）。
struct Kernel {
    bin: PathBuf,
}

impl Kernel {
    fn locate(explicit: Option<&Path>) -> R<Self> {
        if let Some(p) = explicit {
            if p.is_file() {
                return Ok(Self {
                    bin: p.to_path_buf(),
                });
            }
            return fail(
                format!("指定的内核不存在：{}", p.display()),
                "检查 --cercis 指向的可执行文件",
            );
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
        // 开发工作区：kits/research/crates/plugin-research-template → Cercis/kernel/target/debug/cercis
        let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../kernel/target/debug")
            .join(exe_name("cercis"));
        if dev.is_file() {
            return Ok(Self { bin: dev });
        }
        fail(
            "找不到 Cercis 内核（cercis 命令）",
            "用 --cercis <路径> 指定，或设置 CERCIS_BIN",
        )
    }

    fn json(&self, args: &[String]) -> R<Value> {
        let mut all = args.to_vec();
        all.push("--json".to_string());
        let out = Command::new(&self.bin).args(&all).output().map_err(|e| Fail {
            message: format!("无法运行内核 {}：{e}", self.bin.display()),
            hint: "确认内核可执行且有执行权限".into(),
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
            return fail(format!("内核命令失败：{detail}"), "看上面的原文，修好再试");
        }
        value.ok_or(Fail {
            message: "内核没有返回可解析的 JSON".into(),
            hint: "可能是内核版本不匹配".into(),
        })
    }
}

/// 本工具包自带的数据目录（模板与术语包）。
///
/// 顺序：环境变量 `CERCIS_RESEARCH_DIR` → 可执行文件旁边的 `templates/`（发行形态）
/// → 上两级（开发形态：`target/debug/…` → 仓库根）。
fn data_dir() -> R<PathBuf> {
    if let Some(p) = std::env::var_os("CERCIS_RESEARCH_DIR") {
        let p = PathBuf::from(p);
        if p.is_dir() {
            return Ok(p);
        }
    }
    let exe = std::env::current_exe().map_err(|e| Fail {
        message: format!("取不到可执行文件路径：{e}"),
        hint: String::new(),
    })?;
    let mut dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for _ in 0..3 {
        if dir.join("templates").is_dir() && dir.join("terms").is_dir() {
            return Ok(dir);
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    fail(
        "找不到科研工具包的数据目录（templates/ 与 terms/）",
        "设置环境变量 CERCIS_RESEARCH_DIR 指向 kits/research 目录",
    )
}

fn yaml_stems(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| {
                    let p = e.path();
                    match p.extension().and_then(|x| x.to_str()) {
                        Some("yaml") | Some("yml") => {
                            p.file_stem().map(|s| s.to_string_lossy().to_string())
                        }
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[derive(Parser, Debug)]
#[command(
    name = "cercis-plugin-research-template",
    version,
    about = "科研模板与术语包：装入田间试验等模板、切换农业/医学等术语（只改显示名，不动数据）"
)]
struct Cli {
    /// 机器可读输出（app 与脚本使用）
    #[arg(long, global = true)]
    json: bool,
    /// 内核可执行文件（默认 CERCIS_BIN 或 PATH 里的 cercis）
    #[arg(long, global = true)]
    cercis: Option<PathBuf>,
    /// 允许写入（装模板/切术语包都会写项目，需显式授权）
    #[arg(long, global = true)]
    allow_write: bool,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 列出本工具包自带的研究模板与术语包
    List,
    /// 把模板装进项目（可顺带切术语包）
    Install {
        /// 项目目录
        #[arg(long)]
        project: PathBuf,
        /// 模板名（如 field-trial）
        #[arg(long)]
        template: String,
        /// 顺带切换的术语包（如 agriculture / medicine / generic）
        #[arg(long)]
        terms: Option<String>,
    },
    /// 切换术语包（只改显示名，不动数据）
    Terms {
        /// 项目目录
        #[arg(long)]
        project: PathBuf,
        /// 术语包名（如 agriculture）
        #[arg(long)]
        pack: String,
    },
    /// 方法文档：登记与检索（方案 / SOP / 计算公式 / 偏差记录）
    Method {
        #[command(subcommand)]
        sub: MethodCmd,
    },
}

#[derive(Subcommand, Debug)]
enum MethodCmd {
    /// 登记一份方法文档：把文件收进项目的 methods/ 目录，并写一行索引
    Add {
        /// 项目目录
        #[arg(long)]
        project: PathBuf,
        /// 方法编号（如 M-001）
        #[arg(long)]
        id: String,
        /// 标题
        #[arg(long)]
        title: String,
        /// 类别：方案 / SOP / 计算公式 / 偏差记录 / 其他
        #[arg(long, default_value = "SOP")]
        category: String,
        /// 版本（如 v1.2）
        #[arg(long)]
        version: Option<String>,
        /// 源文件（可选；给了就复制进项目 methods/ 目录）
        #[arg(long)]
        source: Option<PathBuf>,
        /// 生效日期（YYYY-MM-DD）
        #[arg(long)]
        effective_date: Option<String>,
        /// 备注
        #[arg(long)]
        notes: Option<String>,
    },
    /// 列出项目里登记的方法文档
    List {
        /// 项目目录
        #[arg(long)]
        project: PathBuf,
    },
}

fn emit(json: bool, value: &Value, human: impl FnOnce(&Value)) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into())
        );
    } else {
        human(value);
    }
}

fn die(json: bool, e: &Fail) -> ! {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema": "cercis.plugin/1",
                "ok": false,
                "error": e.message,
                "hint": e.hint,
            }))
            .unwrap_or_else(|_| "{}".into())
        );
    } else {
        eprintln!("错误：{}", e.message);
        if !e.hint.is_empty() {
            eprintln!("建议：{}", e.hint);
        }
    }
    std::process::exit(1);
}

fn main() {
    let cli = Cli::parse();

    let data = match data_dir() {
        Ok(d) => d,
        Err(e) => die(cli.json, &e),
    };

    match run(&cli, &data) {
        Ok(v) => emit(cli.json, &v, human),
        Err(e) => die(cli.json, &e),
    }
}

/// 真正干活的入口：返回 `R<Value>`，`?` 才能正常向上传递错误。
fn run(cli: &Cli, data: &Path) -> R<Value> {
    match &cli.command {
        Cmd::List => Ok(json!({
            "schema": "cercis.plugin/1",
            "ok": true,
            "action": "research.list",
            "data_dir": data.display().to_string(),
            "templates": yaml_stems(&data.join("templates")),
            "terms": yaml_stems(&data.join("terms")),
        })),

        Cmd::Install {
            project,
            template,
            terms,
        } => {
            if !cli.allow_write {
                die(
                    cli.json,
                    &Fail {
                        message: "装入模板是写入操作，本次调用没有获得写入授权".into(),
                        hint: "在 app 里授予该插件写入权限；命令行使用请显式加 --allow-write".into(),
                    },
                );
            }
            let kernel = match Kernel::locate(cli.cercis.as_deref()) {
                Ok(k) => k,
                Err(e) => die(cli.json, &e),
            };
            let template_file = data.join("templates").join(format!("{template}.yaml"));
            if !template_file.is_file() {
                die(
                    cli.json,
                    &Fail {
                        message: format!("没有这个模板：{template}"),
                        hint: format!("可用：{}", yaml_stems(&data.join("templates")).join(" / ")),
                    },
                );
            }
            let mut installed = kernel.json(&[
                "install-template".into(),
                project.display().to_string(),
                "--file".into(),
                template_file.display().to_string(),
                "--plugin".into(),
                PLUGIN_ID.into(),
            ])?;

            let mut terms_profile = Value::Null;
            if let Some(pack) = terms {
                let pack_file = data.join("terms").join(format!("{pack}.yaml"));
                if !pack_file.is_file() {
                    die(
                        cli.json,
                        &Fail {
                            message: format!("没有这个术语包：{pack}"),
                            hint: format!("可用：{}", yaml_stems(&data.join("terms")).join(" / ")),
                        },
                    );
                }
                let t = kernel.json(&[
                    "terms".into(),
                    project.display().to_string(),
                    "--file".into(),
                    pack_file.display().to_string(),
                    "--plugin".into(),
                    PLUGIN_ID.into(),
                ])?;
                terms_profile = t.get("profile").cloned().unwrap_or(Value::Null);
            }

            let obj = installed.as_object_mut();
            if let Some(map) = obj {
                map.insert("action".into(), json!("research.install"));
                map.insert("template_file".into(), json!(template_file.display().to_string()));
                map.insert("terms_profile".into(), terms_profile);
            }
            Ok(installed)
        }

        Cmd::Terms { project, pack } => {
            if !cli.allow_write {
                die(
                    cli.json,
                    &Fail {
                        message: "切换术语包会写项目文件，本次调用没有获得写入授权".into(),
                        hint: "在 app 里授予该插件写入权限；命令行使用请显式加 --allow-write".into(),
                    },
                );
            }
            let kernel = match Kernel::locate(cli.cercis.as_deref()) {
                Ok(k) => k,
                Err(e) => die(cli.json, &e),
            };
            let pack_file = data.join("terms").join(format!("{pack}.yaml"));
            if !pack_file.is_file() {
                die(
                    cli.json,
                    &Fail {
                        message: format!("没有这个术语包：{pack}"),
                        hint: format!("可用：{}", yaml_stems(&data.join("terms")).join(" / ")),
                    },
                );
            }
            let mut out = kernel.json(&[
                "terms".into(),
                project.display().to_string(),
                "--file".into(),
                pack_file.display().to_string(),
                "--plugin".into(),
                PLUGIN_ID.into(),
            ])?;
            if let Some(map) = out.as_object_mut() {
                map.insert("action".into(), json!("research.terms"));
            }
            Ok(out)
        }

        Cmd::Method { sub } => match sub {
            MethodCmd::List { project } => {
                let kernel = Kernel::locate(cli.cercis.as_deref())?;
                let mut out = kernel.json(&[
                    "query".into(),
                    project.display().to_string(),
                    "--table".into(),
                    "methods".into(),
                    "--sort".into(),
                    "method_id".into(),
                    "--limit".into(),
                    "200".into(),
                ])?;
                if let Some(map) = out.as_object_mut() {
                    map.insert("action".into(), json!("research.method_list"));
                }
                Ok(out)
            }
            MethodCmd::Add {
                project,
                id,
                title,
                category,
                version,
                source,
                effective_date,
                notes,
            } => {
                if !cli.allow_write {
                    die(
                        cli.json,
                        &Fail {
                            message: "登记方法文档是写入操作，本次调用没有获得写入授权".into(),
                            hint: "在 app 里授予该插件写入权限；命令行使用请显式加 --allow-write"
                                .into(),
                        },
                    );
                }
                let kernel = Kernel::locate(cli.cercis.as_deref())?;

                // 有源文件就收进项目的 methods/ 目录（正文归项目，索引走内核）
                let mut file_ref = String::new();
                if let Some(src) = source {
                    if !src.is_file() {
                        return fail(
                            format!("源文件不存在：{}", src.display()),
                            "检查 --source 路径",
                        );
                    }
                    let base = src
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "method.md".into());
                    // 归位：内核建好了 methods/{sop,计算,记录}，按类别放进去；
                    // 方案或其他类别放 methods/ 根目录
                    let sub = match category.as_str() {
                        "SOP" => "sop",
                        "计算公式" => "计算",
                        "偏差记录" => "记录",
                        _ => "",
                    };
                    let dest_dir = project.join("methods").join(sub);
                    std::fs::create_dir_all(&dest_dir).map_err(|e| Fail {
                        message: format!("创建 methods/ 目录失败：{e}"),
                        hint: String::new(),
                    })?;
                    let dest = dest_dir.join(format!("{id}-{base}"));
                    std::fs::copy(src, dest.clone()).map_err(|e| Fail {
                        message: format!("复制方法文档失败：{e}"),
                        hint: String::new(),
                    })?;
                    file_ref = if sub.is_empty() {
                        format!("methods/{id}-{base}")
                    } else {
                        format!("methods/{sub}/{id}-{base}")
                    };
                }

                // 索引行通过内核写入（原子 + 留痕 + 校验）
                let mut args = vec![
                    "append".into(),
                    project.display().to_string(),
                    "--table".into(),
                    "methods".into(),
                    "--target".into(),
                    id.clone(),
                    "--summary".into(),
                    format!("登记方法文档：{title}"),
                ];
                for (key, value) in [
                    ("method_id", Some(id.clone())),
                    ("title", Some(title.clone())),
                    ("category", Some(category.clone())),
                ] {
                    if let Some(v) = value {
                        args.push("--set".into());
                        args.push(format!("{key}={v}"));
                    }
                }
                for (key, value) in [
                    ("version", version.clone()),
                    ("file", {
                        if file_ref.is_empty() {
                            None
                        } else {
                            Some(file_ref.clone())
                        }
                    }),
                    ("effective_date", effective_date.clone()),
                    ("notes", notes.clone()),
                ] {
                    if let Some(v) = value {
                        args.push("--set".into());
                        args.push(format!("{key}={v}"));
                    }
                }
                let mut out = kernel.json(&args)?;
                if let Some(map) = out.as_object_mut() {
                    map.insert("action".into(), json!("research.method_add"));
                    map.insert("method_id".into(), json!(id));
                    map.insert("file".into(), json!(file_ref));
                }
                Ok(out)
            }
        },
    }
}

fn human(v: &Value) {
    match v["action"].as_str().unwrap_or("") {
        "research.list" => {
            println!("科研工具包数据目录：{}", v["data_dir"].as_str().unwrap_or(""));
            let templates = v["templates"].as_array().cloned().unwrap_or_default();
            println!("研究模板（{}）:", templates.len());
            for t in &templates {
                println!("  - {}", t.as_str().unwrap_or(""));
            }
            let terms = v["terms"].as_array().cloned().unwrap_or_default();
            println!("术语包（{}）:", terms.len());
            for t in &terms {
                println!("  - {}", t.as_str().unwrap_or(""));
            }
        }
        "research.install" => {
            println!("已装入模板：{}", v["template"].as_str().unwrap_or(""));
            if let Some(tables) = v["created_tables"].as_array() {
                let names: Vec<&str> = tables.iter().filter_map(|t| t.as_str()).collect();
                println!("新建表：{}", names.join("、"));
            }
            if !v["terms_profile"].is_null() {
                println!("术语包：{}", v["terms_profile"].as_str().unwrap_or(""));
            }
        }
        "research.terms" => {
            println!(
                "已切换术语包：{}（{}）——覆盖 {} 处标签，数据文件未改动",
                v["profile"].as_str().unwrap_or(""),
                v["label_cn"].as_str().unwrap_or(""),
                v["overrides"].as_u64().unwrap_or(0)
            );
        }
        "research.method_add" => {
            println!("已登记方法文档：{}", v["method_id"].as_str().unwrap_or(""));
            let file = v["file"].as_str().unwrap_or("");
            if !file.is_empty() {
                println!("正文位置：{file}");
            }
            println!("（索引行经内核写入，已留痕）");
        }
        "research.method_list" => {
            let rows = v["rows"].as_array().cloned().unwrap_or_default();
            println!("方法文档（{} 份）:", rows.len());
            for r in rows {
                println!(
                    "  - {} {} [{}] {}",
                    r["method_id"].as_str().unwrap_or(""),
                    r["title"].as_str().unwrap_or(""),
                    r["category"].as_str().unwrap_or(""),
                    r["file"].as_str().unwrap_or("")
                );
            }
        }
        _ => println!("{}", serde_json::to_string_pretty(v).unwrap_or_default()),
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
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}
