# Codex Conversation Graph & Timeline

本上下文定义 Codex Conversation 图谱与时间线中的核心领域语言。它记录用户已经确认的概念边界，不替代产品规范或实现计划。

## Language

**Conversation**：
本系统面向用户的基本领域实体，指一个本地持久化的 Codex Thread。范围包含 cli、vscode 和 appServer 等交互线程及其归档记录，不包含 exec、Sub-agent、云端任务或普通 ChatGPT Conversation；它以 Codex 提供的 Conversation ID 作为身份，人工关系和本地元数据都附着其上。
_避免_：ChatGPT Conversation、Task

**Codex Thread**：
Codex 本地数据源中的会话记录，本系统将它映射为一个 Conversation。它不等同于普通 ChatGPT API 的 Conversation 资源。

**Conversation ID**：
Codex Thread 的稳定身份标识，是图节点、人工关系和时间线联动所共同引用的标识。

**Project**：
用户选定的文件系统根目录及其项目边界。Project 是本系统的组织单位，不直接等同于 Codex 的 projectId。

**Project membership**：
当 Conversation 的 cwd 解析后位于选定根目录内，且最近的 Git 根与该 Project 的 Git 根一致时，该 Conversation 属于此 Project；非 Git Project 使用目录包含关系，嵌套的独立仓库不属于外层 Project。Worktree 默认视为独立 Project，解析到同一真实目录的软链接视为路径别名；无法解析的 cwd 不属于 Project，但不因此变成 missing。

**cwd**：
Codex Thread 记录的工作目录。它是判断 Project membership 的来源字段，不单独决定 Project 身份。

**Observation range**：
由 Conversation 的 created_at 到 updated_at 推导出的元数据观察范围，不表示其在整个区间内持续活动，也不表示项目任务工期。时间点按 UTC 解释，按用户时区显示。
_避免_：Activity Range、任务工期

**Time bucket**：
时间线按用户时区划分的日、周或月区间。范围计数使用半开区间 [start, end)，起止相同的 Conversation 以点标记表示并归入包含该时间点的时间桶。

**Graph overlay**：
项目为 Conversation 保存的用户自有信息层，不复制 Codex 的来源字段。没有 overlay 记录表示该 Conversation 没有用户自定义信息，不表示 Conversation 不存在。

**hidden**：
用户对 Graph 显示的隐藏选择。hidden 不表示 Conversation 缺失，也不改变它是否已建立关系。

**missing**：
当完整的 Codex 来源快照中找不到本地仍被引用的 Conversation ID 时产生的来源状态。来源不可用、读取未完成、记录被筛选或尚未加载时不算 missing。

**unlinked**：
当前存在的 Conversation 没有任何入边或出边时产生的拓扑状态。它与 hidden、missing 以及用户状态相互独立。

**User status**：
用户为组织 Conversation 设置的状态，取值为 none、active、done 或 blocked。它不表示 Codex 的归档状态，也不改变 Observation range。

**Archived Conversation**：
Codex 来源标记为已归档的 Conversation。它仍属于本系统的本地 Conversation 范围，不应被误判为 missing。

**Dangling edge**：
至少有一个端点对应 missing Conversation 的人工关系。Dangling edge 会被保留，直到用户明确删除它。

**Complete source snapshot**：
当前主机上 active 与 archived 本地交互线程的所有分页均成功读取并解析后的来源快照。只有在此快照完成后，来源缺失才可以改变 Conversation 的 missing 状态。

**Source unavailable**：
来源读取、分页或解析未完成时的状态。Source unavailable 只能表示当前数据不可确认，不能被解释为 Conversation missing，也不能触发清理操作。

**Overlap count**：
一个 Time bucket 内 Observation range 与该桶相交的不同 Conversation 数。它不是某一时刻的并发数、峰值或累计活动时长；hidden Conversation 计入，missing Conversation 不计入。

**人工关系**：
用户在 Conversation 之间明确建立的语义关系，不由系统根据内容、文件或相似度推断。关系可以使用内置类型或用户定义的关系名称。

**关系方向**：
有方向的人工关系从关系主语指向关系对象，source 和 target 组成可读的关系句式；例如 B depends_on A 表示 B 依赖 A。related_to 是无方向关系，反向端点不构成第二条关系。

**关系唯一性**：
同一对端点和同一关系类型只能存在一条人工关系；不同关系类型或反向关系可以分别存在。关系说明文字不改变关系身份，自环不属于有效人工关系。
