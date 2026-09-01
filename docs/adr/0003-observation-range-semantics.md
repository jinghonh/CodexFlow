# Timeline 使用 Observation range 而非真实活动区间

MVP 的 Timeline 使用 created_at 到 updated_at 形成 Observation range，并明确不把它解释为持续活动区间或任务工期；内部采用 UTC 时间点、显示时使用用户时区，时间桶采用半开区间，起止相同显示为点。每个桶显示与 Observation range 相交的不同 Conversation 数，而非瞬时并发数或峰值；无效时间范围保留列表记录但不绘制时间线。这样可以使用稳定且低成本的线程元数据，代价是无法表达线程中间的空闲时间或每次回合的真实活动；若未来改用回合级数据，需重新评估指标含义。
