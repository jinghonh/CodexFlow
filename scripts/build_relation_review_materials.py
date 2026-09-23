#!/usr/bin/env python3
"""生成 #31 的合成逐对复核材料；仅用于首次生成草案。"""

from __future__ import annotations

import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "data" / "relation-review" / "v0.1-draft"
VERSION = "0.1-draft"
BASE_MS = 1_790_000_000_000

# 所有名称、路径、输出均为专为本基准撰写的合成内容。
TOPICS = [
    ("pagination", "会话分页", "crates/codex/src/history.rs", "游标跨页去重", "归档页重复回传", "游标去重表", "页边界断言"),
    ("identity", "项目归属", "crates/core/src/projects.rs", "共享 Git 目录识别", "独立克隆被误并", "项目身份判定", "克隆隔离断言"),
    ("cache", "分析缓存", "crates/core/src/cache.rs", "内容版本缓存键", "旧摘要复用", "版本键计算", "过期缓存断言"),
    ("credential", "Jev 凭据", "crates/jev/src/config.rs", "钥匙串读取", "切换服务仍发送旧密钥", "服务域凭据绑定", "跨域泄漏断言"),
    ("timeline", "活动时间线", "crates/domain/src/timeline.rs", "分段活动计算", "恢复间隔被计入时长", "回合区间合并", "隔日恢复断言"),
    ("graph", "关系图", "src/ProjectGraphView.tsx", "无向边去重", "反向输入产生双边", "端点规范化", "反向去重断言"),
    ("cancel", "分析取消", "crates/core/src/analysis.rs", "迟到结果屏蔽", "取消后结果仍入库", "运行代次检查", "取消竞态断言"),
    ("navigation", "证据导航", "src/ThreadHistoryView.tsx", "条目定位", "点击证据定位到错误回合", "复合条目键", "跨回合定位断言"),
    ("facts", "文件事实", "crates/core/src/facts.rs", "结构化事实提取", "失败命令被记作成功", "执行结果映射", "失败状态断言"),
]


def stamp(topic_index: int, case_index: int, side: str) -> int:
    base = BASE_MS + topic_index * 86_400_000 + case_index * 3_600_000
    if case_index == 10:  # 时间矛盾：A 实际发生在 B 之后。
        return base + (1_200_000 if side == "A" else 0)
    if case_index == 3:  # 恢复：A 创建早，但其证据回合仍早于 B。
        return base + (0 if side == "A" else 1_200_000)
    return base + (0 if side == "A" else 1_200_000)


def cases(topic: tuple[str, ...]) -> list[tuple[str, str | None, str, str, str, str]]:
    slug, name, path, goal, problem, change, assertion = topic
    return [
        ("continues", "CONTINUES", f"{name}：已完成{goal}的入口，剩下异常路径未处理。", f"沿用前一会话的{goal}入口，补齐异常路径并记录结果。", "B 明确延续 A 留下的同一入口及未完成事项。", "A_TO_B"),
        ("implements", "IMPLEMENTS", f"{name}设计记录：建议用{change}落实{goal}，尚未修改代码。", f"依照此前提出的{change}方案实现{goal}；变更位于 {path}。", "A 提出具体方案，B 明确实施该方案。", "A_TO_B"),
        ("fixes", "FIXES", f"{name}回归：{problem}；复现命令退出码为 1。", f"修复了先前记录的“{problem}”；修改 {path} 后复现命令退出码为 0。", "B 针对 A 的同一故障给出修复和复现结果。", "A_TO_B"),
        ("validates_resumed", "VALIDATES", f"恢复旧会话后完成{change}，产物版本为 rev-{slug}-2。", f"对 rev-{slug}-2 运行{assertion}，结果通过；验证的是恢复回合的产物。", "A 的创建时间虽早，其证据来自恢复回合；B 验证该产物。", "A_TO_B"),
        ("investigates", "INVESTIGATES", f"{name}收到“{problem}”报告，根因尚不清楚。", f"调查“{problem}”：检查 {path} 的输入边界，定位触发条件。", "B 调查 A 报告的问题，尚无修复结论。", "A_TO_B"),
        ("supersedes", "SUPERSEDES", f"{name}先采用每次全量计算{goal}，作为第一版方案。", f"弃用此前全量计算方案，改以{change}完成{goal}；旧实现退出使用。", "B 明确替代 A 的方案，并说明旧方案停用。", "A_TO_B"),
        ("motivated_by", "MOTIVATED_BY", f"{name}的{assertion}暴露了{problem}，形成后续改造动机。", f"因为先前断言暴露{problem}，启动{change}的独立改造工作。", "A 的结果明确促成 B 的新工作。", "A_TO_B"),
        ("alternative", "ALTERNATIVE_TO", f"{name}方案甲：通过{change}实现{goal}，保留比较结果。", f"{name}方案乙：使用独立缓存层实现{goal}，与方案甲并列比较，未声明替代。", "双方针对同一目标探索互斥或可替代方案。", "UNDIRECTED"),
        ("related", "RELATED", f"{name}记录 {path} 中{goal}的接口约束，等待其他模块消费。", f"另一模块引用该接口约束调整调用方；目前无法判定是延续、实现或验证。", "双方有明确接口引用，但细分类别证据不足。", "UNDIRECTED"),
        ("shared_file_none", "NONE", f"在 {path} 修订{goal}的中文帮助文字；变更位于帮助段落。", f"在同一文件 {path} 调整日志级别常量，并更新日志快照。", "共享文件路径，但任务与改动部位不同，未见语义依赖。", "NONE"),
        ("time_contradiction", "NONE", f"在后续回合首次生成 rev-{slug}-late，作为{goal}结果。", f"将一次 rev-{slug}-1 的{assertion}运行记录归入 rev-{slug}-late 验证项。", "B 的证据时间早于 A 产物出现，且实际运行的是旧版；A→B 的验证主张不成立。", "NONE"),
        ("insufficient", None, f"{name}：记录了待讨论的{goal}方向，未保留决定或产物。", "仅存一条“检查完成”的状态输出，缺少目标、对象和结果定位。", "双方条目可定位，但没有足够材料支持关系或确定 NONE；保留待补证。", "UNKNOWN"),
    ]


def make_thread(pair_id: str, side: str, topic_index: int, case_index: int, output: str, project: str) -> dict:
    thread_id = f"synthetic-{pair_id}-{side.lower()}"
    resumed = case_index == 3 and side == "A"
    ordinal = 1 if resumed else 0
    turn_id = f"{thread_id}-turn-{ordinal + 1}"
    item_id = f"{thread_id}-item-{ordinal + 1}"
    when = stamp(topic_index, case_index, side)
    created = when - (7 * 86_400_000 if case_index == 3 and side == "A" else 0)
    version = hashlib.sha256(output.encode("utf-8")).hexdigest()
    command = f"cat /synthetic/{project}/{pair_id}/{side.lower()}/observation.txt"
    metadata = {
        "id": thread_id, "sessionId": thread_id, "title": f"{pair_id} {side}",
        "preview": output[:80], "cwd": f"/synthetic/{project}",
        "projectId": project, "sourceKind": "cli", "sourceDetail": None,
        "threadSource": "cli", "parentThreadId": None, "forkedFromId": None,
        "git": None, "createdAt": created, "updatedAt": when + 60_000,
        "archived": False, "metadataComplete": True, "turnsComplete": True,
        "itemsComplete": True, "missingFromSource": False, "contentComplete": True,
        "readError": None, "observedAtUnixMs": when + 120_000,
    }
    turn = {
        "threadId": thread_id, "id": turn_id, "ordinal": ordinal, "status": "completed",
        "startedAtUnixMs": when, "completedAtUnixMs": when + 60_000,
        "durationMs": 60_000, "timeError": None, "sourceUpdatedAt": when + 60_000,
        "contentVersion": version,
    }
    item = {
        "threadId": thread_id, "turnId": turn_id, "id": item_id, "ordinal": 0,
        "sourceType": "commandExecution", "supported": True, "text": None,
        "command": command, "cwd": f"/synthetic/{project}", "output": output,
        "exitCode": 0, "status": "completed", "changes": [],
        "sourceUpdatedAt": when + 60_000, "contentVersion": version,
    }
    turns = [turn]
    items = [item]
    if resumed:
        old_text = "初次探索已暂停；本回合未形成后续验证所需产物。"
        old_version = hashlib.sha256(old_text.encode("utf-8")).hexdigest()
        old_turn = {**turn, "id": f"{thread_id}-turn-1", "ordinal": 0,
                    "startedAtUnixMs": created, "completedAtUnixMs": created + 60_000,
                    "contentVersion": old_version}
        old_item = {**item, "turnId": old_turn["id"], "id": f"{thread_id}-item-1",
                    "command": command.replace("observation.txt", "initial.txt"),
                    "output": old_text, "contentVersion": old_version}
        turns.insert(0, old_turn)
        items.insert(0, old_item)
    if case_index == 9:
        path = TOPICS[topic_index][2]
        diff = ("@@ 帮助段落 @@\n+调整帮助文字" if side == "A" else
                "@@ 日志配置 @@\n+调整日志级别常量")
        change_version = hashlib.sha256(diff.encode("utf-8")).hexdigest()
        items.append({**item, "id": f"{thread_id}-item-2", "ordinal": 1,
                      "sourceType": "fileChange", "command": None, "output": None,
                      "changes": [{"path": path, "kind": "modified", "diff": diff}],
                      "contentVersion": change_version})
    return {"metadata": metadata, "turns": turns, "items": items}


def make_pointer(thread: dict) -> dict:
    item = next(item for item in thread["items"] if item["sourceType"] == "commandExecution"
                and item["output"] != "初次探索已暂停；本回合未形成后续验证所需产物。")
    return {
        "threadId": item["threadId"], "turnId": item["turnId"],
        "itemId": item["id"], "field": "output", "changeIndex": None,
        "excerpt": item["output"], "contentVersion": item["contentVersion"],
    }


def iso(ms: int) -> str:
    return datetime.fromtimestamp(ms / 1000, timezone.utc).strftime("%Y-%m-%d %H:%M UTC")


def build() -> tuple[dict, list[dict], str]:
    pairs = []
    reviews = []
    lines = ["# 会话关系样本逐对复核稿", "", f"样本版本：`{VERSION}`。全部为合成资料；以下期望标签由代理提出，尚未有人确认。", "", "审核每对的双方原文、时间、证据定位、类型和方向；在 `human_reviews.jsonl` 逐对填写裁决。", ""]
    for ti, topic in enumerate(TOPICS):
        slug, name = topic[:2]
        for ci, (scenario, expected_type, a_text, b_text, rationale, direction) in enumerate(cases(topic)):
            pair_id = f"RR-{ti + 1:02d}-{ci + 1:02d}"
            project = f"synthetic-{slug}"
            a = make_thread(pair_id, "A", ti, ci, a_text, project)
            b = make_thread(pair_id, "B", ti, ci, b_text, project)
            determinability = "INSUFFICIENT_EVIDENCE" if expected_type is None else "DECIDABLE"
            pair = {
                "id": pair_id, "projectId": project, "scenario": scenario,
                "language": "ZH_EN_TECH" if ti % 2 == 0 else "ZH",
                "threads": {"A": a, "B": b},
                "evidence": {"A": make_pointer(a), "B": make_pointer(b)},
                "proposedLabel": {"type": expected_type, "direction": direction,
                                  "determinability": determinability, "rationale": rationale},
            }
            pairs.append(pair)
            reviews.append({"pairId": pair_id, "annotationVersion": VERSION,
                            "status": "PENDING_HUMAN", "reviewer": None,
                            "reviewedAt": None, "finalType": None, "finalDirection": None,
                            "finalDeterminability": None, "notes": None})
            a_pointer = pair["evidence"]["A"]
            b_pointer = pair["evidence"]["B"]
            lines += [f"## {pair_id} · {name} · {scenario}", "",
                      f"- 代理建议：`{expected_type or '待判定'}`；方向 `{direction}`；可判定性 `{determinability}`。",
                      f"- A 证据：`{a_pointer['threadId']}/{a_pointer['turnId']}/{a_pointer['itemId']}.output`；创建于 {iso(a['metadata']['createdAt'])}；证据回合 {iso(stamp(ti, ci, 'A'))}；内容版本 `{a_pointer['contentVersion']}`。",
                      f"- A 摘录：{a_text}",
                      f"- B 证据：`{b_pointer['threadId']}/{b_pointer['turnId']}/{b_pointer['itemId']}.output`；创建于 {iso(b['metadata']['createdAt'])}；证据回合 {iso(stamp(ti, ci, 'B'))}；内容版本 `{b_pointer['contentVersion']}`。",
                      f"- B 摘录：{b_text}", f"- 判定依据：{rationale}",
                      "- 人工确认：待逐对复核；有争议时在记录中注明，不删除样本。", ""]
    dataset = {"schemaVersion": 1, "sampleVersion": VERSION,
               "annotationVersion": VERSION, "provenance": "synthetic",
               "humanConfirmation": "PENDING_HUMAN", "pairs": pairs}
    return dataset, reviews, "\n".join(lines)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    dataset, reviews, markdown = build()
    data_bytes = (json.dumps(dataset, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    (OUT / "pairs.json").write_bytes(data_bytes)
    (OUT / "human_reviews.jsonl").write_text(
        "".join(json.dumps(record, ensure_ascii=False) + "\n" for record in reviews), encoding="utf-8"
    )
    (OUT / "REVIEW.md").write_text(markdown, encoding="utf-8")
    manifest = {"sampleVersion": VERSION, "annotationVersion": VERSION,
                "pairsSha256": hashlib.sha256(data_bytes).hexdigest(),
                "pairCount": len(dataset["pairs"]), "status": "PENDING_HUMAN"}
    (OUT / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
