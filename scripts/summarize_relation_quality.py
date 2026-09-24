#!/usr/bin/env python3
"""只对完整、已冻结且实际运行的 #32 记录计算关系质量。"""

from __future__ import annotations

import argparse
import hashlib
import json
from collections import Counter, defaultdict
from pathlib import Path


def read(path: Path) -> bytes:
    return path.read_bytes()


def sha(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def require(ok: bool, message: str) -> None:
    if not ok:
        raise ValueError(message)


def freeze(dataset_dir: Path) -> tuple[dict, dict[str, dict], dict[str, dict]]:
    manifest = json.loads(read(dataset_dir / "manifest.json"))
    pair_raw = read(dataset_dir / "pairs.json")
    review_raw = read(dataset_dir / "human_reviews.jsonl")
    pairs = json.loads(pair_raw)
    reviews = [json.loads(line) for line in review_raw.splitlines() if line]
    require(manifest.get("status") == "FROZEN"
            and pairs.get("humanConfirmation") == "CONFIRMED"
            and bool(manifest.get("frozenAt")), "人工样本尚未冻结")
    require(manifest.get("pairsSha256") == sha(pair_raw)
            and manifest.get("humanReviewsSha256") == sha(review_raw), "冻结摘要不匹配")
    require(manifest.get("sampleVersion") == pairs.get("sampleVersion")
            and manifest.get("annotationVersion") == pairs.get("annotationVersion"), "样本版本不一致")
    pair_map = {pair["id"]: pair for pair in pairs["pairs"]}
    review_map = {row["pairId"]: row for row in reviews}
    require(len(pair_map) == len(review_map) == len(pairs["pairs"]) >= 100
            and set(pair_map) == set(review_map), "样本或人工记录未逐对覆盖")
    require(all(row["status"] == "CONFIRMED" and row.get("reviewer") and row.get("reviewedAt")
                for row in reviews), "存在未人工确认或有争议样本")
    return manifest, pair_map, review_map


def matches(relation: dict, gold: dict, row: dict) -> bool:
    if relation["kind"] != gold["finalType"]:
        return False
    if gold["finalDirection"] == "UNDIRECTED":
        return {relation["fromThreadId"], relation["toThreadId"]} == {row["threadA"], row["threadB"]}
    if gold["finalDirection"] == "A_TO_B":
        return relation["fromThreadId"] == row["threadA"] and relation["toThreadId"] == row["threadB"]
    if gold["finalDirection"] == "B_TO_A":
        return relation["fromThreadId"] == row["threadB"] and relation["toThreadId"] == row["threadA"]
    return False


def evidence_matches(relation: dict, pair: dict) -> bool:
    selected = relation["evidence"]
    for pointer in pair["evidence"].values():
        actual = next((selected[side] for side in ("left", "right")
                       if selected[side]["threadId"] == pointer["threadId"]), None)
        if actual is None or any(actual[key] != pointer[key] for key in
                                 ("turnId", "itemId", "field", "changeIndex", "contentVersion")):
            return False
        if pointer["excerpt"] not in actual["excerpt"] and actual["excerpt"] not in pointer["excerpt"]:
            return False
    return True


def summary(dataset_dir: Path, report_path: Path) -> dict:
    manifest, pairs, reviews = freeze(dataset_dir)
    report = json.loads(read(report_path))
    lock = report.get("lock", {})
    require(lock.get("dataset", {}).get("pairsSha256") == manifest["pairsSha256"]
            and lock.get("dataset", {}).get("humanReviewsSha256") == manifest["humanReviewsSha256"]
            and lock.get("evaluationScope") == "isolatedPair",
            "报告与冻结样本不一致")
    rows = report.get("pairs", {})
    require(set(rows) == set(pairs), "评测未覆盖全部冻结样本；不得计算质量结论")
    totals: Counter[str] = Counter()
    groups: dict[str, Counter[str]] = defaultdict(Counter)
    for pair_id, pair in pairs.items():
        row = rows[pair_id]
        gold = reviews[pair_id]
        require(row["scenario"] == pair["scenario"] and row["language"] == pair["language"],
                f"{pair_id}: 场景或语言不一致")
        require(row["gold"] == {"type": gold["finalType"], "direction": gold["finalDirection"],
                                 "determinability": gold["finalDeterminability"]},
                f"{pair_id}: 人工标签与报告不一致")
        require(row["threadA"] == pair["threads"]["A"]["metadata"]["id"]
                and row["threadB"] == pair["threads"]["B"]["metadata"]["id"],
                f"{pair_id}: 来源会话不一致")
        counters = [totals, groups[f"scenario:{pair['scenario']}"], groups[f"language:{pair['language']}"]]
        for count in counters:
            count["pairs"] += 1
            if gold["finalDeterminability"] == "INSUFFICIENT_EVIDENCE":
                count["goldInsufficient"] += 1
            elif gold["finalType"] == "NONE":
                count["goldNone"] += 1
            else:
                count["goldPositive"] += 1
        if row.get("candidateMiss"):
            require(row.get("candidate") is None and row.get("run") is None,
                    f"{pair_id}: 候选漏检记录与分析结果矛盾")
            for count in counters:
                count["candidateMiss"] += 1
            continue
        candidate = row.get("candidate") or {}
        require({candidate.get("leftThreadId"), candidate.get("rightThreadId")} ==
                {row["threadA"], row["threadB"]}
                and bool(row.get("candidateInputVersion")), f"{pair_id}: 候选来源不一致")
        run = row.get("run") or {}
        outcome = row.get("outcome")
        require(run.get("state") == "complete" and outcome is not None,
                f"{pair_id}: 运行未完整成功；保留单项错误，不计算质量结论")
        require(run.get("relationOnly") is True and run.get("jevModel") == lock["requestedModel"]
                and run.get("jevBaseUrl") == lock["baseUrl"]
                and run.get("jevRulesVersion") == lock["rulesVersion"],
                f"{pair_id}: 请求配置不一致")
        require(outcome["candidateId"] == candidate["id"]
                and outcome["inputVersion"] == row["candidateInputVersion"],
                f"{pair_id}: 结果与候选版本不一致")
        identity = outcome.get("jevIdentity") or {}
        require(identity.get("requestedModel") == lock["requestedModel"]
                and identity.get("actualModel") == lock["requestedModel"]
                and identity.get("rulesVersion") == lock["rulesVersion"],
                f"{pair_id}: 实际模型版本或题目版本不一致")
        unit = run["units"][0]
        require(unit.get("relationClassification") is not None,
                f"{pair_id}: 缺少第一阶段 Choice 记录")
        require(unit["relationClassification"]["actualModel"] == lock["requestedModel"],
                f"{pair_id}: 分类响应模型版本不一致")
        selected = unit.get("relationEvidenceSelection")
        if any(choice["judgment"] == "supported" for choice in outcome["decisions"]):
            require(selected is not None, f"{pair_id}: 缺少第二阶段证据 Choice 记录")
            require(selected["actualModel"] == lock["requestedModel"],
                    f"{pair_id}: 证据响应模型版本不一致")
        checks = {item["relationId"]: item["evidenceValid"] for item in row["mechanicalEvidence"]}
        relations = outcome["relations"]
        require(all(checks.get(relation["id"]) is True for relation in relations),
                f"{pair_id}: 有效推断关系未通过机械证据检查")
        displayed = [relation for relation in relations if relation["confidence"] >= 0.70]
        type_direction_matches = [relation for relation in displayed if matches(relation, gold, row)]
        accepted = [relation for relation in type_direction_matches if evidence_matches(relation, pair)]
        for count in counters:
            count["modelCalls"] += run["totalCalls"]
            count["displayedEdges"] += len(displayed)
            count["typeDirectionMatchedEdges"] += len(type_direction_matches)
            count["acceptedEdges"] += len(accepted)
            count["recalledPositive"] += int(bool(accepted) and gold["finalDeterminability"] == "DECIDABLE"
                                                   and gold["finalType"] != "NONE")
            count["none"] += int(outcome["status"] == "none")
            count["undetermined"] += int(outcome["status"] == "undetermined")
            count["unknownPairs"] += int(outcome["unknownCount"] > 0)
            count["unknownChoices"] += outcome["unknownCount"]
            count["insufficientEvidence"] += int(outcome["status"] == "insufficientEvidence"
                                                or selected is not None and any(choice["answer"]["choice"] == "INSUFFICIENT"
                                                                                for choice in selected["choices"]))
            count["lowScore"] += int(bool(relations) and not displayed)
            count["shownPositivePairs"] += int(bool(displayed))
    precision = (totals["acceptedEdges"] / totals["displayedEdges"]
                 if totals["displayedEdges"] else None)
    recall = (totals["recalledPositive"] / totals["goldPositive"]
              if totals["goldPositive"] else None)
    positive_scenarios = {name: count for name, count in groups.items()
                          if name.startswith("scenario:") and count["goldPositive"]}
    coverage = all(count["recalledPositive"] > 0 for count in positive_scenarios.values())
    all_keys = ("pairs", "goldInsufficient", "goldNone", "goldPositive", "candidateMiss",
                "modelCalls", "displayedEdges", "typeDirectionMatchedEdges", "acceptedEdges", "recalledPositive",
                "none", "undetermined", "unknownPairs", "unknownChoices",
                "insufficientEvidence", "lowScore", "shownPositivePairs")
    normalized = lambda count: {key: count[key] for key in all_keys}
    def group_summary(count: Counter[str]) -> dict:
        return {**normalized(count),
                "humanAcceptedPrecision": (count["acceptedEdges"] / count["displayedEdges"]
                                           if count["displayedEdges"] else None),
                "decidablePositiveRecall": (count["recalledPositive"] / count["goldPositive"]
                                            if count["goldPositive"] else None)}
    return {
        "sampleVersion": manifest["sampleVersion"],
        "annotationVersion": manifest["annotationVersion"],
        "frozenAt": manifest["frozenAt"],
        "requestedModel": lock["requestedModel"],
        "actualModel": lock["requestedModel"],
        "baseUrl": lock["baseUrl"],
        "rulesVersion": lock["rulesVersion"],
        "evaluationScope": lock["evaluationScope"],
        "candidateRulesVersion": lock["candidateRulesVersion"],
        "factRulesVersion": lock["factRulesVersion"],
        "defaultConfidenceThreshold": 0.70,
        "total": normalized(totals),
        "byGroup": {name: group_summary(count) for name, count in sorted(groups.items())},
        "outcomeRates": {key: totals[key] / totals["pairs"] for key in
                         ("none", "unknownPairs", "undetermined", "insufficientEvidence", "lowScore", "candidateMiss")},
        "humanAcceptedPrecision": {"numerator": totals["acceptedEdges"], "denominator": totals["displayedEdges"], "value": precision},
        "decidablePositiveRecall": {"numerator": totals["recalledPositive"], "denominator": totals["goldPositive"], "value": recall},
        "corePositiveScenarioCoverage": coverage,
        "meetsReleaseGate": precision is not None and precision >= 0.90
                            and recall is not None and recall >= 0.60 and coverage,
        "limitation": "冻结合成样本的单次结果不代表全部实际项目。",
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dataset", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = summary(args.dataset, args.report)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"已生成质量汇总：{args.output}；门槛结果：{result['meetsReleaseGate']}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, KeyError, TypeError, ValueError) as error:
        raise SystemExit(f"拒绝生成质量结论：{error}") from error
