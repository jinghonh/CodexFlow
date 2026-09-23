# CodexFlow

CodexFlow V1 的桌面应用入口。当前阶段提供中文 Codex 来源设置与连接诊断；完整会话采集和 Jev 连接由后续工单接入。

## 本机启动

需要 macOS、Node.js、npm 和 Rust 1.88。项目已通过 `rust-toolchain.toml` 固定 Rust 工具链。

```sh
npm install
npm run tauri dev
```

首次打开会查找应用可见 `PATH` 中的 `codex`，并执行一次不调用模型的诊断。也可在界面中选择绝对路径。选择结果与界面外观保存在应用管理的本机用户数据目录。

## 验证入口

```sh
npm run build
cargo check --workspace
cargo test -p codexflow-codex -p codexflow-store -p codexflow-core
```

受控协议替身验证握手、最低列表能力缺失与异常退出。诊断只读取最小列表元数据，并以不存在的固定会话 ID 探测历史接口；不会恢复会话或启动模型。能力报告中的总结与命名只证明所选二进制导出的协议包含临时会话及结构化输出，不代表实际模型运行已通过。
