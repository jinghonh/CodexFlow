# CodexFlow

CodexFlow 是一个仅在本机运行的 Codex Conversation 图谱与时间线 Dashboard。它从 Codex app-server 读取本地主机上的交互 Thread，将 Thread 映射为稳定的 Conversation，再结合用户维护的 Graph overlay，帮助用户查看项目历史、人工关系和时间分布。

当前版本：`0.1.0`（MVP）

## 功能

- 通过 `codex app-server --stdio` 读取本地主机上的 `cli`、`vscode` 和 `appServer` Conversation。
- 同时显示 active 和 archived Conversation，并区分来源不可用、stale 和 missing。
- 按真实路径、Git 根、工作树、嵌套仓库和软链接规则选择单个 Project。
- 提供 Conversation List、Graph 和 Timeline 三个视图。
- 编辑本地标题、标签、User status、备注、隐藏标记和 Graph 节点布局。
- 创建、编辑和删除人工关系，支持内置关系、自定义关系、无向 `related_to`、悬空边和关系约束。
- 使用 `createdAt` 到 `updatedAt` 的 Observation range 显示 Day、ISO Week、Month 时间桶和 Overlap count。
- 使用 ETag、旁路锁、唯一临时文件、原子替换、损坏保护、备份和版本迁移保存 Graph overlay。
- 刷新时保护未提交修改，提供保存、丢弃和取消分流。

系统不会修改 Codex 原始 Thread，也不会根据内容、文件、标题相似度或模型自动创建关系。

## 架构概览

```text
Codex app-server (stdio JSON-RPC)
              │
              ▼
       CodexThreadSource
              │
              ▼
      ProjectGraphService ─── .codex/graph.yaml
              │
              ▼
       FastAPI JSON API
              │
              ▼
       React + Vite Dashboard
```

- 后端代码位于 `src/codexflow/`，使用 Python、FastAPI 和 Pydantic。
- 前端代码位于 `frontend/src/`，使用 React、TypeScript 和 Vite。
- Graph overlay 是 Project 根目录下的 `.codex/graph.yaml`，不是数据库，也不是 Codex 来源快照。
- 生产启动时，后端同时提供 `frontend/dist` 中的前端静态资源。

## 数据来源和范围

CodexFlow 只通过公开的 app-server stdio JSON-RPC 接口读取来源：

1. `initialize` / `initialized` 握手；
2. 探测 `initialize`、`thread/list` 和 `thread/read` 最低能力；
3. 显式请求 `sourceKinds=[cli, vscode, appServer]`；
4. 分别完整读取 active 与 archived 分页；
5. 使用 app-server 返回的 `thread.id` 作为 Conversation ID。

不纳入 exec、Sub-agent、SSH、远程主机、Cloud Task 或普通 ChatGPT Conversation。Codex 私有 JSONL 和 SQLite 不是回退数据源。

如果 app-server 启动失败、能力不兼容或分页不完整，系统会显示 `Source unavailable`；已有完整快照时保留旧数据并标记为 stale，不会因此制造新的 missing Conversation。

## Project 选择

一次只能选择一个 Project 根目录。系统会保留用户输入的原始路径用于显示，并使用真实路径进行身份和安全检查。

Conversation 是否属于 Project，依据其 `cwd` 的真实路径、路径组件边界、最近 Git 根和工作树根判断：

- Project 根和合法子目录可以纳入；
- 嵌套独立仓库会被排除；
- Worktree 默认视为独立 Project；
- 指向同一真实目录的软链接视为路径别名；
- 失效软链接或无法解析的 `cwd` 不属于 Project，但不等于 missing；
- Project 根之外的路径不能通过 API 读取。

## Graph overlay

Graph overlay 只保存 CodexFlow 自己拥有的字段和人工关系，不复制 Codex 标题、时间、`cwd`、完整内容或来源快照。

文件位置：

```text
<project-root>/.codex/graph.yaml
```

最小示例：

```yaml
version: 1
project:
  name: CodexFlow
nodes:
  conversation-id:
    title: 本地标题
    tags: [research]
    status: active
    note: 后续工作说明
    hidden: false
    layout:
      x: 320
      y: 180
edges:
  - id: edge-id
    source: conversation-id
    target: another-conversation-id
    type: continues
    label: 后续实现
```

文件不存在时按空 overlay 处理，选择 Project 不会立即创建文件；首次成功保存时才创建规范版本 1。该文件属于本机用户数据，默认保持在 Git 忽略范围内。

`hidden` 只影响 Graph 显示；`missing` 表示完整来源快照中找不到仍被 overlay 或关系引用的 ID；`unlinked` 表示当前存在的 Conversation 没有任何关系。这些状态彼此独立。

## Timeline

Timeline 使用来源的 `createdAt` 和 `updatedAt` 形成 Observation range：

- 来源整数 Unix 秒先按 UTC 解释；
- 界面按用户选择的时区显示；
- 支持 Day、ISO Week 和 Month；
- 使用半开时间桶，避免相邻桶重复计数；
- `start=end` 显示为点；
- 无效时间保留在 List，但不绘制也不计入 Overlap count；
- Overlap count 统计每个时间桶内相交的不同 Conversation 数。

## 安装和启动

### 环境要求

- Python 3.11 或更高版本；
- `uv`；
- Codex CLI，且 `codex app-server --stdio` 可从 `PATH` 启动；
- 只有前端开发和前端测试需要 Node.js 与 npm。

### 生产方式启动

在仓库根目录执行：

```bash
uv sync --locked
uv run codexflow
```

然后打开 <http://127.0.0.1:8000>。后端只监听本机回环地址，不提供局域网监听开关。

### 前后端分离开发

先启动后端：

```bash
uv sync --locked --extra test
uv run uvicorn codexflow.app:app --host 127.0.0.1 --port 8000
```

再在另一个终端启动 Vite：

```bash
cd frontend
npm ci
npm run dev
```

开发前端默认位于 <http://127.0.0.1:5173>，并将 `/api` 请求代理到 `127.0.0.1:8000`。前端构建完成后，可再次使用 `uv run codexflow` 通过后端访问构建产物。

## HTTP API

所有接口都使用 `/api` 前缀，并返回 JSON。错误结构为：

```json
{
  "error": {
    "code": "graph_conflict",
    "message": "Graph overlay 已被其他写入者修改",
    "details": null,
    "retryable": true
  }
}
```

主要接口：

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| GET | `/api/health` | 服务、来源和当前 Project 状态 |
| POST | `/api/project/select` | 选择 Project，请求体为 `{ "path": "..." }` |
| GET | `/api/project` | 读取当前 Project 信息 |
| GET | `/api/snapshot` | 获取合并后的 DashboardSnapshot |
| POST | `/api/refresh` | 重新读取来源和 Graph overlay |
| PATCH | `/api/graph/nodes/{conversationId}` | 更新节点 overlay 和布局 |
| POST | `/api/graph/edges` | 创建人工关系 |
| PATCH | `/api/graph/edges/{edgeId}` | 编辑人工关系 |
| DELETE | `/api/graph/edges/{edgeId}` | 删除人工关系 |
| POST | `/api/graph/migrate` | 迁移旧版本 Graph overlay |
| POST | `/api/graph/copy` | 保存冲突处理使用的副本 |

读取 Timeline 时可传入 `granularity=day|week|month` 和 IANA 时区，例如：

```text
/api/snapshot?granularity=week&timezone=Asia/Shanghai
```

所有会改变 Graph overlay 的请求都需要 `If-Match`。ETag 不匹配返回 `412 graph_conflict`；锁竞争返回 `409 graph_busy`。只有用户明确选择覆盖时，客户端才可以请求 `overwrite=true`。

## 测试

后端测试：

```bash
uv run --locked --extra test pytest -q
```

前端测试和构建：

```bash
cd frontend
npm ci
npm test
npm run build
```

测试分别覆盖三个稳定接缝：

- `CodexThreadSource`：使用脱敏 JSON-RPC fixture 验证 app-server 握手、能力、分页和失败语义；
- `ProjectGraphService`：使用伪造来源、临时文件系统和固定时钟验证 Project、Graph、关系、Timeline、并发和迁移；
- 浏览器级 Dashboard：使用 HTTP 测试替身验证 List、Graph、Timeline 的联动、过滤、编辑、刷新和冲突处理。

浏览器验收脚本位于 `tests/e2e/`，使用脱敏 fixture，不连接真实用户 Conversation。

## 安全和范围边界

- 服务只绑定 `127.0.0.1`；
- 客户端不能借助 Project 选择读取根目录之外的文件；
- 不保存认证 token、完整 Conversation 内容或 Codex 原始历史到 Graph overlay；
- 不写入 Codex 原始 Thread；
- 不支持多根 Project、跨 Worktree 合并、云同步、实时文件监听或多人协作；
- 不包含 AI 自动关系识别、Embedding、语义搜索或 Turn 级真实活动区间。

## 相关文档

- [行为规范](docs/SPEC.md)
- [实现顺序与里程碑](docs/PLAN.md)
- [领域上下文](CONTEXT.md)
- [架构决策记录](docs/adr/)
