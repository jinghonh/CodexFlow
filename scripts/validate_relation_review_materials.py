#!/usr/bin/env python3
"""机械检查 #31 合成草案的数量、格式、定位、覆盖和人工状态。"""

from __future__ import annotations

import hashlib
import json
from collections import Counter
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DIR = ROOT / "data" / "relation-review" / "v0.1-draft"
TYPES = {"CONTINUES", "IMPLEMENTS", "FIXES", "VALIDATES", "INVESTIGATES",
         "ALTERNATIVE_TO", "SUPERSEDES", "MOTIVATED_BY", "RELATED", "NONE"}
SCENARIOS = {"continues", "implements", "fixes", "validates_resumed", "investigates",
             "supersedes", "motivated_by", "alternative", "related", "shared_file_none",
             "time_contradiction", "insufficient"}
META_FIELDS = {"id", "sessionId", "title", "preview", "cwd", "projectId", "sourceKind",
               "sourceDetail", "threadSource", "parentThreadId", "forkedFromId", "git",
               "createdAt", "updatedAt", "archived", "metadataComplete", "turnsComplete",
               "itemsComplete", "missingFromSource", "contentComplete", "readError",
               "observedAtUnixMs"}
TURN_FIELDS = {"threadId", "id", "ordinal", "status", "startedAtUnixMs",
               "completedAtUnixMs", "durationMs", "timeError", "sourceUpdatedAt", "contentVersion"}
ITEM_FIELDS = {"threadId", "turnId", "id", "ordinal", "sourceType", "supported", "text",
               "command", "cwd", "output", "exitCode", "status", "changes",
               "sourceUpdatedAt", "contentVersion"}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def check_pointer(pair: dict, side: str) -> None:
    thread = pair["threads"][side]
    meta = thread["metadata"]
    pointer = pair["evidence"][side]
    require(set(meta) == META_FIELDS, f"{pair['id']} {side}: 会话元数据格式错误")
    require(all(set(t) == TURN_FIELDS for t in thread["turns"]), f"{pair['id']} {side}: 回合格式错误")
    require(all(set(i) == ITEM_FIELDS for i in thread["items"]), f"{pair['id']} {side}: 条目格式错误")
    require(len({t["id"] for t in thread["turns"]}) == len(thread["turns"]),
            f"{pair['id']} {side}: 回合标识重复")
    require(len({(i["turnId"], i["id"]) for i in thread["items"]}) == len(thread["items"]),
            f"{pair['id']} {side}: 条目复合标识重复")
    require(all(i["turnId"] in {t["id"] for t in thread["turns"]} for i in thread["items"]),
            f"{pair['id']} {side}: 条目引用不存在的回合")
    require(all(set(change) == {"path", "kind", "diff"} for i in thread["items"]
                for change in i["changes"]), f"{pair['id']} {side}: 文件变更格式错误")
    require(meta["projectId"] == pair["projectId"], f"{pair['id']} {side}: 项目不匹配")
    require(meta["contentComplete"] and meta["itemsComplete"], f"{pair['id']} {side}: 内容不完整")
    require(pointer["threadId"] == meta["id"], f"{pair['id']} {side}: 会话不匹配")
    turn = next((t for t in thread["turns"] if t["id"] == pointer["turnId"]), None)
    require(turn is not None and turn["threadId"] == meta["id"], f"{pair['id']} {side}: 回合不存在")
    item = next((i for i in thread["items"] if i["id"] == pointer["itemId"]
                 and i["turnId"] == pointer["turnId"]), None)
    require(item is not None and item["threadId"] == meta["id"], f"{pair['id']} {side}: 条目不存在或层级错误")
    require(item["contentVersion"] == pointer["contentVersion"], f"{pair['id']} {side}: 内容版本失效")
    require(item["sourceUpdatedAt"] == meta["updatedAt"], f"{pair['id']} {side}: 来源版本失效")
    field = pointer["field"]
    require(field in {"command", "output", "changePath", "changeDiff"}, f"{pair['id']} {side}: 字段错误")
    if field in {"command", "output"}:
        require(pointer["changeIndex"] is None, f"{pair['id']} {side}: 多余变更序号")
        source = item[field]
    else:
        index = pointer["changeIndex"]
        require(isinstance(index, int) and 0 <= index < len(item["changes"]), f"{pair['id']} {side}: 变更序号错误")
        source = item["changes"][index]["path" if field == "changePath" else "diff"]
    require(isinstance(source, str) and pointer["excerpt"] and pointer["excerpt"] in source,
            f"{pair['id']} {side}: 摘录无法定位")
    require(turn["startedAtUnixMs"] is not None and turn["completedAtUnixMs"] is not None,
            f"{pair['id']} {side}: 回合时间缺失")


def evidence_time(pair: dict, side: str) -> int:
    turn_id = pair["evidence"][side]["turnId"]
    return next(turn["startedAtUnixMs"] for turn in pair["threads"][side]["turns"]
                if turn["id"] == turn_id)


def main() -> None:
    raw = (DIR / "pairs.json").read_bytes()
    dataset = json.loads(raw)
    manifest = json.loads((DIR / "manifest.json").read_text(encoding="utf-8"))
    reviews = [json.loads(line) for line in (DIR / "human_reviews.jsonl").read_text(encoding="utf-8").splitlines()]
    markdown = (DIR / "REVIEW.md").read_text(encoding="utf-8")
    require(dataset["schemaVersion"] == 1 and dataset["provenance"] == "synthetic", "样本格式或来源错误")
    require(dataset["sampleVersion"] == manifest["sampleVersion"], "样本版本不一致")
    require(dataset["annotationVersion"] == manifest["annotationVersion"], "标注版本不一致")
    require(manifest["pairsSha256"] == hashlib.sha256(raw).hexdigest(), "样本摘要不匹配")
    pairs = dataset["pairs"]
    require(len(pairs) >= 100 and manifest["pairCount"] == len(pairs), "样本对数不足或清单错误")
    ids = [p["id"] for p in pairs]
    require(len(set(ids)) == len(ids), "重复样本编号")
    require(len(reviews) == len(pairs) and {r["pairId"] for r in reviews} == set(ids), "人工记录未逐对覆盖")
    counts = Counter(p["scenario"] for p in pairs)
    require(set(counts) == SCENARIOS and min(counts.values()) >= 8, "场景覆盖不足")
    require({p["language"] for p in pairs} == {"ZH", "ZH_EN_TECH"}, "语言覆盖不足")
    type_counts = Counter(p["proposedLabel"]["type"] for p in pairs)
    require(set(type_counts) == TYPES | {None}, "关系类型覆盖不足")
    for pair in pairs:
        pid = pair["id"]
        require(f"## {pid} ·" in markdown, f"{pid}: 复核稿缺失")
        require(set(pair["threads"]) == {"A", "B"} and set(pair["evidence"]) == {"A", "B"}, f"{pid}: 双方缺失")
        check_pointer(pair, "A")
        check_pointer(pair, "B")
        a = pair["threads"]["A"]["metadata"]["id"]
        b = pair["threads"]["B"]["metadata"]["id"]
        require(a != b, f"{pid}: 自环")
        label = pair["proposedLabel"]
        require(bool(label["rationale"]), f"{pid}: 判定依据缺失")
        if label["type"] is None:
            require(label["direction"] == "UNKNOWN" and label["determinability"] == "INSUFFICIENT_EVIDENCE",
                    f"{pid}: 证据不足被伪装为负例")
        elif label["type"] == "NONE":
            require(label["direction"] == "NONE" and label["determinability"] == "DECIDABLE",
                    f"{pid}: NONE 标签不一致")
        elif label["type"] in {"ALTERNATIVE_TO", "RELATED"}:
            require(label["direction"] == "UNDIRECTED" and label["determinability"] == "DECIDABLE",
                    f"{pid}: 无向关系方向错误")
        else:
            require(label["direction"] == "A_TO_B" and label["determinability"] == "DECIDABLE",
                    f"{pid}: 因果方向错误")
        a_time = evidence_time(pair, "A")
        b_time = evidence_time(pair, "B")
        if pair["scenario"] == "time_contradiction":
            require(a_time > b_time, f"{pid}: 时间矛盾未建立")
        elif label["direction"] == "A_TO_B":
            require(a_time < b_time, f"{pid}: 因果证据顺序错误")
        if pair["scenario"] == "validates_resumed":
            require(pair["threads"]["A"]["metadata"]["createdAt"] < b_time - 86_400_000,
                    f"{pid}: 缺少恢复间隔")
            require(len(pair["threads"]["A"]["turns"]) == 2 and
                    pair["threads"]["A"]["turns"][0]["startedAtUnixMs"] < a_time - 86_400_000,
                    f"{pid}: 缺少恢复前回合")
        if pair["scenario"] == "shared_file_none":
            paths = [set(change["path"] for item in pair["threads"][side]["items"]
                         for change in item["changes"]) for side in ("A", "B")]
            require(bool(paths[0] & paths[1]), f"{pid}: 未共享文件")
    review_counts = Counter(r["status"] for r in reviews)
    require(set(review_counts) <= {"PENDING_HUMAN", "CONFIRMED", "DISPUTED"}, "人工状态非法")
    for review in reviews:
        require(review["annotationVersion"] == manifest["annotationVersion"],
                f"{review['pairId']}: 标注版本不一致")
        if review["status"] == "PENDING_HUMAN":
            require(all(review[key] is None for key in
                        ("reviewer", "reviewedAt", "finalType", "finalDirection", "finalDeterminability", "notes")),
                    f"{review['pairId']}: 待复核记录不得冒充人工确认")
        else:
            require(bool(review["reviewer"]) and bool(review["reviewedAt"]),
                    f"{review['pairId']}: 人工身份或时间缺失")
            if review["status"] == "DISPUTED":
                require(bool(review["notes"]), f"{review['pairId']}: 争议处理说明缺失")
            else:
                require(review["finalDeterminability"] in {"DECIDABLE", "INSUFFICIENT_EVIDENCE"},
                        f"{review['pairId']}: 最终可判定性错误")
                if review["finalDeterminability"] == "INSUFFICIENT_EVIDENCE":
                    require(review["finalType"] is None and review["finalDirection"] == "UNKNOWN",
                            f"{review['pairId']}: 证据不足不得作为确定负例")
                else:
                    require(review["finalType"] in TYPES, f"{review['pairId']}: 最终类型非法")
                    allowed = ({"NONE"} if review["finalType"] == "NONE" else
                               {"UNDIRECTED"} if review["finalType"] in {"ALTERNATIVE_TO", "RELATED"}
                               else {"A_TO_B", "B_TO_A"})
                    require(review["finalDirection"] in allowed, f"{review['pairId']}: 最终方向与类型不一致")
    require(dataset["humanConfirmation"] == manifest["status"] == "PENDING_HUMAN",
            "草案不得声明人工确认或冻结")
    print(f"通过：{len(pairs)} 对；{len(counts)} 类场景；{len(type_counts)} 种标签状态；{len(pairs) * 2} 个定位指针；人工确认 {review_counts['CONFIRMED']} 对。")
    print("场景：" + json.dumps(dict(sorted(counts.items())), ensure_ascii=False))


if __name__ == "__main__":
    main()
