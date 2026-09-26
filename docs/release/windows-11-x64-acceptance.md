# Windows 11 x64 支持验收记录

验收目标：Windows 11 x64 原生运行与构建，保持 macOS 行为，并生成可手动下载的 NSIS `.exe` 安装器。用户确认首批包无需签名；缺少 WebView2 Runtime 时允许在线下载 bootstrapper；安装器允许选择当前用户或全机范围。全机与当前用户选择均接受安装器启动时请求管理员权限。

验收环境：Windows 11 专业版 x64（10.0.26200），Rust 1.88.0。Windows CI 不属于当前范围。

安装器：`target/x86_64-pc-windows-msvc/release/bundle/nsis/CodexFlow_0.1.0_x64-setup.exe`，5,107,729 bytes，`Get-AuthenticodeSignature` 状态为 `NotSigned`。已用 `/CurrentUser` 完成静默安装到 `%LOCALAPPDATA%\Programs\CodexFlow`，安装后的窗口标题为 `CodexFlow`。启动进程的应用数据目录与 `CODEX_HOME` 指向临时验收目录，凭据使用唯一命名空间，`PATH` 仅保留 Windows `System32`。

## 产品要求

- [x] Windows CLI 仅从应用可见的 `PATH` 自动发现，并兼容 `.exe` 与 `.cmd`。
- [x] Codex 数据目录默认 `%USERPROFILE%\.codex`，支持 `CODEX_HOME` 覆盖。
- [x] 文本服务与 Jev API Key 使用 Windows Credential Manager；没有明文回退。
- [x] NSIS 安装器同时提供当前用户和全机范围。
- [x] WebView2 缺失时允许联网下载 bootstrapper。
- [x] Windows 11 x64 纳入 README 与 V1 规格。
- [x] 暂不增加 Windows GitHub Actions。

## 验收结果

| 项目 | 结果 | 证据 |
| --- | --- | --- |
| Windows 11 x64 原生发行构建 | 通过 | `cargo build --release --target x86_64-pc-windows-msvc --jobs 1`；`npm run build:windows:local`。产物目标为 x64。 |
| 相关 Rust 回归 | 通过 | `cargo test --jobs 1 -p codexflow-jev -p codexflow-codex -p codexflow-core`：97 passed，30 ignored，0 failed。 |
| UI 构建与 Windows 专项回归 | 通过 | Tauri beforeBuild 的 `npm run build` 成功；`src/ProjectExplorerFlow.test.tsx`：3 passed。 |
| 全量 UI 回归 | 部分通过 | Vitest：29 passed，16 failed，单 worker 复跑结果相同；失败分布于分析、项目图、会话详情和时间线视图。Windows 来源选择专项用例通过。 |
| NSIS 安装器生成与无签名检查 | 通过 | `CodexFlow_0.1.0_x64-setup.exe`，5,107,729 bytes；`Get-AuthenticodeSignature` 返回 `NotSigned`。 |
| 当前用户安装与启动 | 通过 | 安装到 `%LOCALAPPDATA%\Programs\CodexFlow`；进程保持运行，窗口标题 `CodexFlow`；验收数据使用临时隔离目录。 |
| Windows Credential Manager 合成密钥保存、读取、删除 | 通过 | `keychain::tests::credential_manager_round_trip_uses_an_isolated_target`。 |
| `.cmd` 与 `.exe` PATH 查找及 `.cmd` 启动 | 通过 | `windows_path_lookup_includes_exe_and_cmd_launchers`、`timed_out_auxiliary_processes_are_reaped_across_retries`；相关 Codex 套件 24 passed。 |

真实 Codex 历史、真实 API Key、付费模型请求和 GitHub Release 上传均不作为本次合成环境验收输入。
