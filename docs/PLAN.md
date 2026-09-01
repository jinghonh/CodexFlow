# Codex Conversation Graph & Timeline System — 实现顺序与里程碑

本文件只规定实现顺序、交付物和验证门槛。行为、数据模型、错误语义和范围边界以 docs/SPEC.md 为准；如果两者冲突，以 SPEC.md 为准。

## 实现前门槛

1. 确认实现者已阅读根目录 CONTEXT.md 和 docs/adr/0001 至 docs/adr/0009。
2. 确认 docs/SPEC.md、docs/PLAN.md 已被 Git 跟踪且不被 .gitignore 忽略。
3. 建立 app-server JSON-RPC fixture，至少覆盖当前稳定最低接口、active/archived 分页和来源失败。
4. 建立 Project 路径 fixture，覆盖 Git 根、嵌套仓库、Worktree、软链接和非 Git 目录。
5. 建立 Graph overlay YAML fixture，覆盖空文件、有效文件、损坏文件、重复键和未来版本。
6. 不在实现前引入 Codex 私有 SQLite/JSONL 解析作为正常数据路径。

## 实现顺序

### 1. 运行时骨架

交付：

- 本地后端启动；
- 回环地址监听；
- 生产环境前端静态资源托管；
- 开发环境前端联调方式；
- app-server stdio 子进程生命周期管理；
- health 接口。

验证：

- 服务只能从本机访问；
- app-server 无法启动时能返回 Source unavailable；
- 进程退出和重启不会破坏 Graph overlay。

### 2. CodexThreadSource

交付：

- initialize/initialized；
- 版本和能力探测；
- 显式 sourceKinds；
- active/archived 双分页；
- opaque cursor 处理；
- Thread 元数据规范化；
- Complete source snapshot；
- Source unavailable 和 stale 状态。

验证：

- 通过 CodexThreadSource 接缝完成全部来源 fixture；
- 不读取私有 SQLite/JSONL；
- 分页失败不会产生新的完整快照。

### 3. Project membership

交付：

- 单根 Project 选择；
- 原始路径和真实路径；
- Git 根和工作树根判断；
- 嵌套仓库排除；
- Worktree 独立；
- 软链接和失效路径处理；
- Project 根范围安全检查。

验证：

- 通过 ProjectGraphService 接缝完成所有路径 fixture；
- 路径组件边界和软链接越界测试通过。

### 4. Graph overlay 存储

交付：

- graph.yaml 版本 1 解析和校验；
- 稀疏 nodes 映射；
- edges 列表；
- 缺失文件处理；
- 重复键和 schema 错误；
- 未来版本只读；
- 旧版本迁移接口；
- ETag；
- 旁路锁；
- 唯一临时文件；
- 原子替换；
- 备份和恢复错误。

验证：

- 双写者不会静默丢失更新；
- YAML 损坏不会覆盖原文件；
- ETag 冲突能被客户端识别；
- 未来版本不会被旧程序写回。

### 5. ProjectGraphService

交付：

- 来源快照与 Graph overlay 合并；
- displayTitle；
- hidden、missing、unlinked 和 User status；
- Dangling edge；
- Graph 节点和关系命令；
- DashboardSnapshot。

验证：

- 状态派生和关系校验通过；
- 来源错误、缺失来源和本地 overlay 状态不互相误判。

### 6. 后端 HTTP API

交付：

- Project 选择和读取；
- snapshot；
- refresh；
- Conversation 读取；
- 节点 overlay 编辑；
- Edge 创建、编辑、删除；
- ETag 和冲突错误；
- 统一错误结构。

验证：

- API 契约测试覆盖成功、失败、冲突和 stale；
- API 不允许 Project 根之外的文件访问。

### 7. Conversation List

交付：

- 来源元数据显示；
- displayTitle；
- 搜索；
- 排序；
- 标签、User status、archived、missing、unlinked 过滤；
- Source unavailable/stale 提示。

验证：

- 浏览器级 Dashboard 接缝覆盖 List 的筛选和选择行为。

### 8. Graph View

交付：

- 节点显示；
- 节点选择；
- 缩放和平移；
- 拖动和布局提交；
- 节点详情编辑；
- 关系创建、编辑和删除；
- missing 占位节点；
- Dangling edge。

验证：

- 不把 React Flow 内部对象作为持久化模型；
- 所有修改通过 ProjectGraphService 和 HTTP 契约完成。

### 9. Timeline View

交付：

- Observation range；
- 用户时区；
- Day/Week/Month；
- 半开桶；
- 起止相同点标记；
- 无效时间警告；
- Overlap count；
- Timeline 与其他视图选择联动。

验证：

- 固定时钟 fixture 覆盖 DST、桶边界、零时长和无效范围。

### 10. 刷新与冲突交互

交付：

- dirty 状态；
- 保存、丢弃、取消；
- clean 刷新；
- stale 来源；
- ETag 冲突；
- 重新加载、保存副本、明确覆盖。

验证：

- 浏览器级场景覆盖每条分支；
- 任何失败都不静默丢失用户修改。

### 11. 安全与错误收口

交付：

- 回环监听检查；
- Project 根和软链接检查；
- app-server 进程退出处理；
- 权限、磁盘满、锁竞争和只读文件错误；
- 统一错误码和用户提示。

验证：

- 本地安全测试通过；
- 所有错误均能区分 Source unavailable、missing、Graph 错误和用户输入错误。

### 12. 最终验收

交付：

- SPEC.md 中的全部 MVP 用户故事有对应行为；
- 三个测试接缝有自动化测试；
- 无 Codex 原始数据写入；
- 无 Cloud、远程或 Sub-agent 越界；
- 文档、API、Graph overlay 和 UI 行为一致。

## 里程碑

### Milestone 1：来源与 Project 基础

完成运行时骨架、CodexThreadSource、单根 Project membership 和 health/snapshot 读取。

验收重点：可以稳定得到一个 Complete source snapshot，并正确处理 Source unavailable。

### Milestone 2：只读 Dashboard

完成 Project 选择、Conversation List、只读 Graph、只读 Timeline 和三视图选择联动。

验收重点：不写 Graph overlay 也能完整展示来源和 Observation range。

### Milestone 3：可编辑 Graph

完成节点 overlay、布局、人工关系、关系校验和 graph.yaml 提交式保存。

验收重点：刷新后布局和人工关系保持，重复边和自环被拒绝。

### Milestone 4：持久化可靠性

完成 ETag、旁路锁、唯一临时文件、原子替换、损坏保护、备份和迁移处理。

验收重点：双写者不会静默覆盖，YAML 错误不会破坏原文件。

### Milestone 5：Timeline 与冲突交互

完成时间桶、Overlap count、dirty 刷新、外部冲突和 stale 来源体验。

验收重点：时间边界和刷新分支符合 SPEC.md。

### Milestone 6：最终质量门

完成三个测试接缝的自动化测试、路径安全测试、版本能力测试和文档一致性检查。

验收重点：另一名开发者可以只阅读 CONTEXT.md、ADR、SPEC.md 和 PLAN.md 独立完成实现。
