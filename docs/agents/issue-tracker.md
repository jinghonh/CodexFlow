# 问题跟踪：GitHub

本仓库的问题和规格保存在 GitHub 议题中，使用 `gh` 命令行工具操作。
当前远程仓库为 `jinghonh/CodexFlow`；执行操作时从 Git 远程配置确认目标仓库。

## 常用操作

- 创建事项：`gh issue create --title "标题" --body-file <正文文件>`
- 读取事项及评论：`gh issue view <编号> --comments`
- 获取结构化信息：`gh issue view <编号> --json number,title,body,labels,comments`
- 列出待处理事项：`gh issue list --state open --json number,title,body,labels`
- 按标签筛选：为列表命令添加 `--label "<标签>"`
- 添加评论：`gh issue comment <编号> --body-file <评论文件>`
- 添加标签：`gh issue edit <编号> --add-label "<标签>"`
- 移除标签：`gh issue edit <编号> --remove-label "<标签>"`
- 关闭事项：`gh issue close <编号>`

多行正文先写入临时文件，再通过 `--body-file` 传入。

技能要求“发布到问题跟踪系统”时，创建 GitHub 议题。
技能要求“获取相关工单”时，读取对应议题的正文、标签和评论。

## 拉取请求参与分流

PRs as a request surface: no.

GitHub 议题和拉取请求共享编号空间。遇到不明确的编号时，
先确认其对象类型，再使用对应的 `gh issue` 或 `gh pr` 命令。

## 探索任务的组织方式

供探索任务技能使用：

- 用带有 `wayfinder:map` 标签的单个议题作为任务地图，
  记录笔记、已有决策和待澄清问题。
- 将子任务关联为地图议题的子议题。
  不支持子议题时，在地图正文中维护任务列表，
  并在子任务正文顶部写明 `Part of #<地图编号>`。
- 子任务类型标签采用 `wayfinder:<类型>`，
  类型为 `research`、`prototype`、`grilling` 或 `task`。
- 优先使用 GitHub 原生议题依赖记录阻塞关系；
  不支持时，在子任务顶部使用 `Blocked by: #<编号>` 记录。
- 按地图顺序选择没有未关闭阻塞项、且尚未分配负责人的开放子任务。
- 认领任务时使用 `gh issue edit <编号> --add-assignee @me`。
- 完成后记录结论、关闭子任务，并在地图的已有决策中补充摘要和链接。
