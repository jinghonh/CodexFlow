# Project 采用用户选定根目录的保守归属规则

MVP 一次只选择一个存在的 Project 根目录，使用解析后的路径和最近 Git 根判断 Conversation membership；非 Git 根目录使用目录包含关系，嵌套独立仓库排除，无法解析的 cwd 排除但不标记 missing，Worktree 默认独立，软链接只作为同一真实目录的别名，graph.yaml 归属于选定根目录。这个选择优先避免把不同项目或不同 Worktree 的 Conversation 意外混在一起，并保持本地数据位置明确；需要合并多个根目录时另行显式建模。
