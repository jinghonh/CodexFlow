# 以 app-server 作为 Codex Thread 的主数据入口

本系统的 Conversation 主数据采用 Codex app-server 的 thread/list 与 thread/read 作为入口，范围限定为确认后的本地持久化交互线程；不把 Codex 私有 JSONL 或 SQLite 格式作为主要集成契约。运行时必须以实际 Codex binary 返回的版本/schema 为准，探测并依赖 initialize、thread/list、thread/read 这组最低能力；可选的 Turn 分页能力按探测结果启用。每次刷新必须完整读取 active 与 archived 的所有分页后才形成 Complete source snapshot；失败时保留上一次完整快照并标记 Source unavailable，不得判定 missing 或执行清理，也不自动回退到私有 JSONL/SQLite。这样可以把版本变化隔离在数据适配边界内，并使用 Codex 自己的线程元数据与筛选语义；代价是 app-server 目前仍属实验性接口，需要显式处理版本不兼容。
