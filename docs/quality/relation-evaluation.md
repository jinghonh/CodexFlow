# #32 真实 Jev 关系评测准备

本入口供维护者在 #31 完成人工复核后手动运行。当前 `data/relation-review/v0.1-draft/` 是代理合成草案，108 对记录均为 `PENDING_HUMAN`；它只能用于格式检查，不能产生准确率、召回率或发布结论。本文没有记录真实 Jev 运行结果。

## 冻结输入契约

在新目录中保留 `pairs.json` 和 `human_reviews.jsonl`，每对均须由人逐对裁决为 `CONFIRMED`，并填写复核人、时间、最终类型、方向和可判定性。证据不足的人工结果为 `finalType=null`、`finalDirection=UNKNOWN`、`finalDeterminability=INSUFFICIENT_EVIDENCE`；它与 `NONE` 分开。争议尚未解决、待复核及缺少人工身份的记录会被拒绝。

新目录的 `pairs.json` 顶层保持 `schemaVersion=1`，将 `humanConfirmation` 设为 `CONFIRMED`；`manifest.json` 保留样本与标注版本、对数及 `pairsSha256`，并设 `status=FROZEN`、`frozenAt`、`humanReviewsSha256`。两个 SHA-256 对应各文件的**原始字节**。冻结后不得改写这三个文件；如需修订，建立新版本目录并重新评测。草案目录原样保留。冻结操作由维护者完成，代理建议标签不能直接复制成最终人工裁决。

## 手动入口

先在 CodexFlow 应用中为当前服务地址保存真实 Jev 配置与 API Key，并使用明确的版本化模型 ID，例如 `jev-1.13.0` 形式。密钥只由现有 macOS 钥匙串读取；命令行不接受密钥参数，也不在报告中保存密钥。运行目录与正常应用数据库分离，不导入或修改普通 Codex 历史。
macOS 可能要求单独允许命令行程序读取同一钥匙串项目；如拒绝访问，入口会报凭据错误并停止。不要为绕过该错误而导出明文密钥。

从仓库根目录执行以下命令。`<应用数据目录>` 必须是该应用通过 Tauri `app.path().app_data_dir()` 使用的目录；`<评测输出目录>` 对新运行必须不存在。评测会向所配置服务发送合成来源材料并消耗真实调用额度，只有 `--run` 或 `--resume` 才能发起请求。

```sh
cargo run -p codexflow-core --example relation_quality --locked -- \
  --dataset <冻结样本目录> --check

cargo run -p codexflow-core --example relation_quality --locked -- \
  --dataset <冻结样本目录> \
  --app-data-dir <应用数据目录> \
  --output <评测输出目录> \
  --call-limit 20 --run
```

`--call-limit` 是**本次命令**跨样本的真实推理请求上限，第一阶段和第二阶段各计一次；版本化模型不需要别名探测。默认单次请求最多等待 180 秒，超时与重试按现有分析路径处理，重试仍计入上限。每对独立导入评测项目，先经过核心事实和候选生成；未进入候选集的对记录为 `candidateMiss`，不调用 Jev。候选命中后，现有分析批次调度 `JevRelationAnalyzer` 原生 `/v1/systemone` Choice 分类和证据选择，再经 `inferred::outcome` 的来源、版本、方向时间及双侧证据机械检查。评测不启动 Codex 总结或工作流命名；输入没有 Codex 总结依赖。

每对独立成项目符合草案“对与对之间没有语义联系”的生成约定。这个基准测量给定会话对的候选、关系判断与证据检查；它不测量真实项目中多条会话竞争每条最多 10 个候选邻居时的覆盖率。发布报告须写明这一外推边界。

额度耗尽会暂停当前运行。按 `Ctrl-C` 会请求取消本地在途分析并等待已取消状态；这不能保证服务端计算或计费已停止。要继续同一冻结输入、服务地址、模型和题目版本，执行：

```sh
cargo run -p codexflow-core --example relation_quality --locked -- \
  --dataset <冻结样本目录> \
  --app-data-dir <应用数据目录> \
  --output <评测输出目录> \
  --call-limit 20 --resume
```

输出目录中的 `lock.json` 固定样本与人工记录摘要、Base URL、请求模型及关系题目版本；`workspace/` 保存隔离的分析状态；`report.json` 逐对保存候选、输入版本、分析运行、两阶段 Choice 的 confidence/probabilities、实际模型版本、证据选择、核心关系结果、机械有效性与错误。文件不含 API Key。继续运行前检查锁定输入，防止混入改变后的标注、模型或题目。单项失败与暂停保留在报告里，不能跳过并宣称质量通过。
应用配置修订（包括轮换密钥）发生变化后，核心会拒绝继续旧运行；修正配置后应创建新的评测输出目录，不拼接两个配置版本的结果。

## 质量汇总

全部样本处理完成后，单独执行：

```sh
python3 scripts/summarize_relation_quality.py \
  --dataset <冻结样本目录> \
  --report <评测输出目录>/report.json \
  --output <评测输出目录>/quality-summary.json
```

脚本重新验证冻结摘要和每对人工确认；样本遗漏、运行失败、模型版本不一致、两阶段记录缺失或任何有效关系的机械证据失败时拒绝生成结论。默认展示边使用现有 `0.70` 置信度门槛。人工认可准确率分母为全部默认展示的推断边，分子为类型、方向及双方证据来源定位均与人工复核样本一致的边；另报仅类型和方向一致的边数，方便定位证据选择问题。可判定正例召回率分母为全部人工确认的可判定正例对，分子为至少有一条默认展示边符合上述全部条件的对。`NONE` 是确定负例，人工证据不足不计入正例分母；候选漏检计入相应样本和召回分母。报告还列出模型 `NONE`、无法判断、证据不足、低分、候选漏检的对数，以及场景、中文与中英混合文本各自的分母和结果。服务 confidence 是 Choice 返回值，不是实测准确率。

发布目标沿用 #11 与 #32：准确率至少 90%，召回率至少 60%，有效关系全部通过机械证据检查，并覆盖全部有可判定正例的核心场景。任何一项未达标须在原实现路径修复并重新运行固定样本，不能修改人工标签或降低门槛。即使通过，结论也仅限本次冻结样本和所记录模型版本，不能外推为全部实际项目的准确率。

人工发布前记录可按 [报告模板](release-report-template.md) 填写，并保留三份机器文件与冻结输入。
