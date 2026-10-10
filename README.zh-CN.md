[English](README.md) | **中文**

# 科研工具包（cercis-kits-research）

Cercis 的科研专用插件集合。**与基础工具包（`cercis-kits-basic`）是两个独立仓库**：
两者互不依赖，可分别安装、分别迭代；都只通过内核命令行 + `--json` 干活。

> **分发方式**：本仓库**不随 app 安装包交付**（安装包只含内核 + 基础工具包）。
> 仓库根目录会提供 `marketplace.json`，作为 app 内**默认预置的插件市场**——
> 用户在 app 里点安装即可获得，**默认不下载**（需求文档 D35 / FR-P44–P50）。
> 在插件市场落地（S10）之前，本工具包用"手动放进项目 `plugins/`"的方式试用。

## 与基础工具包的分工

| | 基础工具包 | 科研工具包（本仓库） |
|---|---|---|
| 面向 | 任何行业（科研、企业台账、库存、设备记录……） | 科研场景 |
| 内容 | 项目向导、通用录入、通用导入导出、通用体检、插件管理器 | 科研字段模板、术语包、学科校验、xlsx/docx 与扫描流程、SPSS/R 导出、打印与二维码、历史审阅、NAS、待办 |
| 仓库 | `cercis-kits-basic` | `cercis-kits-research` |

> 判据：**其他行业也用得上的功能，放基础工具包**；只有科研用得上的，放本仓库
> （需求文档 §14.4 的"共用下沿"原则）。

## 当前状态

**科研工具包已成形（27 项测试）**：四个插件——模板与术语、导入导出（xlsx / SPSS / R）、
打印与二维码标签、田间录入员（语音 + 手稿，逐格置信度）。代码结构如下：

```
research/
├── templates/field-trial.yaml   田间试验模板（9 张表，引用与取值范围已声明）
├── terms/                      术语包：generic / agriculture / medicine
├── crates/
│   ├── plugin-research-template/  插件 1 · 模板与术语（装模板、切术语、登记方法文档）
│   ├── plugin-research-io/        插件 2 · 导入导出（xlsx / codebook / SPSS / R）
│   ├── plugin-research-print/     插件 3 · 打印记录表与二维码标签
│   └── plugin-research-scribe/    插件 4 · 田间录入员（语音 + 手稿 → 待确认草稿）
├── plugins/                    各插件的清单（plugin.yaml）与说明
└── marketplace.json            本仓库作为 app 默认预置的插件市场清单
```

```bash
# 构建与测试（需要内核已构建）
cargo build && cargo test     # 含跨进程端到端：装模板 → 写数据 → 换术语包 → 数据字节不变

# 手工试一遍
export CERCIS_BIN=<内核路径> CERCIS_RESEARCH_DIR=$PWD
./target/debug/cercis-plugin-research-template list
./target/debug/cercis-plugin-research-template install --project <项目> --template field-trial --terms agriculture --allow-write
```

已经做完的（原 S7–S9）：

| 步骤 | 内容 | 验收 |
|------|------|------|
| S7 ✅ | 科研字段模板、术语包、方法文档 | 加载科研模板后界面出现研究/小区/样本/观测等字段；换术语包不影响数据（**已通过字节级测试**） |
| S8 ✅ | xlsx 导入向导、codebook、SPSS / R 导出（含值编码与标签） | 记录表导入成功、整批校验、导出说明书可回溯 |
| S9 ✅ | 打印记录表与二维码标签、田间录入员（语音 + 手稿 + 逐格置信度 + 复核入库） | 跑通"生成记录表 → 手写 → 扫描 → 核对 → 入库"；app 里能重放原音、放大原图、逐格确认 |

> 打印导出 PDF 需要本机有 Chrome / Edge（没有就先出 HTML；可用 `CERCIS_BROWSER` 指定浏览器）。
> NAS 与工作副本属于**内核**能力（`cercis remote` / `cercis workcopy`），科研包不再重复实现。

## 计划中的插件形态

与基础工具包一致：**一组独立插件**，每个插件一份 `plugin.yaml` + 一个可执行入口，
可单独启停与授权；写入一律走内核通道，默认只读。

```
research/
├── crates/          各插件的核心能力（Rust，平台无关）
└── plugins/         随包分发的插件清单与说明
```

## 许可

Apache-2.0（见 `LICENSE` 与 `NOTICE`）。
