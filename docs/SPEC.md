# Codex Conversation Graph & Timeline System — 最终可实施规范

本文件是本功能的行为规范。它基于已经确认的领域术语、ADR、现有 SPEC/PLAN 审查结果和调查底稿；实现必须满足本文件，不能把旧草稿中的冲突描述当作要求。

## 问题陈述

Codex 项目中的多个本地 Conversation 在 Codex 原生界面中主要以相互独立的 Thread 存在。用户很难同时回答以下问题：

- 哪些 Conversation 属于同一个 Project；
- 一个 Conversation 是另一个 Conversation 的后续、依赖、实现、审查或修复；
- 哪些 Conversation 还没有建立人工关系；
- 哪些 Conversation 已经被用户隐藏、已经归档或已经从 Codex 来源中消失；
- 多个 Conversation 在时间上如何分布；
- 一个时间桶内有多少个 Conversation 的 Observation range 相交。

当前文档还存在数据源、项目归属、边方向、状态、时间边界、YAML 并发写入和测试接缝不一致的问题。实现者不能通过猜测 Codex 私有文件格式或自行解释这些冲突来完成需求。

## 解决方案

提供一个仅在本机运行的 Dashboard：

1. 通过 Codex app-server 读取当前主机上的本地持久化交互 Thread；
2. 将每个 Thread 映射为一个 Conversation；
3. 用用户选定的单根 Project 过滤 Conversation；
4. 将 Codex 来源字段与本地 Graph overlay 合并；
5. 提供 Conversation List、Graph 和 Timeline；
6. 允许用户编辑本地标题、标签、状态、备注、隐藏标记、节点布局和人工关系；
7. 将全部用户数据保存为 Project 根目录下的 .codex/graph.yaml；
8. 对缺失来源、数据源失败、YAML 损坏、并发写入和未保存修改提供明确且不破坏数据的行为。

系统不修改 Codex 原始 Thread，也不自动推断 Conversation 关系。

## 用户故事

1. 作为 Project 用户，我希望选择一个本地 Project 根目录，以便只查看与该 Project 相关的 Conversation。
2. 作为 Project 用户，我希望看到当前主机上的本地 Codex 交互 Conversation，以便了解项目历史。
3. 作为用户，我希望同时看到 active 和 archived Conversation，以便归档不会被误判为 missing。
4. 作为用户，我希望每个 Conversation 使用 Codex Thread ID 作为唯一身份，以便刷新后仍能保持关系和布局。
5. 作为用户，我希望看到 Codex 标题、创建时间、更新时间、cwd、来源和归档状态，以便判断来源记录。
6. 作为用户，我希望可以使用自定义标题覆盖 Codex 标题，以便按项目语言组织 Conversation。
7. 作为用户，我希望添加、删除标签，以便按主题、阶段或领域过滤 Conversation。
8. 作为用户，我希望设置 none、active、done 或 blocked 状态，以便组织工作进展。
9. 作为用户，我希望为 Conversation 添加备注，以便保存不能从 Codex 元数据推导出的解释。
10. 作为用户，我希望隐藏一个 Conversation，使它从 Graph 中消失但仍保留在列表和 Timeline 中。
11. 作为用户，我希望知道某个 Conversation 是否没有任何人工关系，以便优先整理 unlinked Conversation。
12. 作为用户，我希望看到 Graph overlay 中仍引用但 Codex 来源已不存在的 Conversation，以便决定是否清理本地记录。
13. 作为用户，我希望数据源暂时不可用时看到 Source unavailable，而不是看到大量错误的 missing Conversation。
14. 作为用户，我希望拖动 Graph 节点并恢复上次布局，以便表达工作流结构。
15. 作为用户，我希望在两个 Conversation 之间创建人工关系，以便表达它们的工作语义。
16. 作为用户，我希望编辑或删除人工关系，以便修正此前的组织结果。
17. 作为用户，我希望使用 continues、depends_on、implements、reviewed_by、fixes、references 和 related_to，以便覆盖常见关系。
18. 作为用户，我希望输入自定义关系名称，以便表达内置类型之外的语义。
19. 作为用户，我希望系统阻止同一端点和关系类型的重复关系，以便 Graph 不被重复记录污染。
20. 作为用户，我希望不同关系类型、反向关系和环路可以共存，以便不被不必要的 DAG 约束限制。
21. 作为用户，我希望看到悬空边及其 missing 端点，以便历史关系不会因来源删除而静默消失。
22. 作为用户，我希望看到每个 Conversation 的 Observation range，以便了解 Codex 元数据覆盖的时间范围。
23. 作为用户，我希望 Timeline 使用我的时区显示日、周和月，以便时间桶符合我的日历理解。
24. 作为用户，我希望起止时间相同的 Conversation 显示为点，而不是消失。
25. 作为用户，我希望无效时间戳被警告而不是被系统偷偷交换或截断。
26. 作为用户，我希望看到每个时间桶内 Observation range 相交的不同 Conversation 数，以便比较项目活动密度。
27. 作为用户，我希望点击 List、Graph 或 Timeline 中的 Conversation 后三处同步高亮，以便在不同视图间定位同一实体。
28. 作为用户，我希望刷新后自动发现新增 Conversation，但不自动创建人工关系，以便新增来源不会改变我的语义判断。
29. 作为用户，我希望刷新时有未保存修改时可以保存、丢弃或取消，以便不会意外丢失编辑。
30. 作为用户，我希望外部程序修改 graph.yaml 后看到冲突提示，以便不会静默覆盖其他修改。
31. 作为用户，我希望 YAML 损坏时原文件保持不变，以便可以从备份或人工修复中恢复。
32. 作为用户，我希望未来版本的 graph.yaml 以只读方式打开，以便旧程序不会破坏新数据。
33. 作为用户，我希望本地服务只监听回环地址，以便其他设备不能直接访问我的 Conversation 数据。
34. 作为用户，我希望客户端不能借助 Project 选择功能读取任意文件，以便 Project 路径不会变成文件读取漏洞。
35. 作为用户，我希望在支持的平台上可以尝试用 Thread ID 打开 Codex Desktop，但失败时仍能复制 ID，以便该功能不会阻塞核心流程。
36. 作为维护者，我希望 Codex 数据适配、ProjectGraphService 和浏览器行为分别有稳定测试接缝，以便升级 Codex 或替换 UI 库时可以定位回归。

## 实现决策

### 1. 领域实体和来源范围

Conversation 是用户界面的领域实体，对应一个本地持久化 Codex Thread。

MVP 纳入：

- 当前主机上的本地持久化交互来源 cli；
- 当前主机上的本地持久化交互来源 vscode；
- 当前主机上的本地持久化交互来源 appServer；
- active Thread；
- archived Thread。

MVP 不纳入：

- exec 来源；
- Sub-agent 及其派生来源；
- SSH、remote-control 或其他远程主机来源；
- Codex Cloud Task；
- 普通 ChatGPT Conversation。

Conversation ID 只保存 app-server 返回的 thread.id。sessionId、projectId、rollout path、云端 Task ID 不能互换。

来源数据只读。系统不得调用 turn/start、thread/resume 或其他会改变 Codex Thread 的写入接口。

### 2. Codex app-server 读取契约

CodexThreadSource 负责隔离 app-server 协议和其版本差异。前端不得直接连接 app-server，也不得直接读取 JSONL 或 SQLite。

连接流程必须是：

1. 启动实际可用的 Codex app-server 子进程，使用 stdio JSON-RPC；
2. 调用 initialize；
3. 等待初始化完成后发送 initialized；
4. 探测并依赖 initialize、thread/list、thread/read 这三项最低能力；
5. 显式传入 sourceKinds=[cli, vscode, appServer]，不能依赖 app-server 默认来源过滤；
6. 分别以 archived=false 和 archived=true 枚举 active 与 archived Thread；
7. 使用服务端返回的 opaque nextCursor 继续分页，直到 nextCursor 为 null；
8. 将两组结果合并为一个 Complete source snapshot；
9. 按 thread.id 去重，并将 thread.id 映射为 Conversation ID。

app-server 的实际 binary、返回的 userAgent 和 schema 是版本依据。实现不能假定 PATH 中的 Codex CLI、桌面内置 Codex CLI 和官方仓库 main 具有相同协议能力。

thread/list 至少读取以下来源字段：

- id；
- name 或标题字段；
- preview；
- createdAt；
- updatedAt；
- recencyAt；
- cwd；
- source；
- historyMode；
- status；
- projectId；
- gitInfo。

thread/read 只用于按 ID 获取可读的单个 Thread 详情；MVP 不要求读取完整对话内容，也不要求依赖 Turn 分页接口。

任何 active 或 archived 分页失败、解析失败、连接断开、必需方法不存在或数据源启动失败，都不能形成新的 Complete source snapshot：

- 如果存在上一次完整快照，继续展示该快照并将来源标为 stale/Source unavailable；
- 如果不存在完整快照，返回 Source unavailable；
- 不重新计算 missing；
- 不执行缺失记录清理；
- 不自动回退到私有 JSONL 或 SQLite；
- 不把 notLoaded 或单个 thread/read 失败解释为 Conversation 不存在。

### 3. Project 和 Project membership

MVP 一次只选择一个 Project 根目录。Project 选择结果只保存在当前本地应用运行期间。

Project 选择必须满足：

- 输入路径存在；
- 输入路径是目录；
- 后端解析为真实路径；
- 后端保存原始选择路径用于显示，使用真实路径用于身份和安全检查；
- 真实路径中的组件边界必须严格检查，不能把 /project-a 当作 /project-ab 的父目录。

Conversation 属于 Project 的规则：

1. 将 Conversation 的 cwd 解析为真实路径；
2. cwd 无法解析、指向失效软链接或不再存在时，不属于 Project，但不标记为 missing；
3. cwd 必须等于 Project 根，或位于 Project 根之下；
4. 如果选定 Project 的最近 Git 根存在，cwd 的最近 Git 根必须与它相同；
5. 如果选定 Project 没有 Git 根，cwd 下的嵌套独立 Git 仓库不属于该 Project；
6. Worktree 以 git rev-parse --show-toplevel 返回的工作树根区分；不能使用 git-common-dir 合并 Worktree；
7. 软链接解析到同一真实目录时视为同一 Project 路径别名；
8. MVP 不支持多根 Project。

Codex app-server 的 projectId 是来源元数据，只用于展示或诊断，不参与 CodexFlow 的 Project membership 判定。

graph.yaml 的逻辑位置是所选 Project 根目录下的 .codex/graph.yaml。系统不得因为 Project 选择而访问该根目录之外的文件。

### 4. 统一数据模型

后端向前端提供一个合并后的 DashboardSnapshot。JSON 使用 camelCase；时间使用 UTC 的 RFC 3339 字符串，精度不超过来源提供的秒。

Conversation 的逻辑结构如下：

    Conversation
      id: string
      codex:
        title: string | null
        preview: string
        createdAt: string
        updatedAt: string
        recencyAt: string | null
        cwd: string
        source: cli | vscode | appServer
        archived: boolean
        historyMode: legacy | paginated | null
        status: notLoaded | idle | systemError | active | unknown
        projectId: string | null
        gitInfo: object | null
      overlay:
        title: string | null
        tags: string[]
        status: none | active | done | blocked
        note: string | null
        hidden: boolean
        layout: { x: number, y: number } | null
      derived:
        missing: boolean
        unlinked: boolean
        sourceAvailable: boolean
        validObservationRange: boolean
      displayTitle: string

来源标题显示顺序：

1. overlay.title 非空时使用它；
2. 否则使用 Codex name/title；
3. 否则使用 preview 的第一行；
4. preview 为空时使用 Conversation ID。

未出现在 Graph overlay 中的 Conversation 默认值为：

- title=null；
- tags=[]；
- status=none；
- note=null；
- hidden=false；
- layout=null。

### 5. graph.yaml 唯一结构

graph.yaml 是稀疏 Graph overlay，只保存本系统拥有的字段和人工关系，不保存 Codex 标题、时间、cwd、完整内容或来源快照。

规范版本 1 的结构为：

    version: 1
    project:
      name: optional display name
    nodes:
      thread-id:
        title: optional custom title
        tags:
          - tag
        status: none
        note: optional note
        hidden: false
        layout:
          x: 320
          y: 180
    edges:
      - id: edge-id
        source: thread-id
        target: thread-id
        type: continues
        label: optional explanation

根级规则：

- version 必须是整数 1；
- project 可省略，name 可省略；
- nodes 缺省等同于空映射；
- edges 缺省等同于空列表；
- 保存时使用 version、project、nodes、edges 的稳定顺序；
- Project 路径不写入 graph.yaml，文件位置代表 Project；
- graph.yaml 属于本机用户数据，MVP 保持在 Git 忽略范围内。

节点规则：

- nodes 的键必须是非空 Conversation ID；
- 节点记录只表示存在本地自定义信息；
- 未修改的来源 Conversation 不创建节点记录；
- title 为空字符串或去除首尾空白后为空时等同于未设置；
- tags 去除首尾空白、去重并保留用户首次出现的顺序；
- hidden 缺省为 false；
- layout 缺省表示使用前端默认布局；
- status 只能是 none、active、done、blocked；
- 用户状态不能使用 archived；
- 节点记录可以引用当前来源中不存在的 ID。

边规则：

- id 是后端生成的稳定不透明标识，用户编辑关系时不得重新生成；
- source 和 target 是 Conversation ID；
- missing 端点允许存在，以支持 Dangling edge；
- type 是内置关系 token 或非空自定义关系名称；
- type 去除首尾空白；内置 token 使用小写固定拼写；
- label 是补充说明，不参与关系唯一性；
- 同一端点和同一关系类型只能存在一条关系；
- 不同关系类型可以连接同一对端点；
- 有方向关系的反向边是另一条关系；
- 禁止自环；
- 允许环路；
- related_to 的端点按稳定字典序规范化，A related_to B 与 B related_to A 只能保存一条。

重复边、非法自环、非法关系类型和无效节点数据都必须在保存前拒绝。

### 6. 人工关系语义

有方向关系使用 source → target，并按以下句式解释：

| type | 句式 |
|---|---|
| continues | source continues target |
| depends_on | source depends_on target |
| implements | source implements target |
| reviewed_by | source is reviewed_by target |
| fixes | source fixes target |
| references | source references target |
| related_to | source related_to target，无方向 |

例如，B 是 A 的后续工作时，关系是 B → A，type=continues。Graph 的布局方向不能改变关系语义。

系统不根据 Conversation 内容、文件修改、标题相似度、Embedding 或 LLM 自动创建关系。

### 7. hidden、missing、unlinked 和用户状态

这些维度必须独立处理。

hidden：

- 是用户的 Graph 显示选择；
- 只从 Graph 画布中隐藏节点及其相关视觉元素；
- 不从 Conversation List 或 Timeline 删除；
- 不改变是否 linked；
- 不改变 Overlap count。

missing：

- 只在 Complete source snapshot 中找不到被 nodes 或 edges 引用的 Conversation ID 时成立；
- archived Conversation 不算 missing；
- 来源筛选、分页未完成、notLoaded、读取错误、路径无法解析和 Source unavailable 都不算 missing；
- missing 节点可以作为占位节点显示；
- missing 节点不参与 Observation range 和 Overlap count；
- 不自动删除 missing 节点或 Dangling edge。

unlinked：

- 是图拓扑派生状态；
- 当前来源中存在的 Conversation 没有任何入边或出边时为 true；
- 与 hidden、missing 和 User status 独立；
- 一个节点即使只连接到 missing 端点，也不算 unlinked；
- “只看 Unlinked”默认只过滤当前来源中存在的 Conversation。

User status：

- 取值为 none、active、done、blocked；
- 只用于组织、显示和过滤；
- 不影响 Observation range、Timeline 或 Overlap count；
- 不替代 Codex 的 archived 来源状态。

### 8. Timeline 和时间计算

Timeline 只使用 Codex 来源的 createdAt 和 updatedAt。用户不能拖动或手动修改 Conversation 时间。

Observation range：

- start = createdAt；
- end = updatedAt；
- 来源整数 Unix 秒先解释为 UTC，再转换为内部时间点；
- start < end 时表示半开区间 [start, end)；
- start = end 时表示一个时间点；
- end < start、时间缺失或格式错误时为无效 Observation range。

无效 Observation range 的行为：

- Conversation 仍显示在 List；
- 不绘制 Timeline bar；
- 不计入 Overlap count；
- 显示可定位的数据警告；
- 不自动交换、截断或补齐时间。

Time bucket：

- Day 是用户时区的本地日 00:00 至次日 00:00；
- Week 使用 ISO 周，从用户时区的星期一 00:00 开始；
- Month 从用户时区当月第一日 00:00 开始；
- 夏令时由用户时区的日历规则决定，不把一天强制换算为固定小时数；
- 默认使用系统时区，用户可以在界面选择显示时区；
- 时区偏好不写入 graph.yaml。

Overlap count：

- 对每个 Time bucket 统计不同且当前存在的 Conversation；
- 正常区间与桶相交的条件为 start < bucketEnd 且 end > bucketStart；
- start=end 的点只计入包含该时间点的桶；
- hidden Conversation 计入；
- missing Conversation 不计入；
- archived Conversation 计入；
- source snapshot stale 时沿用上一次完整快照并在 UI 显示 stale 标记；
- 不统计瞬时并发峰值、不累计活动时长、不推断真实回合活动。

### 9. Graph、List 和 Timeline 交互

界面必须维护一个全局 selectedConversationId：

- 点击 List 行设置它；
- 点击 Graph 节点设置它；
- 点击 Timeline 行或 bar 设置它；
- 三个视图同步高亮；
- selectedConversationId 指向 missing Conversation 时，Graph 可显示占位节点，List 显示来源缺失；
- 关闭详情面板不改变选择；
- 搜索和过滤不能改变实体 ID。

Conversation List 至少支持：

- 搜索显示标题、Codex 标题、preview 和 Conversation ID；
- 按 createdAt 或 updatedAt 排序；
- 按标签过滤；
- 按 User status 过滤；
- 按 archived 过滤；
- 按 missing 过滤；
- 按 unlinked 过滤；
- 查看 stale/Source unavailable 提示。

Graph 至少支持：

- 缩放；
- 平移；
- 节点拖动；
- 拖动结束后保存 layout；
- 节点选择；
- 创建、编辑、删除人工关系；
- 节点详情编辑；
- missing 占位节点和 Dangling edge 展示。

Timeline 至少支持：

- Day、Week、Month；
- 横向缩放和滚动；
- 显示用户时区；
- 点击和悬停查看精确时间；
- 按标签、User status、archived 和 hidden 过滤；
- 显示 Overlap count；
- 显示无效时间警告。

### 10. 后端接口

后端提供同源 JSON HTTP API。所有接口以 /api 为前缀，时间使用 UTC RFC 3339，错误使用统一结构：

    {
      error: {
        code: string,
        message: string,
        details: object | null,
        retryable: boolean
      }
    }

最低接口：

GET /api/health

- 返回本地服务状态、app-server 连接状态、实际 Codex binary/userAgent 和当前 Project 状态；
- 不返回对话正文或秘密。

POST /api/project/select

- 请求体：{ path: string }；
- 验证路径存在、是目录、真实路径可解析；
- 设置当前单根 Project；
- 读取 Project 来源和 Graph overlay；
- 返回 ProjectView 或结构化错误。

GET /api/project

- 返回当前 Project 的原始选择路径、真实路径、Git 根、工作树根、是否 Git 项目和 graph 文件状态；
- 不返回 Project 根之外的文件内容。

GET /api/snapshot

- 返回当前 Project 的 DashboardSnapshot；
- 返回来源状态、快照生成时间、Graph overlay 的 ETag；
- 若有上一次完整快照但当前来源不可用，返回 200 和 source.status=stale；
- 从未形成完整快照时返回 503/source_unavailable；
- Graph YAML 损坏时返回 422/graph_parse_error，不覆盖原文件。

来源状态只能是 ready、stale、unavailable 或 incompatible。stale 表示沿用上一次 Complete source snapshot；unavailable 表示尚未有可用快照或当前来源不可访问；incompatible 表示必需 app-server 能力不存在。

POST /api/refresh

- 重新读取 Codex active 与 archived 分页；
- 重新读取并验证 graph.yaml；
- 返回新的 DashboardSnapshot；
- 任何分页或解析失败都保留上一次完整来源快照；
- 不自动删除 missing、边或本地 overlay；
- 前端在调用前必须处理未保存修改。

PATCH /api/graph/nodes/{conversationId}

- 修改一个 Conversation 的 overlay 字段；
- 支持 title、tags、status、note、hidden、layout；
- 要求 If-Match 头匹配当前 Graph overlay ETag；
- 成功后返回新的 Snapshot 和 ETag；
- Conversation 来源是否存在不影响保存本地 overlay，但新增未知 ID 必须明确标记为 missing。

POST /api/graph/edges

- 请求体：source、target、type、label；
- 后端生成 id；
- 校验自环、重复关系、关系类型和端点；
- 成功后返回新的边、Snapshot 和 ETag。

PATCH /api/graph/edges/{edgeId}

- 修改 type、label、source 或 target；
- id 不变；
- 修改后的关系必须重新通过重复和自环校验；
- 成功后返回 Snapshot 和 ETag。

DELETE /api/graph/edges/{edgeId}

- 只删除指定人工关系；
- 不删除端点 Conversation；
- 成功后返回 Snapshot 和 ETag。

Graph overlay 的 ETag 是当前 graph.yaml 完整文件字节的 SHA-256 小写十六进制摘要，并作为强 ETag 返回；文件不存在时使用特殊值 absent。所有会改变 Graph overlay 的接口都必须要求 If-Match；只有用户明确选择覆盖冲突时才允许使用通配符 If-Match: *。

GET /api/conversations/{conversationId}

- 返回指定 Conversation 的合并模型；
- 来源不存在但 ID 被 Graph overlay 或边引用时返回 missing 占位模型；
- ID 未被任何当前 Graph 数据引用时返回 404。

Open in Codex 是可选适配器，不属于核心接口。支持的平台可以返回 codex://threads/{thread.id}；调用失败时返回可复制的 Conversation ID，不得影响其他接口。

HTTP 状态和错误码至少遵循以下映射：

| HTTP 状态 | error.code | 含义 |
|---|---|---|
| 400 | invalid_request | 请求字段缺失或格式错误 |
| 403 | path_not_allowed | 路径超出 Project 安全边界 |
| 404 | project_not_selected / conversation_not_found / edge_not_found | 目标不存在 |
| 409 | graph_busy / duplicate_edge / self_edge | 锁竞争或关系约束冲突 |
| 412 | graph_conflict | If-Match 与当前文件不一致 |
| 422 | project_invalid / graph_parse_error / graph_schema_error / graph_version_error / invalid_time | 输入或文件内容无法接受 |
| 423 | graph_read_only | graph.yaml 属于未来版本或当前不可写 |
| 503 | source_unavailable / source_incompatible | Codex 来源不可用或缺少最低能力 |

### 11. 本地运行和安全边界

技术基线：

- 后端使用 Python/FastAPI；
- 前端使用 React/TypeScript/Vite；
- Graph 使用 React Flow 兼容实现；
- Timeline 使用 CSS Grid、SVG 或等价的轻量绘制，不引入需要维护任务工期的甘特图模型；
- 不使用数据库保存 Graph overlay，graph.yaml 是唯一用户数据存储。

- 生产环境由后端提供已构建前端；
- 开发环境可以单独运行 Vite，但前端仍只调用本地 HTTP API；
- 后端只监听 127.0.0.1；
- 不提供局域网监听开关；
- 后端管理一个长期存活的 app-server stdio 子进程，不依赖 daemon 控制 socket；
- app-server 子进程退出时标记 Source unavailable，并允许安全重启；
- 客户端只能选择 Project 根目录，不能通过 API 读取任意文件；
- 所有路径在真实路径解析后做 Project 根范围检查；
- 禁止软链接把 Graph 文件访问引出 Project 根；
- 不读取 Codex 私有 SQLite/JSONL 作为正常回退路径；
- 不保存认证 token、完整 Conversation 内容或 Codex 原始历史到 Graph overlay。

### 12. graph.yaml 写入、冲突和恢复

Graph overlay 保存是提交式的：

- 节点拖动结束时保存；
- 创建、编辑、删除边时保存；
- 节点详情中的文本编辑点击保存时保存；
- 编辑面板未提交期间由前端保留 dirty 状态；
- 不在每个鼠标移动事件中写文件。

写入流程必须：

1. 在 .codex/graph.yaml.lock 获取旁路锁；
2. 读取当前文件并计算基线 ETag；
3. 比较请求 If-Match 与当前 ETag；
4. 校验完整 Graph overlay；
5. 使用同目录内每次写入唯一名称的 .graph.yaml.tmp.<random> 临时文件；
6. 完整写入并刷新临时文件；在运行时支持时刷新父目录；
7. 原子替换 graph.yaml；
8. 释放锁并返回新的 ETag。

固定名称的 graph.yaml.tmp 不允许作为共享临时文件。

并发行为：

- 锁获取失败返回 409/graph_busy；
- ETag 不匹配返回 412/graph_conflict；
- 不自动合并两个 Graph overlay；
- 外部冲突界面提供重新加载、保存副本、明确覆盖；
- 明确覆盖只能由用户在冲突界面选择后发起；
- 保存副本不能替换当前 graph.yaml。

刷新行为：

- dirty 时必须显示保存、丢弃、取消；
- 保存失败或产生冲突时停留在 dirty 状态；
- 丢弃只丢弃尚未提交的前端修改；
- 取消不刷新来源，也不改变当前页面状态；
- clean 时发现外部文件变化可以直接重新加载，但仍须重新验证 YAML。

YAML 错误行为：

- 文件不存在：视为空 overlay，首次成功保存时创建规范版本 1；
- YAML 语法错误：返回 graph_parse_error，原文件不动；
- 重复 YAML mapping key：视为错误，原文件不动；
- 已知字段类型错误：返回 graph_schema_error，原文件不动；
- version 缺失、非法或不支持：返回 graph_version_error，原文件不动；
- version 大于当前版本：以只读方式打开，禁止保存；
- 迁移旧版本时先在内存中完成并验证，成功后创建带时间标记的备份，再原子写入；
- 迁移备份使用 graph.yaml.bak.<UTC timestamp>.<random>，不得覆盖已有备份；MVP 不自动删除备份；
- 迁移失败时原文件和备份均不被覆盖；
- 未知字段和值在读写时尽量保留；YAML 注释和格式不属于数据契约；
- 文件权限、磁盘满和父目录只读时返回结构化保存错误，不能清空内存中的当前快照。

### 13. 前端启动和项目选择

启动后前端必须先调用 health，再要求用户选择 Project 或恢复本次运行期间的当前 Project。

项目选择成功后：

1. 显示 Project 真实路径和 Git/Worktree 信息；
2. 请求 snapshot；
3. 同时加载 List、Graph 和 Timeline；
4. 显示 graph.yaml 是否不存在、只读、损坏或存在冲突；
5. 不因 graph.yaml 不存在而创建空文件，直到首次成功保存。

### 14. 可观察性和错误显示

用户可见错误至少包括：

- Codex app-server 未安装或无法启动；
- 必需协议能力缺失；
- active/archived 分页失败；
- 来源数据 stale；
- Project 路径无效；
- cwd 无法解析；
- graph.yaml 解析失败；
- graph.yaml schema/version 不兼容；
- Graph overlay 并发冲突；
- Graph overlay 锁竞争；
- 重复关系；
- 自环；
- 无效时间范围。

错误消息必须说明：

- 错误类别；
- 是否可重试；
- 是否保留了旧快照；
- 用户可执行的下一步。

## 测试决策

### 测试原则

测试只锁定外部行为和公开接缝，不锁定内部类名、私有函数、React Flow 内部结构、SQLite 表结构、JSONL 文件布局或具体临时文件名。

当前仓库没有业务实现和既有测试，因此没有可复用的测试框架或测试先例。实现可以选择适合语言栈的工具，但必须保留以下三个接缝。

### 接缝一：CodexThreadSource

这是 app-server 适配器接缝。测试使用脱敏的 JSON-RPC 响应 fixture，不读取真实用户 Conversation。

必须覆盖：

- initialize/initialized 顺序；
- 0.150.1 stable schema；
- 含实验方法的版本能力差异；
- 显式 sourceKinds；
- active 与 archived 两组分页；
- opaque cursor 循环；
- 空列表；
- 分页中途失败；
- app-server 进程退出；
- 必需方法缺失；
- notLoaded；
- thread/read 单项失败；
- 不把来源错误判定为 missing；
- 秒级时间转换。

### 接缝二：ProjectGraphService

这是领域应用服务接缝。测试注入伪造 CodexThreadSource、临时文件系统和固定时钟。

必须覆盖：

- 单根 Project 选择；
- 真实路径和路径组件边界；
- 非 Git Project；
- Git 根；
- 嵌套独立仓库；
- Worktree 与 git-common-dir；
- 有效软链接；
- 失效软链接；
- missing、hidden、unlinked 和 User status 正交性；
- 稀疏 overlay 合并；
- Dangling edge；
- 关系方向；
- related_to 规范化；
- 重复关系、自环、反向关系和环路；
- Observation range；
- 无效时间范围；
- Day/Week/Month 桶；
- DST 和用户时区；
- Overlap count；
- YAML 缺失、损坏、重复键和版本错误；
- ETag 冲突；
- 锁竞争；
- 临时文件写入和原子替换；
- 迁移备份和未来版本只读。

### 接缝三：浏览器级 Dashboard

浏览器测试通过 HTTP API 测试替身运行，不启动真实 Codex app-server。

必须覆盖：

- Project 选择；
- List/Graph/Timeline 的 selectedConversationId 联动；
- 搜索和过滤；
- hidden 只影响 Graph；
- missing 占位节点；
- unlinked 过滤；
- 节点编辑和提交；
- 拖动结束保存；
- 关系创建、编辑、删除；
- 自定义关系和重复提示；
- related_to 反向重复；
- Timeline 桶和点标记；
- 无效时间警告；
- dirty 时保存/丢弃/取消；
- 外部冲突的重新加载/保存副本/明确覆盖；
- Source unavailable 和 stale 展示。

## 范围边界

本规范不包含：

- AI 自动关系识别；
- Embedding、语义搜索或内容相似度；
- Conversation 内容摘要；
- 完整 Conversation transcript 展示；
- Turn 级真实活动区间；
- 手动修改 Conversation 时间；
- Git Commit、Branch、Pull Request、File、Task、Spec、Plan 或 Agent 节点；
- 自动分类；
- Cloud Task；
- 普通 ChatGPT Conversation；
- exec、Sub-agent、SSH、remote-control 来源；
- 多根 Project；
- 跨 Worktree 合并；
- 多人协作；
- 云同步；
- Graph overlay 的 Git 版本控制；
- 实时文件监听；
- 自动备份同步到云端；
- 自动打开指定 Turn 或 Item；
- 依赖桌面内部工具；
- Codex 私有 SQLite/JSONL 解析作为正常运行路径；
- 对 Codex 原始 Thread 的任何写入。

## 进一步说明

1. 本规范中的 Conversation、Project、Project membership、Observation range、Graph overlay、hidden、missing、unlinked、Dangling edge、Complete source snapshot、Source unavailable 和 Overlap count 以根目录 CONTEXT.md 为准。
2. 当前确认的长期架构决策记录在 docs/adr/0001 至 docs/adr/0009。
3. 现有 docs/research/codex-conversation-data.md 是带一手资料链接的研究底稿，不是行为规范；其中关于 projectId 优先级和私有存储 fallback 的建议未被采用。
4. 当前 .gitignore 已通过例外规则保留 docs/SPEC.md 和 docs/PLAN.md，且两个文件已被 Git 跟踪；本规范不要求增加重复例外。
5. 临时交互原型只用于帮助检查状态模型，不是生产代码，也不是本规范的运行依赖。
6. 任何未在本文件定义的 Codex 字段都必须被视为可选来源元数据，不能成为 Conversation 身份或 Project membership 的隐含条件。
