# #33：macOS V1 发布准备与受控验收

日期：2026-09-24。基线：`3f3471a54a4ada2d05ec52602a34abe26b31e775`（`origin/master`）。本记录只覆盖与 #32 真实 Jev 关系质量可分离的发布准备。#33 与父规格 #11 仍须保持开放。

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
| 真实 Codex 临时回合取消 | 用隔离应用与合成会话材料手动触发临时总结，UI 记录实际模型 `gpt-5.6-sol`，取消时先显示“取消中”，再显示“已取消，旧总结保留”；隔离数据库中运行状态为 `cancelled`，有临时会话 ID，未保存新总结。另一次未手动取消的尝试也以 `cancelled` 结束，未得到有效总结。随后真实 app-server 的普通未归档与归档列表均为 0，未见临时会话进入普通历史；这仍不能作为真实总结质量通过。 |
| macOS 钥匙串生命周期 | 隔离桌面包用本机 HTTP 夹具和仅供测试的合成字符串执行保存、轮换、重启读取与删除。钥匙串中相应项目由无到有、删除后再次为无；轮换后旧凭据的本机服务返回 `JEV_AUTHENTICATION_FAILED`，界面错误不含密钥。删除后连接和推理按钮禁用。 |
| 重启与恢复 | 隔离包重启后选定项目、缓存、工作流人工名称与成员调整仍在。将该隔离数据库的 `PRAGMA user_version` 从 15 临时设为 16 后启动，UI 显示 `DATABASE_TOO_NEW` 和安全恢复提示；失败打开后版本仍为 16。恢复备份的版本 15 数据库后，项目和人工修正再次可见。 |
| 来源不可用 | 把所选二进制改为不存在的隔离路径后，UI 显示 `BINARY_UNAVAILABLE` 与“缓存可能过期”，原有 4 条夹具会话仍可浏览；改回夹具后二次索引恢复完整。 |

受控钥匙串测试后，检查了隔离应用数据、WebKit 和缓存目录：合成测试密钥 A/B 均未以明文出现。此检查覆盖这些目录与公开 UI 错误，不等于对系统全部日志或所有拒绝访问路径的穷尽证明。未锁定或改变用户的登录钥匙串；macOS 实际拒绝访问/锁定恢复路径仍未做桌面实测，相关失败行为只有可注入凭据存储的核心测试验证。

## 仅使用固定或合成夹具验证的桌面流程

独立桌面包选择临时 Git 项目 `/tmp/codexflow-issue33-fixture/project`，另建其 worktree `/tmp/codexflow-issue33-fixture/worktree`。本机复制并改写仓库的 `fake_codex.py`，只把夹具工作目录指向上述临时路径并组合已有的列表与历史响应；原仓库夹具未改变。此流程不代表真实用户历史兼容性。

- 刷新时读取 3 页、6 条列表记录，合并为 4 条会话；可见已归档、`subAgent / review`、父会话及派生会话。主工作区和 worktree 的归属依据都指向共享 Git 目录。
- 点击受控会话后，界面显示分页路径的 2 回合、2 条目、完整性与来源时间；时间线得到两个可定位活动段，工作流泳道中每条会话只显示一次。关系图完成布局，显示 4 个节点与 `FORKED_FROM`、`SUBAGENT_OF` 两条观察关系。
- 将工作流改名为“受控验收工作流”，把未分组会话移入该工作流，界面显示 4 名成员；重启和再次刷新后名称、人工归属均保留。
- 本机 Jev HTTP 夹具收到 `GET /v1/models` 和仅含固定合成材料的 `POST /v1/systemone`；桌面显示模型列表、合成判定、实际模型 `jev-fixture-1.0.0` 与用量。延迟 POST 发起后点击取消，界面立即显示本地取消；夹具稍后返回时，界面未接受迟到推理结果。这不代表真实 Jev 连接、版本或质量通过。

`npm run test:ui` 的 44 项全部通过。`cargo test --locked -p codexflow-codex -p codexflow-core -p codexflow-store -p codexflow-jev` 全部通过，覆盖适配器、核心与存储的相关成功和错误路径；其中 Codex/Jev 的自动化模型响应均为受控夹具。上述桌面操作和自动测试共同提供分离的证据，不能把夹具结论写成真实后端成功。

## 仍未验证、不可关闭 #33 的项目

1. 独立 `CODEX_HOME` 的 `codex login status` 为 `Not logged in`；普通配置存在文件式 `auth.json`，而应用的隔离检查禁止该方式进入模型总结。实际临时总结没有成功终态，真实工作流命名及其取消终态也未完成。不能为本轮绕过只读、禁网络和钥匙串边界。
2. 真实 Jev 的 Bearer 验证、模型列表、合成推理、固定实际模型版本、真实 HTTP 取消与关系质量均未验收。#32 仍受 #31 人工逐对复核和冻结样本阻塞；本记录的本机 HTTP 夹具没有质量意义。不能用无密钥环境或固定判断替身代替真实 Jev 质量结果。
3. 本轮没有完成从真实 Codex 历史到双后端分析、推断关系双侧证据、拒绝、重算、工作流整理的完整桌面连续流程。关系裁决、过期证据、预算暂停继续、协议不兼容、认证/额度故障、部分历史、保存冲突及迁移中途故障已有对应自动化边界测试，尚不能升级为正式包完整实测通过。实际钥匙串拒绝访问与真实来源的完整分页条目读取也未完成。
4. 22 个父规格核心场景的自动化覆盖不等于最终逐项桌面验收；尤其第 19–22 项依赖真实 Jev、钥匙串拒绝访问或双后端连续运行。维护者应以 #11 的原始编号逐项补齐实际证据，不得用本轮合成结果勾选完整通过。

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
| 8 | 分析失败保留事实和旧结果 | `partial_new_source_excludes_candidates_and_keeps_old_facts`（[批次测试](../../crates/core/src/analysis_batch.rs)）；受控桌面来源不可用仍可浏览 4 条缓存会话。 | 桌面模型失败后旧结果及事实连续检查。 |
| 9 | 自身临时分析不进入普通索引 | 独立真实 app-server 中断后普通列表仍为 0；[Codex 适配器测试](../../crates/codex/src/lib.rs)覆盖临时会话排除。 | 真实成功终态总结及重启后再索引，依赖独立钥匙串认证。 |
| 10 | worktree 合并，独立克隆与嵌套仓库分离 | 受控桌面项目/worktree 归属；`real_repositories_worktrees_clones_nested_repos_and_missing_paths`（[项目测试](../../crates/core/src/projects.rs)）。 | 最终包复核独立克隆和嵌套仓库。 |
| 11 | 每条会话单泳道，跨工作流关系可见 | 受控桌面 4 条会话各一次；`工作流泳道按主要归属显示`（[时间线测试](../../src/ProjectTimelineView.test.tsx)）。 | 最终包跨工作流推断关系复核，依赖 #32。 |
| 12 | 改名、移成员及解除修正 | 受控桌面改名、移成员、重启及刷新保留；`workstream_corrections_survive_regrouping_restart_and_restore_independently`（[核心测试](../../crates/core/src/lib.rs)）。 | 桌面显式恢复自动状态与双后端重算。 |
| 13 | 预算暂停、继续、取消迟到结果 | `call_limit_pauses_and_continuation_only_handles_remaining_units`、`in_flight_jev_result_is_rejected_after_cancel_or_config_change`（[批次测试](../../crates/core/src/analysis_batch.rs)）；[分析视图测试](../../src/ProjectAnalysisView.test.tsx)。 | 真正双后端桌面连续运行，依赖受控认证和 #32。 |
| 14 | Codex 与 Jev 分别确认取消终态 | 真实 Codex 临时回合从“取消中”到“已取消”；本机 Jev HTTP 夹具迟到结果被拒；`cancellation_waits_for_codex_terminal_and_can_continue`（[批次测试](../../crates/core/src/analysis_batch.rs)）。 | 真实 Jev HTTP 取消以及成功分析后的两侧桌面取消。 |
| 15 | 重启保留缓存、裁决和人工修正，运行不误完成 | 受控桌面重启保留缓存与工作流修正；`reopening_marks_unfinished_run_interrupted_without_erasing_cache`、`unfinished_summary_run_is_recovered_after_restart`（[存储测试](../../crates/store/src/lib.rs)）。 | 桌面裁决和中断批次重启连续路径。 |
| 16 | 迁移失败与旧版本打开新格式不破坏数据 | 受控桌面未来版本 16 显示 `DATABASE_TOO_NEW`，恢复版本 15 备份后数据可见；`failed_later_migration_rolls_back_earlier_schema_steps`（[存储测试](../../crates/store/src/lib.rs)）。 | 最终包迁移中途故障注入。 |
| 17 | 无向反向去重，端点语义统一 | [推断关系测试](../../crates/core/src/inferred.rs)与[工作流规则测试](../../crates/core/src/workstreams.rs)覆盖去重及方向。 | 桌面反向输入及结构/因果边核对。 |
| 18 | 过滤与切换不改变身份及人工数据 | `通过公开应用查询联动时间线、关系图和详情，过滤后保留选择`（[探索器测试](../../src/ProjectExplorerFlow.test.tsx)）；受控桌面刷新后人工工作流修正仍在。 | 最终包多视图切换后裁决与成员核对。 |
| 19 | 配置故障可修复且两种测试独立 | 本机 HTTP 夹具完成模型列表与固定推理；受控钥匙串保存/轮换/删除；`locked_keychain_keeps_saved_nonsecret_config_visible`（[核心测试](../../crates/core/src/lib.rs)）。 | 真实 Jev 错误与修复、钥匙串实际拒绝访问、独立 Codex 总结成功；依赖 #32 与受控认证。 |
| 20 | 凭据生命周期与服务地址隔离、无泄露 | 受控桌面合成密钥保存、轮换、重启、删除和限定目录明文检查；`jev_credentials_are_separate_from_persisted_settings_and_scoped_to_address`（[核心测试](../../crates/core/src/lib.rs)）。 | 真实钥匙串拒绝访问；真实服务地址切换不发送旧密钥的桌面核对。 |
| 21 | Jev 无关系/不确定/低分/证据不足与中英材料 | `inferred_outcomes_exclude_none_unknown_bad_evidence_and_conflicting_time`、`native_jev_two_post_path_persists_a_locatable_inferred_edge`（[批次测试](../../crates/core/src/analysis_batch.rs)）。 | #31 冻结的人工样本与 #32 真实固定版本质量报告；无法由夹具替代。 |
| 22 | 双后端共享预算、别名/设置变更及旧结果隔离 | `alias_probe_and_retry_each_consume_the_batch_call_limit`、`changed_jev_settings_cannot_be_mixed_into_a_frozen_run`、`jev_cache_requires_matching_nonsecret_config_rules_and_pinned_actual_model`（[批次测试](../../crates/core/src/analysis_batch.rs)）。 | 最终包双后端连续路径及真实 Jev 版本确认，依赖 #32 与受控认证。 |

以上 22 项都有可定位的现有证据或明确缺口；**没有一项因表中列出自动化测试而自动升级为完整 V1 通过**。最终验收应在每行补上包版本、隔离数据位置、实际操作与可复核结果，再判断是否通过。

## 2026-09-24 续作：隔离桌面连续路径

本次基于 `bb890ca89b55bc0259584ca9fb4e58e2db9b1228` 构建另一个 `0.1.0`、`arm64` 包，标识 `dev.codexflow.acceptance.issue33.continuation`，应用名 `CodexFlow V1 Continuation.app`。`npm ci` 与带该标识的 `npm run tauri -- build --config ...` 成功；脚本对整个包作本机临时签名，将完整包复制到 `/tmp/codexflow-issue33-continuation-install/` 后 `codesign --verify --verbose=2` 成功。复制件可执行文件 SHA-256 为 `c4fc7386544d67fcbac0c213393c4cebc0b66a35a551cc6ab5cfe953b7b2d341`。这是本机包验证，签名与 Gatekeeper 边界同前述记录。

可重建的受控来源由 [`scripts/prepare_issue33_fixture.py`](../../scripts/prepare_issue33_fixture.py)从仓库的 `fake_codex.py` 派生；只改写临时项目工作目录，并为 `list-rich` 模式启用已有的分页历史响应。它新建空 Git 仓库、一个 worktree、合成二进制和空 `CODEX_HOME`。本次运行命令为：

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

**隔离边界补充：**独立应用标识和空 `CODEX_HOME` 隔离了桌面缓存与 Codex 来源，却没有隔离 Jev 的系统钥匙串项目。`KeychainCredentialStore` 使用固定 `dev.codexflow.desktop.jev` 服务名；新标识的设置页仍显示默认地址的凭据“已保存”。本次没有读取、轮换、删除或发送该凭据，也没有点击 Jev 连接、推理或项目分析。若要在此包继续 Jev 凭据桌面实测，必须使用独立 macOS 测试用户或先实现经过迁移设计的凭据命名空间；仅换包标识与 `CODEX_HOME` 不够。该观察不证明凭据明文泄漏，但限制了本轮可安全执行的双后端路径。

## 性能沿用 #30 的已通过与暂缓口径

本轮没有重新测量性能。沿用 [`issue-30-final-acceptance.md`](../performance/issue-30-final-acceptance.md) 的最后正式包结果：500 节点、2999 条关系的图 20 次桌面操作第 95 百分位 1.490 秒，低于 5 秒；缓存会话查询 20 次第 95 百分位 417 毫秒，低于 500 毫秒。正式包首次概览构建后首次样本 2.344 秒，超过 2 秒；归档过滤第 95 百分位 592 毫秒、证据回合首屏 1298 毫秒、证据条目与事实首屏 1861 毫秒，均未达到 500 毫秒。后三类及首次概览属于用户授权的本轮暂缓，不是性能全部通过。#30 已以 `NOT_PLANNED` 关闭；该口径不改变 #31、#32 和 #33 的真实性要求。

## 维护者下一步

在不复制明文密钥、不读取用户原有会话的前提下，准备具备钥匙串认证的独立 Codex 测试环境，以及由 #31 冻结、#32 使用真实 Jev 固定版本完成的质量结果。随后用受控历史在最终签名包中重做真实总结、命名、取消、双后端预算、证据裁决及恢复流程，补齐 22 项场景和钥匙串拒绝访问证据。全部未放宽门槛实际满足后，#33 才能作为关闭 #11 的依据。
