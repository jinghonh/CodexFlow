# #31 关系质量人工复核材料

`v0.1-draft/` 是**代理生成的合成草案**，不是人工标注或已冻结的发布基准。它只提供维护者逐对复核的起点。不得将 `proposedLabel`、复核稿或机械校验通过解释为人工认可。全部会话、路径及命令输出均为虚构内容，不含真实 Codex 历史或凭据。

## 文件与格式

| 文件 | 用途 |
| --- | --- |
| `pairs.json` | 稳定的机器可读样本。`schemaVersion=1`，每对含两条会话的 `ThreadMetadata`、`HistoryTurn`、`HistoryItem` 形状数据、双方证据定位和代理建议。 |
| `REVIEW.md` | 逐对展示来源摘录、回合时间、建议类型、方向、依据和待复核状态。 |
| `human_reviews.jsonl` | 每行对应一对，留给维护者记录最终裁决；当前全为 `PENDING_HUMAN`。 |
| `manifest.json` | 草案版本、对数、`pairs.json` 的 SHA-256 和整体状态。 |

证据定位由 `threadId`、`turnId`、`itemId`、`field`、`changeIndex`、`excerpt`、`contentVersion` 组成，沿用领域证据的来源定位字段。字段当前指向规范化条目的 `output`，其内容版本是输出原文的 SHA-256。它是**样本定位指针**，不冒充已经由文件和命令事实提取器产生的 `SourceEvidence` 事实；后续评测应在实际候选与核心验证流程中建立对应证据，再进行模型评测。检查脚本逐对核对项目归属、会话/回合/条目层级、字段、摘录、内容版本及时间。所有证据均来自双方条目，不以解释文本代替来源记录。

`proposedLabel.type` 使用 V1 推断类型，`NONE` 表示可判定的无关系；`null` 搭配 `INSUFFICIENT_EVIDENCE` 表示材料不足，不能计入确定负例。`direction` 为 `A_TO_B`、`UNDIRECTED`、`NONE` 或 `UNKNOWN`。`A_TO_B` 表示 A 为前序、B 为后续；人工纠正方向时可填写 `B_TO_A`。无向类型按 A/B 稳定编号读取。一个项目内每对是独立合成会话，不能仅凭不同对的相似叙述推导额外关系。`annotationVersion` 与 `sampleVersion` 一同记录在清单、样本和逐对人工记录中。

## 人工复核与冻结

维护者逐对阅读 `REVIEW.md` 和 `pairs.json` 的原始条目，并填写 `human_reviews.jsonl`：

1. 对可判定样本，把 `status` 改为 `CONFIRMED`，填写 `reviewer`、`reviewedAt`、`finalType`、`finalDirection`、`finalDeterminability=DECIDABLE`；最终类型允许与代理建议不同。
2. 对确实缺少材料的样本，仍可确认该处理：`finalType=null`、`finalDirection=UNKNOWN`、`finalDeterminability=INSUFFICIENT_EVIDENCE`，并在 `notes` 中写明缺口。此类样本与 `NONE` 分开统计。
3. 对有争议的样本使用 `DISPUTED`，写明争点、处理方式和需要补充的证据。保留该样本及其原始编号；不要在看到评测结果后删除或改标签。
4. 全部样本完成处理后，维护者另建不可变的正式版本目录，记录最终人工确认和冻结时间，并固定样本、人工记录及输入规则的摘要。`v0.1-draft` 原样保留为来源，不直接改名宣称已冻结。#32 只能读取已完成人工确认的正式版本。

运行 `python3 scripts/validate_relation_review_materials.py` 验证草案。该检查只证明格式、数量、场景、指针和状态一致；语义判断仍由人完成。`scripts/build_relation_review_materials.py` 仅用于最初生成草案，**人工开始复核后不可重跑覆盖记录**。
