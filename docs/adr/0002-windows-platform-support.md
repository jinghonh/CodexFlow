# Windows 11 x64 适配保留系统边界

CodexFlow 在 macOS 与 Windows 11 x64 正式支持本机运行，并把 Codex CLI 发现、操作系统凭据访问和安装打包留在平台适配层。Windows 版只从 `PATH` 查找 `.exe` 与 `.cmd`，以 `%USERPROFILE%\.codex` 为 Codex 默认目录并尊重 `CODEX_HOME`；文本服务与 Jev API Key 写入 Windows Credential Manager。NSIS 提供一个可选当前用户或全机范围的未签名安装器，WebView2 缺失时允许联网下载 bootstrapper。

采用系统凭据库是为了延续 API Key 不落明文文件的边界；凭据后端不可用时保存必须失败。Windows 的 NSIS `both` 安装模式会在启动时请求管理员权限，这一安装体验由用户明确接受。此决定于 2026-09-26 确认。
