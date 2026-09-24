# #33：macOS V1 发布准备与受控验收

日期：2026-09-24。基线：`3f3471a54a4ada2d05ec52602a34abe26b31e775`（`origin/master`）。本记录只覆盖与 #32 真实 Jev 关系质量可分离的发布准备。#33 与父规格 #11 仍须保持开放。

## 现行验收口径与历史证据边界

后续需求 [#35](https://github.com/jinghonh/CodexFlow/issues/35) 已于 `3904e73e45078c512fece98fb9d2ff6c39afadde` 合入并关闭。**现行总结与工作流命名使用用户配置的同一组 OpenAI 兼容文本服务；Jev 仍只负责关系判断与证据选择。**下文在 #35 之前取得的 Codex 临时总结取消结果只保留为当时实现的历史观察，不计作现行文本服务的设置、推理、取消或质量通过。旧段落中的“Codex 总结/命名”均按此时间边界阅读；最终验收以本节、下方更新后的场景矩阵和“#35 合入后的签名包检查与待执行桌面路径”为准。

当前 #31 草案有 108 对、216 个可定位证据指针，**人工确认仍为 0/108**，尚未冻结；#32 未运行真实固定版本 Jev 质量评测。下文的受控夹具与自动化断言不提供真实准确率或召回率，#33 与 #11 不可关闭。

## 构建与本机安装

本机为 `Mac16,12`、Apple M4、16 GiB、macOS 27.0（26A428）、`arm64`；构建工具为 Node.js `v24.19.0`、npm `11.17.0`、Rust/Cargo `1.88.0`、Xcode `27.0`。在上述基线上执行 `npm ci`、`npm run tauri -- build`，生成 `target/release/bundle/macos/CodexFlow.app`。包内 `CFBundleIdentifier=dev.codexflow.desktop`、`CFBundleShortVersionString=0.1.0`，可执行文件为 `Mach-O arm64`。包大小约 18 MiB。

Tauri 原始包仅有链接器临时签名，`codesign --verify` 曾报告资源未绑定。新增 `npm run build:macos:local`，完成 Tauri 构建后调用 `scripts/sign_macos_local.sh` 对整个包临时签名并验证。该签名用于本机安装复核，**不代表开发者证书签名或公证**；未验证外部下载渠道的 Gatekeeper 行为。将签名后的整个 `.app` 以 `ditto` 复制到 `/tmp/codexflow-issue33-install/CodexFlow.app`，复制后的 `codesign --verify` 通过，包版本和架构一致。最终正式包及复制件中 `Contents/MacOS/codexflow-desktop` 的 SHA-256 均为 `62d053020e7587ee9993029201511d93cd0c5475c77a23825e45935332cee503`。

为避免读写用户原有应用数据，另以相同源码构建 `dev.codexflow.acceptance.issue33` / `CodexFlow V1 Acceptance.app`，临时签名后复制到 `/tmp/codexflow-issue33-install/` 并从该位置实际启动。启动时传入独立 `CODEX_HOME=/tmp/codexflow-issue33-codex-home`；桌面缓存仅落在 `~/Library/Application Support/dev.codexflow.acceptance.issue33/`，WebKit 和缓存目录同样按该标识隔离。正式标识的包只做复制和签名验证，未在用户现有数据目录启动。

复现本机默认包：

```sh
npm ci
npm run build:macos:local
codesign --verify --verbose=2 target/release/bundle/macos/CodexFlow.app
```

如需在隔离数据目录做桌面操作，使用独立标识构建，然后以独立 `CODEX_HOME` 启动；不要对真实历史执行写入操作：

```sh
npm run tauri -- build --config '{"identifier":"dev.codexflow.acceptance.issue33","productName":"CodexFlow V1 Acceptance"}'
bash scripts/sign_macos_local.sh 'target/release/bundle/macos/CodexFlow V1 Acceptance.app'
mkdir -p /tmp/codexflow-issue33-codex-home
open -n --env CODEX_HOME=/tmp/codexflow-issue33-codex-home -a 'target/release/bundle/macos/CodexFlow V1 Acceptance.app'
```

## 已实际通过的本机路径

| 路径 | 可核对结果与边界 |
| --- | --- |
| 正式构建与安装复制 | 正式 `0.1.0`、`arm64` 应用包生成，整体临时签名和复制后的完整性验证通过；隔离标识的同代码包实际启动并进入桌面首页。 |
| 独立真实 Codex 来源诊断 | 实际 CLI `codex-cli 0.150.1` 在独立 `CODEX_HOME` 下完成 app-server 初始化、未归档与归档列表请求；列表为 0 条，未读取用户原有会话。界面显示实际解析路径与版本。 |
| 真实 app-server 协议读取 | 独立目录中受控创建会话并发起一条合成回合后，同一进程的 `thread/turns/list` 返回 1 回合，`thread/read(includeTurns=true)` 返回 1 回合；`thread/items/list` 报“not supported yet”，因此没有把完整分页条目读取记为通过。无完整终态的受控会话在进程重启后没有出现在普通列表。 |
| #35 前旧路径：真实 Codex 临时回合取消 | 用隔离应用与合成会话材料手动触发临时总结，UI 记录实际模型 `gpt-5.6-sol`，取消时先显示“取消中”，再显示“已取消，旧总结保留”；隔离数据库中运行状态为 `cancelled`，有临时会话 ID，未保存新总结。另一次未手动取消的尝试也以 `cancelled` 结束，未得到有效总结。随后真实 app-server 的普通未归档与归档列表均为 0，未见临时会话进入普通历史；这仍不能作为现行文本服务成功或真实总结质量通过。 |
| macOS 钥匙串生命周期 | 隔离桌面包用本机 HTTP 夹具和仅供测试的合成字符串执行保存、轮换、重启读取与删除。钥匙串中相应项目由无到有、删除后再次为无；轮换后旧凭据的本机服务返回 `JEV_AUTHENTICATION_FAILED`，界面错误不含密钥。删除后连接和推理按钮禁用。 |
| 重启与恢复 | 隔离包重启后选定项目、缓存、工作流人工名称与成员调整仍在。将该隔离数据库的 `PRAGMA user_version` 从 15 临时设为 16 后启动，UI 显示 `DATABASE_TOO_NEW` 和安全恢复提示；失败打开后版本仍为 16。恢复备份的版本 15 数据库后，项目和人工修正再次可见。 |
| 来源不可用 | 把所选二进制改为不存在的隔离路径后，UI 显示 `BINARY_UNAVAILABLE` 与“缓存可能过期”，原有 4 条夹具会话仍可浏览；改回夹具后二次索引恢复完整。 |

受控钥匙串测试后，检查了隔离应用数据、WebKit 和缓存目录：合成测试密钥 A/B 均未以明文出现。此检查覆盖这些目录与公开 UI 错误，不等于对系统全部日志或所有拒绝访问路径的穷尽证明。先前实测没有执行钥匙串锁定或拒绝访问步骤；这些恢复路径仍未做桌面实测，相关失败行为只有可注入凭据存储的核心测试验证。先前保存、轮换和删除操作是否影响该登录用户的既有凭据，现有记录无法证实。

原验收记录证明使用了独立应用标识、应用数据目录和 `CODEX_HOME`，**没有记录使用独立 macOS 测试用户**。因此无法证实上述钥匙串生命周期实测与该登录用户既有凭据完全隔离，也不能仅凭合成密钥的“由无到有、删除后为无”推断没有既有凭据风险。后续独立标识包显示固定服务名下已有凭据状态，见下文“隔离边界补充”；钥匙串隔离的证据缺口保留。

## 仅使用固定或合成夹具验证的桌面流程

独立桌面包选择临时 Git 项目 `/tmp/codexflow-issue33-fixture/project`，另建其 worktree `/tmp/codexflow-issue33-fixture/worktree`。本机复制并改写仓库的 `fake_codex.py`，只把夹具工作目录指向上述临时路径并组合已有的列表与历史响应；原仓库夹具未改变。此流程不代表真实用户历史兼容性。

- 刷新时读取 3 页、6 条列表记录，合并为 4 条会话；可见已归档、`subAgent / review`、父会话及派生会话。主工作区和 worktree 的归属依据都指向共享 Git 目录。
- 点击受控会话后，界面显示分页路径的 2 回合、2 条目、完整性与来源时间；时间线得到两个可定位活动段，工作流泳道中每条会话只显示一次。关系图完成布局，显示 4 个节点与 `FORKED_FROM`、`SUBAGENT_OF` 两条观察关系。
- 将工作流改名为“受控验收工作流”，把未分组会话移入该工作流，界面显示 4 名成员；重启和再次刷新后名称、人工归属均保留。
- 本机 Jev HTTP 夹具收到 `GET /v1/models` 和仅含固定合成材料的 `POST /v1/systemone`；桌面显示模型列表、合成判定、实际模型 `jev-fixture-1.0.0` 与用量。延迟 POST 发起后点击取消，界面立即显示本地取消；夹具稍后返回时，界面未接受迟到推理结果。这不代表真实 Jev 连接、版本或质量通过。

`npm run test:ui` 的 44 项全部通过。`cargo test --locked -p codexflow-codex -p codexflow-core -p codexflow-store -p codexflow-jev` 全部通过，覆盖适配器、核心与存储的相关成功和错误路径；其中 Codex/Jev 的自动化模型响应均为受控夹具。上述桌面操作和自动测试共同提供分离的证据，不能把夹具结论写成真实后端成功。

## 仍未验证、不可关闭 #33 的项目

1. 现行文本服务尚未在独立 macOS 测试用户下完成签名桌面包的设置、合成推理、总结、命名与取消连续实测。此前隔离 `CODEX_HOME` 的 `codex login status` 为 `Not logged in`，Codex 临时总结只有取消终态；这些旧结果既不阻止现行文本服务，也不能充当它的成功证据。
2. 真实 Jev 的 Bearer 验证、模型列表、合成推理、固定实际模型版本、真实 HTTP 取消与关系质量均未验收。#31 人工确认仍为 0/108，#32 尚无冻结样本的真实评测；本记录的本机 HTTP 夹具没有质量意义。不能用固定判断替身代替真实 Jev 质量结果。
3. 尚未完成从受控真实 Codex 历史到文本服务总结/命名、Jev 关系与证据、拒绝、重算、工作流整理的签名桌面包连续流程。关系裁决、过期证据、预算暂停继续、协议不兼容、认证/额度故障、部分历史、保存冲突及迁移中途故障已有对应自动化边界测试，尚不能升级为正式包完整实测通过。实际钥匙串拒绝访问与真实来源的完整分页条目读取也未完成。
4. 22 个父规格核心场景的自动化覆盖不等于最终逐项桌面验收；尤其第 19–22 项依赖两类服务配置、真实 Jev、钥匙串拒绝访问或双后端连续运行。维护者应以 #11 的原始编号和 #35 覆盖后的现行行为逐项补齐实际证据，不得用本轮合成结果勾选完整通过。

## 父规格 22 项核心场景证据矩阵

下表编号对应 [#11 的“至少覆盖以下验收场景”](https://github.com/jinghonh/CodexFlow/issues/11)。状态只说明截至本记录可追溯的证据强度；“受控桌面”使用隔离数据与合成来源，“自动化”使用测试替身。二者都不能替代真实后端质量和最终包逐项验收。源码测试名用于定位具体断言，不表示单凭测试名就已完成桌面实测。

| # | 核心场景 | 当前证据 | 尚需补齐 |
| --- | --- | --- | --- |
| 1 | 已归档与子代理完整列表不重复 | 受控桌面读取 3 页、6 条并合并为 4 条；`complete_and_partial_lists_keep_one_cached_thread_per_id`（[核心测试](../../crates/core/src/lib.rs)）。 | 最终包在受控真实历史上逐项复核。 |
| 2 | 分页中途失败不误删缓存 | `paged_refresh_can_be_cancelled_and_resumed_without_losing_cache_or_project`、`failed_thread_history_does_not_block_another_thread_or_erase_metadata`（[核心测试](../../crates/core/src/lib.rs)）。 | 桌面故障注入与恢复连续路径。 |
| 3 | 隔日恢复保留时间空档 | 受控桌面显示两个可定位活动段；`跨日恢复保留空档`（[时间线测试](../../src/ProjectTimelineView.test.tsx)）。 | 用最终包复核真实来源时间。 |
| 4 | 缺失时间只显示已知活动 | `跨日恢复保留空档，未知与无位置回合可检查`（[时间线测试](../../src/ProjectTimelineView.test.tsx)）；[核心来源时间测试](../../crates/codex/src/history.rs)覆盖无效时间。 | 桌面缺时来源实测。 |
| 5 | `NONE`、无效端点与证据不入图 | `inferred_outcomes_exclude_none_unknown_bad_evidence_and_conflicting_time`（[批次测试](../../crates/core/src/analysis_batch.rs)）及[推断关系测试](../../crates/core/src/inferred.rs)。 | 真实 Jev 输出与最终图复核，依赖 #32。 |
| 6 | 拒绝关系后重算仍拒绝 | `确认、拒绝和显式恢复立即更新图边`（[图测试](../../src/ProjectGraphView.test.tsx)）；[存储裁决并发测试](../../crates/store/src/lib.rs)。 | 双后端桌面重算连续路径。 |
| 7 | 恢复会话的后续工作不只按创建时间否决 | [候选生成](../../crates/core/src/candidates.rs)用最近活动时间，[推断验证](../../crates/core/src/inferred.rs)用证据回合时间；尚无该完整场景的直接证据。 | 构造较早创建且隔日恢复的两条会话，验证候选、因果边及真实 Jev 结果；依赖 #32。 |
| 8 | 分析失败保留事实和旧结果 | `partial_new_source_excludes_candidates_and_keeps_old_facts`（[批次测试](../../crates/core/src/analysis_batch.rs)）；`summary_uses_text_service_cache_and_keeps_old_result_on_failure`（[文本设置测试](../../crates/core/src/text_settings.rs)）；受控桌面来源不可用仍可浏览 4 条缓存会话。 | 签名包中分别注入文本服务和 Jev 失败，连续检查旧结果与事实。 |
| 9 | 自身分析不进入普通索引 | 旧 Codex 临时回合中断后普通列表为 0，仅是历史证据；现行[文本适配器](../../crates/text/src/lib.rs)以 HTTP 发送生成请求，不启动 Codex 会话。 | 在独立用户的签名包中完成文本总结后重启和刷新，核对普通 Codex 会话数与身份不变。 |
| 10 | worktree 合并，独立克隆与嵌套仓库分离 | 受控桌面项目/worktree 归属；`real_repositories_worktrees_clones_nested_repos_and_missing_paths`（[项目测试](../../crates/core/src/projects.rs)）。 | 最终包复核独立克隆和嵌套仓库。 |
| 11 | 每条会话单泳道，跨工作流关系可见 | 受控桌面 4 条会话各一次；`工作流泳道按主要归属显示`（[时间线测试](../../src/ProjectTimelineView.test.tsx)）。 | 最终包跨工作流推断关系复核，依赖 #32。 |
| 12 | 改名、移成员及解除修正 | 受控桌面改名、移成员、重启及刷新保留；`workstream_corrections_survive_regrouping_restart_and_restore_independently`（[核心测试](../../crates/core/src/lib.rs)）及 `manual_name_is_not_sent_to_text_model_or_overwritten`（[批次测试](../../crates/core/src/analysis_batch.rs)）。 | 签名包中显式恢复自动状态，并在文本服务命名与 Jev 重算后复核人工修正。 |
| 13 | 预算暂停、继续、取消迟到结果 | `text_summaries_and_name_share_budget_after_members_are_fixed`、`call_limit_pauses_and_continuation_only_handles_remaining_units`、`in_flight_jev_result_is_rejected_after_cancel_or_config_change`（[批次测试](../../crates/core/src/analysis_batch.rs)）；文本总结迟到响应见[文本设置测试](../../crates/core/src/text_settings.rs)。 | 签名包中用文本服务和 Jev 连续运行、暂停、继续及取消，核对两类请求和重试共用预算。 |
| 14 | 文本服务与 Jev 分别确认本地取消（#35 覆盖原 Codex 回合路径） | 旧 Codex `turn/interrupt` 终态仅是历史证据；本机 Jev HTTP 夹具拒绝迟到响应；`late_summary_response_is_rejected_after_cancel_or_source_change`（[文本设置测试](../../crates/core/src/text_settings.rs)）。 | 独立用户签名包中分别取消文本合成验证、总结、命名和 Jev HTTP 请求，核对本地终态与迟到响应；命名请求单独的迟到响应案例仍待补。 |
| 15 | 重启保留缓存、裁决和人工修正，运行不误完成 | 受控桌面重启保留缓存与工作流修正；`reopening_marks_unfinished_run_interrupted_without_erasing_cache`、`unfinished_summary_run_is_recovered_after_restart`（[存储测试](../../crates/store/src/lib.rs)）。 | 桌面裁决和中断批次重启连续路径。 |
| 16 | 迁移失败与旧版本打开新格式不破坏数据 | 受控桌面未来版本 16 显示 `DATABASE_TOO_NEW`，恢复版本 15 备份后数据可见；`failed_later_migration_rolls_back_earlier_schema_steps`（[存储测试](../../crates/store/src/lib.rs)）。 | 最终包迁移中途故障注入。 |
| 17 | 无向反向去重，端点语义统一 | [推断关系测试](../../crates/core/src/inferred.rs)与[工作流规则测试](../../crates/core/src/workstreams.rs)覆盖去重及方向。 | 桌面反向输入及结构/因果边核对。 |
| 18 | 过滤与切换不改变身份及人工数据 | `通过公开应用查询联动时间线、关系图和详情，过滤后保留选择`（[探索器测试](../../src/ProjectExplorerFlow.test.tsx)）；受控桌面刷新后人工工作流修正仍在。 | 最终包多视图切换后裁决与成员核对。 |
| 19 | 配置故障可修复且两类服务验证独立 | 旧桌面 Jev HTTP 夹具完成模型列表与固定推理；`validation_sends_only_fixed_synthetic_material`（[文本设置测试](../../crates/core/src/text_settings.rs)）覆盖文本服务的独立合成验证。 | 独立 macOS 用户的签名包中分别复核文本服务推理与 Jev 模型列表/推理、配置错误修复、钥匙串拒绝访问，以及文本总结仍可单独运行；真实 Jev 依赖 #32。 |
| 20 | 两类凭据生命周期与服务地址隔离、无泄露 | 旧桌面 Jev 合成密钥生命周期和限定目录检查的用户隔离无法证实；`settings_keep_secrets_out_of_database_and_bind_key_to_address`（[文本设置测试](../../crates/core/src/text_settings.rs)）及 Jev [核心测试](../../crates/core/src/lib.rs)使用可注入凭据存储。 | 独立 macOS 测试用户中分别复核文本/Jev 钥匙串保存、轮换、重启、删除、拒绝访问和跨地址不转发旧密钥；现有用户不得操作既有条目。 |
| 21 | Jev 无关系/不确定/低分/证据不足与中英材料 | `inferred_outcomes_exclude_none_unknown_bad_evidence_and_conflicting_time`、`native_jev_two_post_path_persists_a_locatable_inferred_edge`（[批次测试](../../crates/core/src/analysis_batch.rs)）只覆盖状态与证据约束。 | 中文及中英混合技术文本仍待 #31 纳入冻结样本，并由 #32 用真实固定版本 Jev 评测；无法由夹具替代。 |
| 22 | 文本服务与 Jev 共享预算、别名/设置变更及旧结果隔离 | `text_summaries_and_name_share_budget_after_members_are_fixed`、`alias_probe_and_retry_each_consume_the_batch_call_limit`、`changed_jev_settings_cannot_be_mixed_into_a_frozen_run`、`jev_cache_requires_matching_nonsecret_config_rules_and_pinned_actual_model`（[批次测试](../../crates/core/src/analysis_batch.rs)）；文本总结缓存见[文本设置测试](../../crates/core/src/text_settings.rs)。 | 签名包中验证两类服务设置变化、密钥轮换、Jev 实际版本、缓存复用和迟到结果隔离；真实 Jev 依赖 #32。 |

以上 22 项都有可定位的现有证据或明确缺口；**没有一项因表中列出自动化测试而自动升级为完整 V1 通过**。最终验收应在每行补上包版本、隔离数据位置、实际操作与可复核结果，再判断是否通过。

## 2026-09-24 续作：隔离桌面连续路径

本次基于 `bb890ca89b55bc0259584ca9fb4e58e2db9b1228` 构建另一个 `0.1.0`、`arm64` 包，标识 `dev.codexflow.acceptance.issue33.continuation`，应用名 `CodexFlow V1 Continuation.app`。`npm ci` 与带该标识的 `npm run tauri -- build --config ...` 成功；脚本对整个包作本机临时签名，将完整包复制到 `/tmp/codexflow-issue33-continuation-install/` 后 `codesign --verify --verbose=2` 成功。复制件可执行文件 SHA-256 为 `c4fc7386544d67fcbac0c213393c4cebc0b66a35a551cc6ab5cfe953b7b2d341`。这是本机包验证，签名与 Gatekeeper 边界同前述记录。

可重建的受控来源由 [`scripts/prepare_issue33_fixture.py`](../../scripts/prepare_issue33_fixture.py)从仓库的 `fake_codex.py` 派生；只改写临时项目工作目录，并为 `list-rich` 模式启用已有的分页历史响应。它新建空 Git 仓库、一个 worktree、合成二进制和空 `CODEX_HOME`。脚本的 Git 子进程使用受控环境、空模板与钩子目录，屏蔽继承的 `GIT_*` 重定向和用户 Git 配置。本次运行命令为：

修复后的定向隔离检查只使用一次性临时目录：向脚本进程注入指向夹具外临时仓库的 `GIT_DIR`、`GIT_WORK_TREE`、`GIT_COMMON_DIR`、索引和对象目录变量，同时注入自定义全局/系统配置、模板和提交/检出钩子。脚本完成后，外部临时仓库的提交与工作区状态未变，钩子标记未生成；新项目和 worktree 的根目录都在新夹具内。检查未接触真实用户仓库。

```sh
python3 scripts/prepare_issue33_fixture.py /tmp/codexflow-issue33-continuation-fixture
open -n --env CODEX_HOME=/private/tmp/codexflow-issue33-continuation-fixture/codex-home \
  -a '/tmp/codexflow-issue33-continuation-install/CodexFlow V1 Continuation.app'
```

应用数据使用独立标识对应的 `~/Library/Application Support/dev.codexflow.acceptance.issue33.continuation/`。在新启动的包内，先将 Codex 二进制设为 `/private/tmp/codexflow-issue33-continuation-fixture/fake-list-rich`，再选择该夹具的 `project`。桌面连续操作得到如下可观察结果：

1. 来源诊断显示合成二进制与分页能力；项目列表读取 3 页、6 条并合并为 4 条。已归档的 `thread-a`、`thread-d`，`subAgent / review` 的 `thread-b` 和派生 `thread-c` 均可见；主工作区与 worktree 的归属都指向同一共享 Git 目录。
2. 打开 `thread-a` 后，历史显示“读取方式：分页；已取得 2 回合、2 条目”，内容完整；时间线出现两个来源回合段，四条会话各只占一条泳道。切换关系图后布局完成，显示 4 个节点与 `FORKED_FROM`、`SUBAGENT_OF` 两条观察关系。
3. 人工改名为“续作受控验收工作流”，将未分组 `thread-d` 移入；界面显示 4 名成员和“未分组会话 · 0”。关闭并重新打开同一隔离包后，项目缓存、该名称、四名成员与人工修正操作入口仍在，列表再次刷新仍为 3 页、6 条记录合并成 4 条会话。
4. 把二进制改成不存在的夹具目录路径后，桌面显示 `BINARY_UNAVAILABLE`、可重试与“已有缓存保留”；四条会话和人工工作流仍可见，列表提示“缓存可能过期”。改回合成二进制后诊断重新连接，自动刷新恢复完整列表。此故障只作用于本轮合成来源。

本次 `npm run test:ui` 在构建并行时曾出现 5 项超时和 2 项界面断言失败；构建结束后原样完整复跑，**9 个测试文件、44 项全部通过**，未修改产品代码。该首次失败与并行构建资源竞争相符，但未进行系统级性能归因，不计为产品缺陷修复。

**隔离边界补充：**独立应用标识和空 `CODEX_HOME` 隔离了桌面缓存与 Codex 来源，却没有隔离 Jev 的系统钥匙串项目。`KeychainCredentialStore` 使用固定 `dev.codexflow.desktop.jev` 服务名；新标识的设置页仍显示默认地址的凭据“已保存”。本次续作没有读取、轮换、删除或发送该凭据，也没有点击 Jev 连接、推理或项目分析；这不能补足先前生命周期实测的用户隔离证据。若要在此包继续 Jev 凭据桌面实测，必须使用独立 macOS 测试用户或先实现经过迁移设计的凭据命名空间；仅换包标识与 `CODEX_HOME` 不够。该观察不证明凭据明文泄漏，但限制了本轮可安全执行的双后端路径。

## #35 合入后的签名包检查与待执行桌面路径

在当前 `master` 基线 `3904e73e45078c512fece98fb9d2ff6c39afadde` 上执行 `npm run build:macos:local`，新生成的 `target/release/bundle/macos/CodexFlow.app` 已通过整个包的 `codesign --verify --verbose=2`。包标识为 `dev.codexflow.desktop`，版本 `0.1.0`，可执行文件为 `Mach-O arm64`，SHA-256 为 `9997db0f4e4d3b4965c4f020e6742f927556d694f7506e69895c8ff0cc16398b`。这只证明当前源码可生成并验证本机临时签名包；**本轮没有启动该包，也没有完成新文本服务的签名包桌面冒烟**。先前两个隔离标识包均早于 #35，不能当作此包的文本服务证据。

本轮无需系统钥匙串或真实模型服务的定向检查：`cargo test --locked -p codexflow-text` 的 3 项、`cargo test --locked -p codexflow-core text` 的 10 项，以及 `npx vitest run src/ThreadSummaryView.test.tsx src/ProjectAnalysisView.test.tsx src/ProjectWorkstreamsView.test.tsx` 的 13 项均通过。它们使用合成 HTTP 响应或可注入凭据存储，覆盖 Chat Completions 地址与请求、脱敏错误、固定合成验证、文本设置与跨地址密钥约束、总结缓存和失败保留、总结与命名共享预算、命名失败保留分组、人工名称不被覆盖、总结取消或来源变化后的迟到响应，以及相应界面状态。这些自动化结果没有访问当前登录用户的钥匙串条目，也不等于签名包的桌面流程。

以下步骤是**待在全新独立 macOS 测试用户执行**的签名包验收记录模板。独立应用标识、`CODEX_HOME` 或不同 Base URL 都不能替代独立用户：文本与 Jev 的系统钥匙串服务名分别固定为 `dev.codexflow.desktop.text`、`dev.codexflow.desktop.jev`。本轮在当前用户没有保存、替换、删除或读出既有明文钥匙串项，也没有触发真实付费服务。

1. **建立隔离和记录包身份。**在独立 macOS 测试用户下，将上述完整签名 `.app` 复制到仅供测试的位置并核对签名、版本、架构与哈希。选择一个尚不存在的临时路径运行 `python3 scripts/prepare_issue33_fixture.py <临时路径>`，以其 `codex-home` 作为 `CODEX_HOME` 启动测试包，选择其 `project` 和 `fake-list-rich`。记录测试用户名、包哈希、应用数据路径、来源二进制和操作时间。该夹具只含合成会话，适合来源、分页、结构图、工作流和文本总结入口检查；它没有足够的双侧事实证据来驱动 Jev 候选批次，关系阶段另需两条有可定位双方条目的受控会话。
2. **分别配置两类回环服务。**测试用户新建的文本服务合成凭据仅指向 `127.0.0.1` 的 OpenAI 兼容服务根地址，按所填网关前缀及 `/v1` 规则接收 `POST .../chat/completions`。其固定验证响应应包含 `choices[0].message.content` 为 JSON 字符串 `{"ok":true}`，并给出响应实际模型 ID；核对请求只含固定合成材料，不含项目历史，记录服务端收到请求的路径、模型和认证头是否存在，**不要记录密钥值**。文本服务没有独立的模型列表连接按钮，“验证固定合成推理”同时检查连通性与协议。Jev 另用回环服务响应 `GET /v1/models` 与固定合成 `POST /v1/systemone`；两个按钮分别执行，模型列表成功不能代替推理成功。先用本机夹具完成这些操作，再另列真实服务验收，不把回环结果记作真实模型质量。
3. **检查总结和命名。**打开已完整读取的合成会话，确认预览显示文本服务、模型、最多 40,000 字符的覆盖范围，再手动生成总结。受控文本服务按[总结输入与验证](../../crates/core/src/summary.rs)返回五项内容及可定位的 `evidenceIds`，例如 `{"goal":"合成目标","activity":"执行检查","outcome":"完成","decisions":"保留方案","issues":"无","evidenceIds":["item:turn-1:item-1"]}`；核对请求模型、响应实际模型、来源证据和缓存状态，刷新 Codex 列表确认没有新增分析会话。项目预览应分别列出文本总结、Jev 关系与证据选择、文本命名的发送范围和调用上限。工作流成员先由关系形成，再按[命名材料与验证](../../crates/core/src/analysis_batch.rs)返回例如 `{"name":"合成工作流"}`；验证失败时仍可见确定性暂用名。旧 Codex 临时回合取消记录不参与此步骤的判定。
4. **检查共享预算、取消和迟到响应。**使用可定位双方证据的受控候选和可延迟响应的两个回环服务，设置小于预计请求数的总调用上限；手动启动、等待暂停、提高上限后继续，核对已保存单元不重复调用，文本总结/命名与 Jev 分类/证据选择及重试共同计数。分别在文本合成验证、文本总结、文本命名、Jev HTTP 请求在途时取消，并让夹具随后返回；记录 UI 本地终态、请求计数和数据库结果未被迟到响应覆盖。Jev 与文本 HTTP 的本地取消不承诺远端停算或停止计费。文本**命名**请求独立的迟到响应取消案例在 #35 集成记录中仍列为关注点，不能以总结取消测试代替。
5. **检查缓存、人工修正和恢复。**相同输入与服务/模型设置下重试，应复用有效总结和名称；仅轮换测试密钥不应触发重算。更改文本地址或模型后，只让相关自动总结和名称待更新；更改 Jev 服务、模型实际版本或规则后，让相关推断结果待更新，不能让旧运行迟到结果覆盖新设置。人工改名、移动成员、拒绝关系后再分析，确认修正持久；显式恢复自动状态后才解除相应修正。分别注入认证、额度、协议、来源不可用、重启和迁移错误，记录事实浏览及旧有效结果是否保留。真实 Jev 固定版本的关系质量须等待 #31 冻结样本与 #32 报告，不能由这些合成步骤替代。

每一步应保存不含密钥的包身份、隔离路径、操作序列、请求计数、UI 状态和结果定位，再更新上方 22 项矩阵。当前只有构建、签名与定向自动化检查可记为本轮通过；上述签名包桌面步骤、真实凭据流及真实 Jev 质量仍是待验收。

## 性能沿用 #30 的已通过与暂缓口径

本轮没有重新测量性能。沿用 [`issue-30-final-acceptance.md`](../performance/issue-30-final-acceptance.md) 的最后正式包结果：500 节点、2999 条关系的图 20 次桌面操作第 95 百分位 1.490 秒，低于 5 秒；缓存会话查询 20 次第 95 百分位 417 毫秒，低于 500 毫秒。正式包首次概览构建后首次样本 2.344 秒，超过 2 秒；归档过滤第 95 百分位 592 毫秒、证据回合首屏 1298 毫秒、证据条目与事实首屏 1861 毫秒，均未达到 500 毫秒。后三类及首次概览属于用户授权的本轮暂缓，不是性能全部通过。#30 已以 `NOT_PLANNED` 关闭；该口径不改变 #31、#32 和 #33 的真实性要求。

## 维护者下一步

在不复制明文密钥、不读取用户原有会话的前提下，准备独立 macOS 测试用户与受控来源，在当前签名包中执行文本服务和 Jev 各自的设置、合成验证、取消、双后端预算、证据裁决及恢复步骤；总结和命名按 #35 的文本服务路径验收。同时由维护者完成 #31 的 108 对逐项复核与冻结，再由 #32 使用真实 Jev 固定版本完成质量报告。补齐 22 项场景、两类钥匙串拒绝访问及必要的真实后端证据后，#33 才能作为关闭 #11 的依据。
