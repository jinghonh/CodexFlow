# graph.yaml 采用稀疏 Graph overlay

graph.yaml 只保存用户拥有的 Conversation 信息和人工关系，采用以 Conversation ID 为键的 nodes 映射与关系列表作为唯一结构；Codex 标题、时间、cwd 和内容始终从来源读取，未被用户修改的 Conversation 不生成本地节点记录。这个选择避免复制会过期的来源数据，代价是每次读取都必须把来源快照与 Graph overlay 合并。
