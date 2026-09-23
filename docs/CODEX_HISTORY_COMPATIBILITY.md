# Codex 历史读取兼容矩阵

核验日期：2026-09-24。本机命令 `codex-cli 0.150.1`。以 `initialize.capabilities.experimentalApi=true` 初始化 app-server，仅调用读取接口；没有恢复来源会话或运行模型。实际数据只输出接口状态与数量，未保存会话标识、正文或其他私人历史。

从未归档、已归档列表各取最多 30 条，样本共 60 条。其中来源声明 `historyMode=paginated` 为 56 条，`historyMode=legacy` 为 4 条。每种形态选择一条做以下只读探测：

| 来源形态 | `thread/turns/list`，`itemsView=notLoaded` | `thread/items/list` | `thread/read`，`includeTurns=true` | 适配策略 |
| --- | --- | --- | --- | --- |
| `paginated` | 成功，首屏 1 回合 | 成功，首屏 2 条目 | 成功，回合条目视图为 `full` | 先分页读取回合与条目；完整读取仅作失败兼容路径 |
| `legacy` | 成功，首屏 1 回合 | 返回 `-32601` | 成功，回合条目视图为 `full` | 条目分页不可用时使用 `thread/read` 完整历史 |

本机导出的实验 JSON Schema 还确认：回合默认 `itemsView=summary`，请求可显式选择 `notLoaded` 或 `full`；条目分页返回 `{turnId, item}`；两种分页均用不透明 `nextCursor`。适配器请求 `notLoaded` 后只从条目分页取得完整条目，并在旧式读取中检查 `itemsView=full`；因此摘要不会进入完整条目缓存。

契约测试另覆盖两页不透明游标、跨回合条目对应、未知类型、旧式完整读取、摘要拒绝、分页中途失败、单会话失败后继续读取。上述真实探测是有限样本，不代表所有旧版本和全部存储形态；后续发布验收仍需在受控历史上扩充兼容验证。
