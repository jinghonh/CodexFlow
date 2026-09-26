//! 手动运行冻结关系样本；只复用核心关系分析，不启动 Codex 会话。
use codexflow_core::{
    SourceService, EVALUATION_CANDIDATE_RULES_VERSION, EVALUATION_FACT_RULES_VERSION,
};
use codexflow_domain::{
    AnalysisLimits, AnalysisRunState, HistoryCoverage, HistoryItem, HistoryReadPath,
    HistorySnapshot, HistoryTurn, LocalProject, ThreadAttribution, ThreadMetadata,
};
use codexflow_jev::RELATION_RULES_VERSION;
use codexflow_store::{PreferenceStore, SessionStore};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Dataset {
    schema_version: u32,
    sample_version: String,
    annotation_version: String,
    human_confirmation: String,
    pairs: Vec<Pair>,
}

#[derive(Deserialize)]
struct Pair {
    id: String,
    #[serde(rename = "projectId")]
    project_id: String,
    scenario: String,
    language: String,
    threads: BTreeMap<String, SampleThread>,
    evidence: BTreeMap<String, SamplePointer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SamplePointer {
    thread_id: String,
    turn_id: String,
    item_id: String,
    field: String,
    change_index: Option<usize>,
    excerpt: String,
    content_version: String,
}

#[derive(Deserialize)]
struct SampleThread {
    metadata: ThreadMetadata,
    turns: Vec<HistoryTurn>,
    items: Vec<HistoryItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Review {
    pair_id: String,
    annotation_version: String,
    status: String,
    reviewer: Option<String>,
    reviewed_at: Option<String>,
    final_type: Option<String>,
    final_direction: Option<String>,
    final_determinability: Option<String>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn frozen_dataset(dir: &Path) -> Result<(Dataset, BTreeMap<String, Review>, Value), String> {
    let manifest = read_json(&dir.join("manifest.json"))?;
    let pairs_bytes = fs::read(dir.join("pairs.json")).map_err(|e| e.to_string())?;
    let reviews_bytes = fs::read(dir.join("human_reviews.jsonl")).map_err(|e| e.to_string())?;
    let dataset: Dataset = serde_json::from_slice(&pairs_bytes).map_err(|e| e.to_string())?;
    let manifest_string = |key: &str| manifest[key].as_str().unwrap_or("");
    if manifest_string("status") != "FROZEN"
        || dataset.human_confirmation != "CONFIRMED"
        || manifest_string("frozenAt").is_empty()
        || manifest_string("sampleVersion") != dataset.sample_version
        || manifest_string("annotationVersion") != dataset.annotation_version
        || manifest_string("pairsSha256") != digest(&pairs_bytes)
        || manifest_string("humanReviewsSha256") != digest(&reviews_bytes)
        || dataset.schema_version != 1
        || manifest["pairCount"].as_u64() != Some(dataset.pairs.len() as u64)
        || dataset.pairs.len() < 100
    {
        return Err(
            "仅接受至少 100 对、全部人工确认且摘要匹配的冻结样本；当前草案不可评测。".into(),
        );
    }
    let mut reviews = BTreeMap::new();
    for line in reviews_bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let review: Review = serde_json::from_slice(line).map_err(|e| e.to_string())?;
        if review.annotation_version != dataset.annotation_version
            || review.status != "CONFIRMED"
            || review.reviewer.as_deref().unwrap_or("").is_empty()
            || review.reviewed_at.as_deref().unwrap_or("").is_empty()
        {
            return Err(format!("{}: 缺少明确的人工确认。", review.pair_id));
        }
        let valid = match (
            review.final_determinability.as_deref(),
            review.final_type.as_deref(),
            review.final_direction.as_deref(),
        ) {
            (Some("INSUFFICIENT_EVIDENCE"), None, Some("UNKNOWN")) => true,
            (Some("DECIDABLE"), Some("NONE"), Some("NONE")) => true,
            (Some("DECIDABLE"), Some("ALTERNATIVE_TO" | "RELATED"), Some("UNDIRECTED")) => true,
            (
                Some("DECIDABLE"),
                Some(
                    "CONTINUES" | "IMPLEMENTS" | "FIXES" | "VALIDATES" | "INVESTIGATES"
                    | "SUPERSEDES" | "MOTIVATED_BY",
                ),
                Some("A_TO_B" | "B_TO_A"),
            ) => true,
            _ => false,
        };
        if !valid || reviews.insert(review.pair_id.clone(), review).is_some() {
            return Err("人工裁决重复或类型、方向不一致。".into());
        }
    }
    let ids: BTreeSet<_> = dataset.pairs.iter().map(|pair| pair.id.as_str()).collect();
    if ids.len() != dataset.pairs.len()
        || reviews.len() != ids.len()
        || !ids.iter().all(|id| reviews.contains_key(*id))
        || dataset.pairs.iter().any(|pair| {
            pair.threads.len() != 2
                || !pair.threads.contains_key("A")
                || !pair.threads.contains_key("B")
        })
    {
        return Err("冻结样本编号、双侧会话或人工记录不完整。".into());
    }
    for pair in &dataset.pairs {
        for side in ["A", "B"] {
            let sample = &pair.threads[side];
            let pointer = pair
                .evidence
                .get(side)
                .ok_or_else(|| format!("{}: 缺少 {side} 证据。", pair.id))?;
            let turn_exists = sample
                .turns
                .iter()
                .any(|turn| turn.id == pointer.turn_id && turn.thread_id == sample.metadata.id);
            let item = sample.items.iter().find(|item| {
                item.id == pointer.item_id
                    && item.turn_id == pointer.turn_id
                    && item.thread_id == sample.metadata.id
            });
            let text = item.and_then(|item| match pointer.field.as_str() {
                "text" => item.text.as_deref(),
                "command" => item.command.as_deref(),
                "output" => item.output.as_deref(),
                "changePath" => pointer
                    .change_index
                    .and_then(|index| item.changes.get(index))
                    .map(|change| change.path.as_str()),
                "changeDiff" => pointer
                    .change_index
                    .and_then(|index| item.changes.get(index))
                    .map(|change| change.diff.as_str()),
                _ => None,
            });
            if !turn_exists
                || pointer.thread_id != sample.metadata.id
                || sample.metadata.project_id.as_deref() != Some(pair.project_id.as_str())
                || item.is_none_or(|item| {
                    item.content_version != pointer.content_version
                        || item.source_updated_at != sample.metadata.updated_at
                })
                || pointer.excerpt.is_empty()
                || !text.is_some_and(|text| text.contains(&pointer.excerpt))
            {
                return Err(format!("{}: {side} 证据定位或来源版本无效。", pair.id));
            }
        }
    }
    let pair_count = dataset.pairs.len();
    Ok((
        dataset,
        reviews,
        json!({
            "sampleVersion": manifest_string("sampleVersion"),
            "annotationVersion": manifest_string("annotationVersion"),
            "frozenAt": manifest_string("frozenAt"),
            "pairsSha256": digest(&pairs_bytes),
            "humanReviewsSha256": digest(&reviews_bytes),
            "pairCount": pair_count,
        }),
    ))
}

fn project_id(id: &str) -> String {
    format!("evaluation:{id}")
}

fn import_samples(store: &SessionStore, pairs: &[Pair], root: &Path) -> Result<(), String> {
    let mut threads = Vec::new();
    let mut projects = Vec::new();
    let mut attributions = Vec::new();
    for pair in pairs {
        let project = project_id(&pair.id);
        projects.push(LocalProject {
            id: project.clone(),
            name: pair.id.clone(),
            root: root.to_string_lossy().into_owned(),
            git_common_dir: None,
        });
        for sample in pair.threads.values() {
            threads.push(sample.metadata.clone());
            attributions.push(ThreadAttribution {
                thread_id: sample.metadata.id.clone(),
                project_id: Some(project.clone()),
                workspace_root: Some(root.to_string_lossy().into_owned()),
                basis: "frozenEvaluationSample".into(),
                detail: "冻结样本独立会话对".into(),
                diagnostic: None,
                source_project_id: sample.metadata.project_id.clone(),
            });
        }
    }
    store
        .save_collection(&threads, &[])
        .map_err(|e| e.message)?;
    store
        .save_projects_and_attributions(&projects, &attributions, None)
        .map_err(|e| e.message)?;
    for pair in pairs {
        for sample in pair.threads.values() {
            if store
                .history_coverage(&sample.metadata.id)
                .map_err(|e| e.message)?
                .is_some()
            {
                continue;
            }
            store
                .save_history(&HistorySnapshot {
                    coverage: HistoryCoverage {
                        thread_id: sample.metadata.id.clone(),
                        source_updated_at: sample.metadata.updated_at,
                        attempted_at_unix_ms: sample.metadata.observed_at_unix_ms,
                        path: HistoryReadPath::FullRead,
                        turns_complete: true,
                        items_complete: true,
                        turn_pages: 1,
                        item_pages: 1,
                        loaded_turns: sample.turns.len() as u64,
                        loaded_items: sample.items.len() as u64,
                        incompatible: false,
                        error: None,
                    },
                    turns: sample.turns.clone(),
                    items: sample.items.clone(),
                })
                .map_err(|e| e.message)?;
        }
    }
    Ok(())
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let temp = path.with_extension("json.tmp");
    fs::write(
        &temp,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::rename(temp, path).map_err(|e| e.to_string())
}

fn arg(name: &str, args: &[String]) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == name)
        .map(|window| window[1].clone())
}

fn total_calls(service: &SourceService, pairs: &[Pair]) -> Result<u32, String> {
    pairs.iter().try_fold(0u32, |total, pair| {
        let calls = service
            .latest_analysis_run(&project_id(&pair.id))
            .map_err(|error| error.message)?
            .map_or(0, |run| run.total_calls);
        total
            .checked_add(calls)
            .ok_or_else(|| "调用计数溢出；已停止评测。".into())
    })
}

async fn execute() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    let dataset_dir = PathBuf::from(arg("--dataset", &args).ok_or("缺少 --dataset")?);
    let (dataset, reviews, lock) = frozen_dataset(&dataset_dir)?;
    if args.iter().any(|item| item == "--check") {
        println!(
            "冻结样本检查通过：{} 对；未发起模型调用。",
            dataset.pairs.len()
        );
        return Ok(());
    }
    let resume = args.iter().any(|item| item == "--resume");
    if !resume && !args.iter().any(|item| item == "--run") {
        return Err("必须显式指定 --run 或 --resume；检查模式为 --check。".into());
    }
    let app_dir = PathBuf::from(arg("--app-data-dir", &args).ok_or("缺少 --app-data-dir")?);
    let output = PathBuf::from(arg("--output", &args).ok_or("缺少 --output")?);
    let limit: u32 = arg("--call-limit", &args)
        .ok_or("缺少 --call-limit")?
        .parse()
        .map_err(|_| "调用上限必须为正整数")?;
    if limit == 0 {
        return Err("调用上限必须为正整数".into());
    }
    let source_config = PreferenceStore::new(app_dir)
        .load_current()
        .map_err(|e| e.message)?;
    let lock = json!({"dataset":lock,"evaluationScope":"isolatedPair","baseUrl":source_config.jev.base_url,
        "requestedModel":source_config.jev.model,"rulesVersion":RELATION_RULES_VERSION,
        "candidateRulesVersion":EVALUATION_CANDIDATE_RULES_VERSION,
        "factRulesVersion":EVALUATION_FACT_RULES_VERSION});
    let workspace = output.join("workspace");
    if resume {
        if read_json(&output.join("lock.json"))? != lock {
            return Err("样本、模型、地址或题目版本已变化；不能混入旧评测。".into());
        }
    } else {
        if output.exists() {
            return Err("新评测的输出目录已存在；请另选路径或显式 --resume。".into());
        }
        fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
        write_json(&output.join("lock.json"), &lock)?;
        write_json(
            &output.join("report.json"),
            &json!({
                "schemaVersion":1,"lock":lock,"pairs":{},"qualityConclusion":null
            }),
        )?;
    }
    let store = SessionStore::new(workspace.clone()).map_err(|e| e.message)?;
    if !resume {
        PreferenceStore::new(workspace.clone())
            .save(&source_config)
            .map_err(|e| e.message)?;
    }
    let service = Arc::new(SourceService::new(workspace.clone()).map_err(|e| e.message)?);
    import_samples(&store, &dataset.pairs, &workspace)?;
    let status = service.jev_status().await.map_err(|e| e.message)?;
    if let Some(error) = status.credential_error {
        return Err(error.message);
    }
    if !status.credential_configured {
        return Err("当前应用 Jev 配置尚无匹配的系统凭据；未发起模型调用。".into());
    }
    let mut report = read_json(&output.join("report.json"))?;
    let baseline = total_calls(&service, &dataset.pairs)?;
    for pair in &dataset.pairs {
        let spent = total_calls(&service, &dataset.pairs)?
            .checked_sub(baseline)
            .ok_or("调用计数倒退；已停止评测。")?;
        if spent >= limit {
            break;
        }
        let project = project_id(&pair.id);
        let preview = service.candidate_preview(&project).map_err(|e| e.message)?;
        let left = &pair.threads["A"].metadata.id;
        let right = &pair.threads["B"].metadata.id;
        let candidate = preview.candidates.iter().find(|item| {
            (item.left_thread_id == *left && item.right_thread_id == *right)
                || (item.left_thread_id == *right && item.right_thread_id == *left)
        });
        let review = &reviews[&pair.id];
        let mut row = json!({"pairId":pair.id,"scenario":pair.scenario,"language":pair.language,
            "threadA":left,"threadB":right,
            "gold":{"type":review.final_type,"direction":review.final_direction,"determinability":review.final_determinability},
            "candidate":candidate,"candidateInputVersion":candidate.and_then(|item| preview.candidate_versions.get(&item.id)),
            "candidateRulesInputVersion":preview.input_version});
        if let Some(candidate) = candidate {
            let remaining = limit - spent;
            let existing = service
                .latest_analysis_run(&project)
                .map_err(|e| e.message)?;
            let run = match existing {
                Some(run) if run.relation_only && run.state == AnalysisRunState::Complete => run,
                Some(run) if run.relation_only => service
                    .continue_analysis_run(&run.id, remaining, |_| {})
                    .await
                    .map_err(|e| e.message)?,
                Some(_) => return Err("评测工作目录中存在非评测运行。".into()),
                None => service
                    .start_relation_evaluation(
                        project.clone(),
                        candidate.id.clone(),
                        AnalysisLimits {
                            call_limit: remaining,
                            ..Default::default()
                        },
                        |_| {},
                    )
                    .await
                    .map_err(|e| e.message)?,
            };
            let mut current = run;
            while matches!(
                current.state,
                AnalysisRunState::Queued | AnalysisRunState::Running | AnalysisRunState::Cancelling
            ) {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        service.cancel_analysis_run(&current.id).await.map_err(|e| e.message)?;
                        loop {
                            current = service.analysis_run(&current.id).map_err(|e| e.message)?.ok_or("评测运行丢失")?;
                            if current.state == AnalysisRunState::Cancelled { break; }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        current = service.analysis_run(&current.id).map_err(|e| e.message)?.ok_or("评测运行丢失")?;
                    }
                }
            }
            let graph = service.project_graph(&project).map_err(|e| e.message)?;
            row["run"] = serde_json::to_value(&current).map_err(|e| e.to_string())?;
            row["outcome"] = serde_json::to_value(
                graph
                    .inference_outcomes
                    .iter()
                    .find(|item| item.candidate_id == candidate.id),
            )
            .map_err(|e| e.to_string())?;
            row["mechanicalEvidence"] = serde_json::to_value(graph.reviewed_relations.iter().map(|item| json!({"relationId":item.relation.id,"evidenceValid":item.evidence_valid,"staleReason":item.stale_reason})).collect::<Vec<_>>()).map_err(|e| e.to_string())?;
            report["pairs"][&pair.id] = row;
            write_json(&output.join("report.json"), &report)?;
            if matches!(
                current.state,
                AnalysisRunState::Paused | AnalysisRunState::Cancelled
            ) {
                break;
            }
        } else {
            row["candidateMiss"] = json!(true);
            report["pairs"][&pair.id] = row;
            write_json(&output.join("report.json"), &report)?;
        }
    }
    println!(
        "评测记录：{}；本次最多 {} 次真实调用。质量指标须通过独立汇总脚本计算。",
        output.join("report.json").display(),
        limit
    );
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = execute().await {
        eprintln!("评测未完成：{error}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_shape_imports_into_core_candidate_path_without_model_calls() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dataset: Dataset = serde_json::from_slice(
            &fs::read(root.join("../../data/relation-review/v0.1-draft/pairs.json")).unwrap(),
        )
        .unwrap();
        let pair = &dataset.pairs[0];
        let path = env::temp_dir().join(format!(
            "codexflow-relation-review-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let service = SourceService::new(path.clone()).unwrap();
        let store = SessionStore::new(path.clone()).unwrap();
        import_samples(&store, std::slice::from_ref(pair), &path).unwrap();
        let preview = service.candidate_preview(&project_id(&pair.id)).unwrap();
        assert_eq!(preview.candidates.len(), 1);
        assert!(!preview.candidates[0].evidence.pairs.is_empty());
        assert!(preview.candidates[0]
            .evidence
            .pairs
            .iter()
            .any(|candidate| {
                ["A", "B"].iter().all(|side| {
                    let pointer = &pair.evidence[*side];
                    [&candidate.left, &candidate.right].iter().any(|actual| {
                        actual.thread_id == pointer.thread_id
                            && actual.item_id == pointer.item_id
                            && actual.field == codexflow_domain::EvidenceField::Output
                    })
                })
            }));
        drop(service);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn pending_draft_is_rejected_before_credentials_or_network() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let result = frozen_dataset(&root.join("../../data/relation-review/v0.1-draft"));
        assert!(result.err().unwrap().contains("冻结样本"));
    }
}
