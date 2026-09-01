# Codex Conversation Graph & Timeline System — PLAN

## 1. 实现目标

按照 SPEC，实现一个本地运行的 Codex Conversation 可视化工具。

第一阶段重点完成：

```text
Codex Conversation 数据读取
        ↓
统一数据模型
        ↓
Conversation List
        ↓
Graph Editor
        ↓
graph.yaml 持久化
        ↓
Timeline
        ↓
Graph / Timeline 联动
```

不实现 AI 自动关系识别。

---

# 2. 推荐技术栈

前端：

```text
React
TypeScript
Vite
React Flow
```

Graph 使用：

```text
@xyflow/react
```

原因：

- 节点拖拽成熟；
- Edge 创建成熟；
- 自定义节点容易；
- 支持节点选择；
- 支持 Zoom / Pan；
- 适合后续扩展。

Timeline 可以优先自己实现，不建议第一版引入复杂甘特图库。

推荐：

```text
React
+
CSS Grid / SVG
```

原因是当前 Timeline：

```text
不需要拖动
不需要依赖关系编辑
不需要任务进度编辑
```

本质只是时间区间可视化。

---

# 3. 后端

推荐：

```text
Python
FastAPI
```

职责：

```text
读取 Codex Conversations
读取 graph.yaml
写 graph.yaml
项目发现
Conversation 查询
```

后端不负责 Graph 布局算法。

---

# 4. 项目结构

建议：

```text
codex-graph/
│
├── frontend/
│   ├── src/
│   │   ├── components/
│   │   │   ├── conversation/
│   │   │   ├── graph/
│   │   │   └── timeline/
│   │   │
│   │   ├── pages/
│   │   ├── stores/
│   │   ├── api/
│   │   ├── types/
│   │   └── utils/
│   │
│   └── package.json
│
├── backend/
│   ├── app/
│   │   ├── api/
│   │   ├── codex/
│   │   ├── graph/
│   │   ├── models/
│   │   └── main.py
│   │
│   └── requirements.txt
│
├── docs/
│   ├── SPEC.md
│   └── PLAN.md
│
└── README.md
```

---

# 5. Phase 1：调查 Codex 数据来源

第一步不要直接写 UI。

先验证能够稳定获得：

```text
conversation_id
title
created_at
updated_at
cwd
```

目标是实现统一接口：

```python
list_conversations(project_path)
```

返回：

```json
[
  {
    "id": "thread-a",
    "title": "Implement Trust Region",
    "created_at": "...",
    "updated_at": "...",
    "cwd": "/project"
  }
]
```

优先使用 Codex 提供的正式接口。

如果正式接口无法满足需求，再增加本地数据适配层。

必须把 Codex 数据访问封装在：

```text
backend/app/codex/
```

避免前端依赖 Codex 内部实现。

---

# 6. Phase 2：定义统一数据模型

建立：

```typescript
interface Conversation {
    id: string
    codexTitle: string
    createdAt: string
    updatedAt: string
    cwd: string

    title?: string
    tags: string[]
    status?: string
    note?: string

    hidden: boolean

    position?: {
        x: number
        y: number
    }
}
```

Edge：

```typescript
interface ConversationEdge {
    id: string
    source: string
    target: string
    type: string
    label?: string
}
```

---

# 7. Phase 3：实现 graph.yaml

后端建立：

```text
GraphRepository
```

提供：

```text
load_graph(project_path)

save_graph(project_path, graph)

update_node(...)

update_edge(...)

delete_edge(...)
```

第一版可以直接：

```text
整文件读取
→ 修改
→ 整文件写回
```

无需数据库。

---

# 8. graph.yaml 安全写入

写文件时使用：

```text
graph.yaml.tmp
        ↓
完整写入
        ↓
rename
        ↓
graph.yaml
```

避免程序异常导致 YAML 损坏。

---

# 9. Phase 4：项目数据合并

实现：

```text
Codex Conversations
        +
graph.yaml
        ↓
Merged Project Model
```

伪代码：

```python
for conversation in codex_conversations:

    metadata = graph.nodes.get(conversation.id)

    merged.append(
        merge(conversation, metadata)
    )
```

如果 graph.yaml 中存在 Codex 不存在的 ID：

```text
missing = true
```

---

# 10. Phase 5：Conversation List

先实现左侧 Conversation List。

支持：

```text
搜索
按创建时间排序
按更新时间排序
按标签过滤
按状态过滤
只看 Unlinked
```

每一行至少展示：

```text
Title
Updated At
Status
Tags
```

---

# 11. Unlinked 判断

定义：

```python
linked_ids =
{
    edge.source
    for edge in edges
}
∪
{
    edge.target
    for edge in edges
}
```

如果：

```text
conversation.id not in linked_ids
```

则：

```text
Unlinked = true
```

---

# 12. Phase 6：Graph 基础实现

使用 React Flow。

将：

```text
Conversation
```

转换成：

```text
React Flow Node
```

将：

```text
graph.yaml edges
```

转换成：

```text
React Flow Edge
```

第一阶段完成：

- Zoom；
- Pan；
- Node Drag；
- Node Selection；
- Edge Rendering。

---

# 13. Phase 7：Graph 编辑

实现：

```text
拖节点
        ↓
更新 position
        ↓
保存 graph.yaml
```

连接：

```text
Source Handle
        ↓
拖到 Target Handle
        ↓
创建 Edge
        ↓
选择关系类型
        ↓
保存
```

---

# 14. Edge 编辑 UI

点击 Edge 后打开侧栏：

```text
From
To
Type
Label

[Save]
[Delete]
```

默认类型：

```text
continues
depends_on
implements
reviewed_by
fixes
references
related_to
```

同时允许：

```text
Custom
```

---

# 15. Phase 8：Node 编辑

点击节点显示 Details Panel。

允许修改：

```text
Title
Tags
Status
Note
Hidden
```

只读：

```text
Conversation ID
Codex Title
Created At
Updated At
cwd
```

---

# 16. 节点状态

第一版可以定义：

```text
none
active
done
blocked
archived
```

状态只用于组织和过滤。

不影响 Timeline。

---

# 17. Phase 9：Timeline 数据模型

对每个 Conversation：

```typescript
interface TimelineItem {
    conversationId: string
    start: Date
    end: Date
}
```

其中：

```text
start = createdAt
end   = updatedAt
```

---

# 18. Timeline 坐标计算

假设当前显示区间：

```text
T_min
T_max
```

Conversation：

```text
start_i
end_i
```

计算：

\[
x_{start}
=
\frac{start_i-T_{min}}
{T_{max}-T_{min}}
\]

\[
x_{end}
=
\frac{end_i-T_{min}}
{T_{max}-T_{min}}
\]

Bar：

```text
left  = x_start
width = x_end - x_start
```

---

# 19. Timeline 时间尺度

第一版实现三种：

```text
Day
Week
Month
```

例如：

```text
[ Day ] [ Week ] [ Month ]
```

默认根据整个项目时间跨度自动选择。

---

# 20. Phase 10：Activity Density

将整个时间轴划分为时间桶：

Day 模式：

```text
1 day / bucket
```

Week 模式：

```text
1 week / bucket
```

Month 模式：

```text
1 month / bucket
```

对每一个 bucket：

```python
count = number_of_conversations_overlapping(bucket)
```

重叠条件：

```text
conversation.start <= bucket.end

AND

conversation.end >= bucket.start
```

---

# 21. Phase 11：Graph / Timeline 联动

建立统一：

```text
selectedConversationId
```

放入前端 Store。

例如：

```text
Zustand
```

Graph 点击：

```text
setSelectedConversation(id)
```

Timeline 点击：

```text
setSelectedConversation(id)
```

Conversation List 点击：

```text
setSelectedConversation(id)
```

三个组件都订阅：

```text
selectedConversationId
```

因此：

```text
Conversation List
Graph
Timeline
```

天然联动。

---

# 22. Phase 12：刷新机制

增加：

```text
Refresh
```

按钮。

刷新时：

```text
重新读取 Codex
        ↓
重新 merge graph.yaml
        ↓
发现新增 Conversation
        ↓
刷新 UI
```

第一版不需要实时监听。

---

# 23. Phase 13：打开原始 Conversation

如果 Codex 支持可稳定定位到某个 thread，应提供：

```text
Open in Codex
```

如果当前无法稳定跳转，则第一版保留：

```text
Conversation ID
```

及复制能力。

此功能不能阻塞 MVP。

---

# 24. Phase 14：异常处理

必须处理：

### graph.yaml 不存在

自动创建：

```yaml
version: 1

nodes: {}

edges: []
```

### YAML 格式错误

不得覆盖原文件。

显示：

```text
graph.yaml parse failed
```

### Conversation 删除

显示：

```text
Missing
```

### created_at = updated_at

Timeline 至少绘制最小宽度的标记。

---

# 25. Phase 15：UI 布局

建议桌面端：

```text
┌─────────────────────────────────────────────────┐
│ Project / Search / Filter / Refresh             │
├─────────────┬───────────────────────────────────┤
│             │                                   │
│ Conversation│                                   │
│ List        │             Graph                 │
│             │                                   │
│             │                                   │
├─────────────┴───────────────────────────────────┤
│                                               │
│                  Timeline                     │
│                                               │
└─────────────────────────────────────────────────┘
```

Graph 应占主要空间。

Timeline 高度保持适中。

---

# 26. Phase 16：持久化策略

不要每次鼠标移动都写文件。

节点拖动时：

```text
drag
drag
drag
drag
drag end
```

只在：

```text
drag end
```

保存。

文本编辑可以：

```text
Save
```

显式保存。

---

# 27. Phase 17：测试

后端测试：

```text
Codex conversation parser
graph.yaml load
graph.yaml save
merge
missing conversation
unlinked calculation
```

前端测试重点：

```text
Graph selection
Edge creation
Edge deletion
Node position update
Timeline range calculation
Graph / Timeline selection sync
```

---

# 28. 实现顺序

严格建议按照以下顺序实现：

```text
1 Codex 数据读取
        ↓
2 graph.yaml
        ↓
3 数据 merge
        ↓
4 Conversation List
        ↓
5 Graph 只读展示
        ↓
6 Graph 编辑
        ↓
7 Node / Edge Metadata
        ↓
8 Timeline
        ↓
9 Activity Density
        ↓
10 Graph / Timeline 联动
        ↓
11 过滤 / 搜索
        ↓
12 UI 优化
```

不要一开始同时开发：

```text
Graph
Timeline
Codex Parser
YAML
各种过滤
```

否则调试成本会明显增加。

---

# 29. 推荐开发里程碑

## Milestone 1：Data Prototype

目标：

```text
能够读取当前项目的所有 Conversation
```

输出简单 JSON。

---

## Milestone 2：Graph Prototype

目标：

```text
Conversation → React Flow Node
```

Graph 可以正常显示。

---

## Milestone 3：Editable Graph

目标：

```text
创建 Edge
删除 Edge
拖动 Node
编辑 Node
```

并成功保存：

```text
.codex/graph.yaml
```

---

## Milestone 4：Timeline

目标：

自动生成：

```text
Conversation Activity Timeline
+
Active Conversation Count
```

---

## Milestone 5：Integrated Dashboard

实现：

```text
Conversation List
      ↕
Graph
      ↕
Timeline
```

三者联动。

---

# 30. MVP 完成后的下一阶段

MVP 稳定后，再考虑：

```text
Task Group
Conversation Group
文档节点
Git Commit 节点
Branch 信息
Worktree 信息
Conversation 内容摘要
关系模板
Graph 子图
多个 Graph Workspace
项目统计
```

其中比较值得优先增加的是：

```text
Conversation Group
```

即允许多个 Conversation 被人工分组，但不改变：

\[
Conversation
\]

作为基础实体的设计。