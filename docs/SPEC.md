# Codex Conversation Graph & Timeline System — SPEC

## 1. 项目目标

构建一个面向 Codex 项目的本地可视化系统，用于管理同一项目下多个 Codex Conversation 之间的关系和时间分布。

系统核心不尝试自动判断不同 Conversation 之间的语义关系，而是：

- 将每一个 Codex Conversation 视为一个独立节点；
- 由用户人工建立、修改和删除 Conversation 之间的关系；
- 自动读取 Conversation 的时间信息；
- 提供 Graph 和 Timeline/Gantt 两种互补视图。

核心模型：

[\
\text{Conversation}=\text{系统唯一基本实体}\
]

其中：

\text{Conversation}\
+\
\text{人工定义关系}\
]

\text{Conversation}\
+\
\text{自动时间信息}\
]

---

# 2. 使用场景

在一个较大的 Codex 项目中，一个任务可能经历：

```text
Conversation A
需求讨论
    ↓
生成 spec.md
    ↓
Conversation B
实现任务
    ↓
Conversation C
继续实现
    ↓
Conversation D
检查完成情况
    ↓
Conversation E
修复 Review 问题
```

这些 Conversation 在 Codex 原生界面中通常只是独立对话。

本系统需要将其组织为：

```text
需求讨论
   │
   │ continues
   ↓
实现第一阶段
   │
   │ continues
   ↓
实现第二阶段
   │
   │ reviewed_by
   ↓
代码检查
```

同时在时间视图中显示这些 Conversation 的实际活动时间。

---

# 3. 设计原则

## 3.1 不自动推断 Conversation 关系

系统不通过：

- LLM；
- Embedding；
- 对话内容相似度；
- 文件修改相似度；

自动判断两个 Conversation 是否相关。

Conversation 之间的语义关系全部由用户人工定义。

---

## 3.2 Codex 数据与用户数据分离

Codex 原始数据负责提供：

```text
conversation_id
原始标题
创建时间
最后更新时间
cwd
conversation 内容
其他 Codex metadata
```

本系统负责保存：

```text
自定义标题
节点标签
节点状态
节点备注
Graph 位置
Conversation 之间的关系
关系类型
```

两者通过：

```text
conversation_id
```

关联。

---

## 3.3 不修改 Codex 原始 Conversation 数据

系统原则上只读取 Codex 数据。

用户创建的额外信息统一保存到项目目录：

```text
.codex/
└── graph.yaml
```

系统不得依赖修改 Codex 内部数据库才能正常工作。

---

# 4. 系统范围

第一阶段系统只管理：

```text
Conversation
```

暂不将以下对象建模为独立 Graph 节点：

```text
Task
Spec
Plan
Git Commit
Pull Request
File
Agent
Sub-agent
```

这些能力后续可以扩展。

---

# 5. 核心实体

## 5.1 Conversation

每一个 Codex Conversation 对应一个节点。

建议内部结构：

```yaml
conversation:
  id: string

  codex:
    title: string
    created_at: datetime
    updated_at: datetime
    cwd: string

  metadata:
    title: string | null
    tags: []
    status: string | null
    note: string | null

  layout:
    x: number
    y: number
```

其中：

```text
codex.*
```

来自 Codex。

```text
metadata.*
layout.*
```

来自本系统。

---

# 6. Graph View

## 6.1 基本目标

Graph View 用于回答：

> 不同 Conversation 之间是什么关系？

示例：

```text
         ┌───────────────┐
         │ 确定需求       │
         └───────┬───────┘
                 │
              continues
                 │
                 ▼
         ┌───────────────┐
         │ 实现功能       │
         └───────┬───────┘
                 │
             reviewed_by
                 │
                 ▼
         ┌───────────────┐
         │ Review        │
         └───────────────┘
```

---

# 7. Graph 节点

每个节点至少展示：

```text
自定义标题 / Codex 标题
状态
标签
创建时间
最后活动时间
```

默认优先显示：

```text
metadata.title
```

如果不存在，则显示：

```text
codex.title
```

---

# 8. Graph 节点操作

用户必须能够：

- 拖动节点；
- 修改节点显示名称；
- 添加标签；
- 删除标签；
- 设置状态；
- 添加备注；
- 查看 Conversation 基本信息；
- 打开对应 Codex Conversation；
- 创建节点之间的边；
- 删除边；
- 修改边类型。

Graph 节点本身不能被真正删除。

如果 Conversation 仍存在于 Codex 中，则它始终是一个有效节点。

可以提供：

```text
Hide
```

功能将节点从当前 Graph 中隐藏。

---

# 9. Graph Edge

Edge 表示两个 Conversation 之间的人工语义关系。

基本结构：

```yaml
- id: edge-001
  from: conversation-a
  to: conversation-b
  type: continues
  label: null
```

---

# 10. 默认关系类型

第一版至少支持：

```text
continues
depends_on
implements
reviewed_by
fixes
references
related_to
```

含义：

| Type         | 含义                                     |
| ------------ | -------------------------------------- |
| continues    | B 是 A 的后续工作                            |
| depends\_on  | B 依赖 A                                 |
| implements   | B 实现 A 中确定的内容                          |
| reviewed\_by | B 的工作由另一个 Conversation Review          |
| fixes        | 当前 Conversation 修复前一个 Conversation 的问题 |
| references   | 当前 Conversation 参考另一个 Conversation     |
| related\_to  | 一般关联                                   |

用户还应该能够直接输入自定义关系名称。

---

# 11. Graph 布局

节点位置需要持久化。

例如：

```yaml
layout:
  "thread-a":
    x: 320
    y: 180

  "thread-b":
    x: 620
    y: 180
```

重新打开项目后，应恢复上一次 Graph 布局。

---

# 12. Timeline / Gantt View

## 12.1 基本目标

Timeline View 用于回答：

> 每一个 Conversation 在什么时候开始、什么时候结束，以及某个时间段同时存在多少个活动 Conversation？

Timeline 不需要人工维护。

---

# 13. Conversation 时间范围

默认定义：

```text
start_time = created_at
end_time   = updated_at
```

因此 Timeline 展示的是：

```text
Conversation Activity Range
```

而不是严格意义上的项目任务工期。

例如：

```text
Conversation A

2026-09-01 10:00
        ↓
开始

2026-09-01 16:30
        ↓
最后一次活动
```

Timeline 显示：

```text
10:00 ─────────────── 16:30
```

---

# 14. Timeline 示例

```text
Conversation             Sep 1      Sep 2      Sep 3

确定需求                  ███████
实现功能                     ███████████
继续实现                               ███████
Review                                  █████
修复问题                                   █████
```

---

# 15. 活跃 Conversation 数量

Timeline 需要自动统计：

```text
某个时间点 / 时间段中处于活动区间的 Conversation 数
```

即：

\sum\_i\
I\
(\
start\_i\
\leq t\
\leq end\_i\
)\
]

例如：

```text
Sep 1       7
Sep 2      11
Sep 3       5
Sep 4       9
```

可在 Timeline 上方增加 Activity Density。

例如：

```text
Active
12 │        █
10 │        █       █
 8 │ █      █       █
 6 │ █  █   █   █   █
 4 │ █  █   █   █   █
   └─────────────────────
     1  2   3   4   5
```

---

# 16. Timeline 交互

Timeline 至少支持：

- 横向缩放时间；
- 横向滚动；
- 按日期查看；
- 按周查看；
- 按月查看；
- 点击 Conversation；
- Hover 查看详细时间；
- 搜索 Conversation；
- 按标签过滤；
- 按状态过滤。

Timeline 中：

```text
Conversation 时间不可人工拖动修改。
```

时间必须来自 Codex metadata。

---

# 17. Graph 与 Timeline 联动

Graph 和 Timeline 必须通过：

```text
conversation_id
```

联动。

例如：

在 Graph 点击：

```text
实现 Trust Region
```

则：

```text
Graph
节点高亮

Timeline
对应行高亮
```

反向同样成立。

---

# 18. Conversation 列表

除了 Graph 和 Timeline，需要有一个简单的 Conversation List。

至少展示：

```text
Title
Created At
Updated At
Tags
Status
```

支持：

```text
搜索
排序
过滤
```

Conversation List 可作为寻找未组织 Conversation 的入口。

---

# 19. 未关联 Conversation

系统需要能够识别：

```text
存在于 Codex
但没有任何 Graph Edge
```

的 Conversation。

标记为：

```text
Unlinked
```

例如：

```text
All Conversations     38
Linked                27
Unlinked              11
```

方便用户逐步整理项目。

---

# 20. graph.yaml

建议结构：

```yaml
version: 1

project:
  name: GP-PSL

nodes:

  "thread-aaa":
    title: "确定 PSL Trust Region 方案"

    tags:
      - PSL
      - design

    status: done

    note: >
      确定输入空间 trust region 方案。

    hidden: false

    layout:
      x: 320
      y: 180


  "thread-bbb":
    title: "实现 PSL Trust Region"

    tags:
      - PSL
      - implementation

    status: in_progress

    layout:
      x: 620
      y: 180


edges:

  - id: edge-001

    from: "thread-aaa"
    to: "thread-bbb"

    type: continues


  - id: edge-002

    from: "thread-bbb"
    to: "thread-ccc"

    type: reviewed_by
```

---

# 21. graph.yaml 保存原则

只保存本系统独有的信息。

不要重复保存：

```text
created_at
updated_at
cwd
完整 Conversation 内容
```

这些字段始终从 Codex 获取。

---

# 22. 数据同步

系统启动时：

```text
读取 Codex Conversations
        ↓
读取 .codex/graph.yaml
        ↓
按照 conversation_id 合并
        ↓
生成 UI
```

---

# 23. Conversation 新增

如果 Codex 新增 Conversation：

```text
Codex
新增 Thread
   ↓
Dashboard 刷新
   ↓
发现新的 conversation_id
   ↓
自动加入 Conversation List
   ↓
Graph 中成为未关联节点
```

不自动创建 Edge。

---

# 24. Conversation 不存在

如果：

```text
graph.yaml
```

存在某个：

```text
conversation_id
```

但 Codex 已不存在对应 Conversation：

系统不得删除 graph.yaml 数据。

节点应显示为：

```text
Missing Conversation
```

并允许用户手动清理。

---

# 25. 项目识别

系统以 Codex Conversation 的：

```text
cwd
```

作为主要项目识别依据。

原则上：

```text
相同项目目录
        ↓
属于同一 Project Dashboard
```

---

# 26. 页面结构

建议：

```text
┌──────────────────────────────────────────────────┐
│ Project                                          │
│ Search     Filter       Refresh                  │
├───────────────┬──────────────────────────────────┤
│ Conversations │                                  │
│               │                                  │
│ Conversation A│            Graph                 │
│ Conversation B│                                  │
│ Conversation C│       A ───────→ B               │
│ Conversation D│                    │             │
│               │                    ↓             │
│               │                    C             │
│               │                                  │
├───────────────┴──────────────────────────────────┤
│ Activity Timeline                               │
│                                                 │
│ A ███████                                       │
│ B    ███████████                                │
│ C          ███████                              │
└──────────────────────────────────────────────────┘
```

---

# 27. MVP 范围

第一版只要求完成：

1. 读取当前项目 Conversation；
2. Conversation List；
3. Graph 节点展示；
4. Graph 节点拖动；
5. Graph 人工连线；
6. 删除 Edge；
7. 修改关系类型；
8. 修改节点标题；
9. 保存 `.codex/graph.yaml`；
10. 自动 Timeline；
11. 自动 Activity Count；
12. Graph / Timeline 点击联动。

以下功能暂不属于 MVP：

```text
AI 自动关系识别
Conversation 内容 Embedding
任务自动分类
Git Commit 自动关联
PR 自动关联
文档节点
Task 节点
多人协作
远程服务
云同步
```

---

# 28. 验收标准

系统达到以下条件即认为 MVP 完成：

- 可以读取一个 Codex 项目中的 Conversation；
- 一个 Conversation 对应唯一节点；
- 新 Conversation 能自动出现；
- 用户可以手动建立 Conversation 关系；
- 关系能够持久化；
- 重新启动系统后 Graph 布局保持不变；
- Timeline 无需人工配置即可生成；
- Timeline 时间严格来自 Codex metadata；
- 可以看到任意时间段内的活跃 Conversation 数；
- Graph 和 Timeline 可以通过 conversation\_id 相互联动；
- 不修改 Codex 原始 Conversation 数据；
- 所有人工数据统一保存在 `.codex/graph.yaml`。
