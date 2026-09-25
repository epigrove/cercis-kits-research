# 科研模板与术语包（research_template）

科研工具包的第一个插件：把科研字段结构装进项目，并管理领域术语。

## 三个命令

```bash
# 1. 看看这个工具包带什么
cercis-plugin-research-template list

# 2. 装模板（+ 顺带选一套术语）
cercis-plugin-research-template install \
  --project <项目> --template field-trial --terms agriculture --allow-write

# 3. 换术语包（只改显示名，不动数据）
cercis-plugin-research-template terms --project <项目> --pack medicine --allow-write

# 4. 方法文档：登记与检索
cercis-plugin-research-template method add --project <项目> \
  --id M-001 --title "SPAD 测定 SOP" --category SOP --source sop.md --allow-write
cercis-plugin-research-template method list --project <项目>
```

## 带什么

| 类型 | 内容 |
|------|------|
| 模板 | `field-trial`（田间试验）：研究 / 因素 / 处理 / 小区 / 事件 / 样本 / 观测 / 方法文档 / 媒体，共 9 张表，引用与取值范围都已声明 |
| 术语包 | `generic`（通用）、`agriculture`（农业：小区、处理、农事记录……）、`medicine`（医学：受试单元、干预、访视、标本……） |

## 两条硬规则

1. **写入只走内核通道**：装模板、切术语包、登记方法文档都调用内核命令，因此自动获得
   原子写入、逐次存档与变更流水；插件自己不碰任何数据文件。
2. **换术语包不影响数据**：术语只覆盖表/字段的**显示标签**，列名
   （`unit_id`、`treatment_id`…）、类型、引用、数据文件一律不动——本项目有自动化测试
   用**字节级比对**证明这一点（`tests/s7_flow.rs`）。

## 术语包怎么用

同一份数据，不同学科看到不同的词：

| 数据里的列 | 通用 | 农业 | 医学 |
|---|---|---|---|
| `units` 表 | 实验单元 | **小区** | 受试单元 |
| `treatments` 表 | 处理 | 处理 | **干预** |
| `events` 表 | 事件 | 农事记录 | **访视** |
| `samples` 表 | 样本 | 样品 | **标本** |
| `observations` 表 | 观测 | 测定记录 | **检查结果** |

换术语包后，界面、导出说明书、AI 材料（AGENTS.md 与契约）都会跟着换词，
但 `tables/*.csv` 的列名与内容完全不变。
