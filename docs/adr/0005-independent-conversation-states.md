# 将 Conversation 的来源、显示、拓扑和用户状态分离

Conversation 的 missing、hidden、unlinked 和 User status 作为相互独立的维度，Archived Conversation 属于 Codex 来源状态；missing 记录及其 Dangling edge 默认保留，只有用户明确操作才清理。这样可以避免数据源暂时不可用、用户隐藏节点、关系尚未建立和用户生命周期标注互相误判，代价是界面需要同时解释多个状态。
