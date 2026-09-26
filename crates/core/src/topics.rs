use super::SourceService;
use codexflow_domain::{
    AppError, ErrorCode, GlobalTopicThread, GlobalTopicView, JevTopicClassification, LocalProject,
    SemanticIndexResult, SemanticNeighbor, SemanticVector, ThreadMetadata, ThreadSummary,
    ThreadTopicAssignment, TopicLabel,
};
use codexflow_jev::JevRelationAnalyzer;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};
use tokio_util::sync::CancellationToken;

const TOPIC_ASSIGNMENT_THRESHOLD: f64 = 0.45;
const EMBEDDING_BATCH_SIZE: usize = 32;
const NEIGHBOR_LIMIT: usize = 10;
const GLOBAL_RELATION_CALL_LIMIT: usize = 100;
const GLOBAL_RELATION_SCOPE: &str = "__codexflow_global_relations__";

#[derive(Clone)]
struct SummaryRecord {
    thread: ThreadMetadata,
    project: LocalProject,
    history_generation: i64,
    embedding_text: String,
    text_digest: String,
    artifacts: Vec<String>,
    files: Vec<String>,
}

#[derive(Clone)]
struct GlobalCandidate {
    candidate: codexflow_domain::RelationCandidate,
    input_version: String,
    tier: CandidateTier,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CandidateTier {
    Strong,
    Medium,
    Weak,
}

fn text_digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn tokens(text: &str) -> std::collections::BTreeSet<String> {
    text.split(|character: char| {
        !character.is_alphanumeric() && character != '_' && character != '-'
    })
    .filter(|word| word.chars().count() >= 4)
    .map(str::to_lowercase)
    .collect()
}

fn summary_text(
    thread: &ThreadMetadata,
    summary: &ThreadSummary,
    artifact_summaries: &[String],
) -> String {
    let mut sections = vec![
        format!("会话标题：{}", thread.title.as_deref().unwrap_or("未命名")),
        format!("会话预览：{}", thread.preview),
        format!("目标：{}", summary.content.goal),
        format!("活动：{}", summary.content.activity),
        format!("结果：{}", summary.content.outcome),
        format!("决定：{}", summary.content.decisions),
        format!("问题：{}", summary.content.issues),
    ];
    if !artifact_summaries.is_empty() {
        sections.push(format!("产物摘要：{}", artifact_summaries.join("；")));
    }
    sections
        .into_iter()
        .filter(|part| !part.ends_with('：'))
        .collect::<Vec<_>>()
        .join("\n")
}

fn cosine(left: &[f32], right: &[f32]) -> Option<f64> {
    if left.is_empty() || left.len() != right.len() {
        return None;
    }
    let (mut dot, mut left_norm, mut right_norm) = (0.0f64, 0.0f64, 0.0f64);
    for (&a, &b) in left.iter().zip(right) {
        if !a.is_finite() || !b.is_finite() {
            return None;
        }
        let a = f64::from(a);
        let b = f64::from(b);
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    (left_norm > 0.0 && right_norm > 0.0).then(|| dot / (left_norm.sqrt() * right_norm.sqrt()))
}

fn candidate_id(left_project: &str, left: &str, right_project: &str, right: &str) -> String {
    let digest = Sha256::digest(
        format!("global-candidate\0{left_project}\0{left}\0{right_project}\0{right}").as_bytes(),
    );
    format!("candidate:{digest:x}")
}

fn empty_evidence() -> codexflow_domain::EvidenceSample {
    codexflow_domain::EvidenceSample {
        left_available: 0,
        right_available: 0,
        combinations_available: 0,
        combinations_shown: 0,
        left_sampled: 0,
        right_sampled: 0,
        sampling_rule: "跨项目总结候选不要求独立来源摘录；Jev 判断具体工作关系。".into(),
        pairs: Vec::new(),
    }
}

fn stable_input_version(
    left: &SummaryRecord,
    right: &SummaryRecord,
    vectors: &BTreeMap<String, SemanticVector>,
    embedding_base_url: &str,
    embedding_model: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(super::candidates::CANDIDATE_RULE_VERSION);
    hash.update(super::facts::RULE_VERSION);
    hash.update(codexflow_jev::RELATION_RULES_VERSION);
    hash.update(embedding_base_url);
    hash.update(embedding_model);
    for record in [left, right] {
        hash.update(&record.project.id);
        hash.update(&record.thread.id);
        hash.update(record.thread.updated_at.to_be_bytes());
        hash.update(record.history_generation.to_be_bytes());
        hash.update(&record.text_digest);
        if let Some(vector) = vectors.get(&record.thread.id) {
            hash.update(&vector.text_digest);
            hash.update(&vector.actual_model);
        }
    }
    format!("{:x}", hash.finalize())
}

fn global_candidates(
    records: &[SummaryRecord],
    vectors: &BTreeMap<String, SemanticVector>,
    embedding_base_url: &str,
    embedding_model: &str,
    now_seconds: i64,
) -> Result<Vec<GlobalCandidate>, AppError> {
    let mut candidates = Vec::new();
    let mut bounded_degree: BTreeMap<String, usize> = BTreeMap::new();
    let mut tier_degree: BTreeMap<(String, CandidateTier), usize> = BTreeMap::new();
    for (left_index, left) in records.iter().enumerate() {
        for right in records.iter().skip(left_index + 1) {
            if left.project.id == right.project.id {
                continue;
            }
            let gap = left.thread.updated_at.abs_diff(right.thread.updated_at);
            let mut score = 0i32;
            let mut reasons = Vec::new();
            let explicit = left.embedding_text.contains(&right.thread.id)
                || right.embedding_text.contains(&left.thread.id);
            if explicit {
                score += 100;
                reasons.push(codexflow_domain::CandidateReason {
                    signal: "explicitReference".into(),
                    detail: "会话总结或预览明确包含另一条会话 ID。".into(),
                });
            }
            let shared_artifact = left
                .artifacts
                .iter()
                .find(|name| right.artifacts.contains(name));
            if let Some(artifact) = shared_artifact {
                score += 35;
                reasons.push(codexflow_domain::CandidateReason {
                    signal: "artifact".into(),
                    detail: format!("双方来源事实提到同一产物：{artifact}"),
                });
            }
            if let Some(file) = left.files.iter().find(|path| right.files.contains(path)) {
                score += 25;
                reasons.push(codexflow_domain::CandidateReason {
                    signal: "file".into(),
                    detail: format!("双方来源事实记录同一文件路径：{file}"),
                });
            }
            if left
                .thread
                .git
                .as_ref()
                .and_then(|git| git.branch.as_ref())
                .is_some_and(|branch| {
                    right
                        .thread
                        .git
                        .as_ref()
                        .and_then(|git| git.branch.as_ref())
                        == Some(branch)
                })
            {
                score += 4;
                reasons.push(codexflow_domain::CandidateReason {
                    signal: "branch".into(),
                    detail: "不同项目中的会话记录了相同分支名。".into(),
                });
            }
            if gap <= 14 * 86_400 {
                let gap_days = gap as f64 / 86_400.0;
                score += (4.0 * 2.0f64.powf(-gap_days / 14.0)).round().max(1.0) as i32;
                reasons.push(codexflow_domain::CandidateReason {
                    signal: "time".into(),
                    detail: format!("最近活动相距 {gap_days:.1} 天；时间线索按 14 天半衰期衰减。"),
                });
            }
            let left_words = tokens(&format!(
                "{} {}",
                left.thread.title.as_deref().unwrap_or(""),
                left.thread.preview
            ));
            let right_words = tokens(&format!(
                "{} {}",
                right.thread.title.as_deref().unwrap_or(""),
                right.thread.preview
            ));
            let overlap = left_words
                .intersection(&right_words)
                .take(3)
                .cloned()
                .collect::<Vec<_>>();
            if !overlap.is_empty() {
                score += 6 + overlap.len() as i32;
                reasons.push(codexflow_domain::CandidateReason {
                    signal: "text".into(),
                    detail: format!("标题或预览中的共同词：{}", overlap.join("、")),
                });
            }
            let semantic_score = vectors
                .get(&left.thread.id)
                .filter(|vector| {
                    vector.base_url == embedding_base_url
                        && vector.requested_model == embedding_model
                        && vector.text_digest == left.text_digest
                })
                .zip(vectors.get(&right.thread.id).filter(|vector| {
                    vector.base_url == embedding_base_url
                        && vector.requested_model == embedding_model
                        && vector.text_digest == right.text_digest
                }))
                .and_then(|(a, b)| cosine(&a.values, &b.values))
                .map(|similarity| {
                    let left_age =
                        now_seconds.saturating_sub(left.thread.updated_at).max(0) as f64 / 86_400.0;
                    let right_age = now_seconds.saturating_sub(right.thread.updated_at).max(0)
                        as f64
                        / 86_400.0;
                    similarity * 2.0f64.powf(-((left_age + right_age) / 2.0) / 14.0)
                });
            if let Some(similarity) = semantic_score {
                score += (similarity * 100.0).round() as i32;
                reasons.push(codexflow_domain::CandidateReason {
                    signal: "semantic".into(),
                    detail: format!("全局向量相似度按 14 天半衰期调整后得分 {similarity:.3}。"),
                });
            }
            if reasons.is_empty() {
                continue;
            }
            let tier = if explicit || shared_artifact.is_some() {
                CandidateTier::Strong
            } else if reasons
                .iter()
                .any(|reason| matches!(reason.signal.as_str(), "file" | "branch" | "time"))
            {
                CandidateTier::Medium
            } else {
                CandidateTier::Weak
            };
            let (left_id, right_id) = if left.thread.id < right.thread.id {
                (&left.thread.id, &right.thread.id)
            } else {
                (&right.thread.id, &left.thread.id)
            };
            let candidate = codexflow_domain::RelationCandidate {
                id: candidate_id(
                    &left.project.id,
                    &left.thread.id,
                    &right.project.id,
                    &right.thread.id,
                ),
                left_thread_id: left_id.clone(),
                right_thread_id: right_id.clone(),
                score,
                reasons,
                evidence: empty_evidence(),
                left_summary: Some(if left.thread.id == *left_id {
                    left.embedding_text.clone()
                } else {
                    right.embedding_text.clone()
                }),
                right_summary: Some(if left.thread.id == *left_id {
                    right.embedding_text.clone()
                } else {
                    left.embedding_text.clone()
                }),
            };
            let (left_record, right_record) = if left.thread.id == *left_id {
                (left, right)
            } else {
                (right, left)
            };
            let input_version = stable_input_version(
                left_record,
                right_record,
                vectors,
                embedding_base_url,
                embedding_model,
            );
            candidates.push(GlobalCandidate {
                candidate,
                input_version,
                tier,
            });
        }
    }
    candidates.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| b.candidate.score.cmp(&a.candidate.score))
            .then_with(|| a.candidate.id.cmp(&b.candidate.id))
    });
    let mut selected = Vec::new();
    for candidate in &candidates {
        if candidate.tier == CandidateTier::Strong {
            selected.push(candidate.clone());
        }
    }
    for tier in [CandidateTier::Medium, CandidateTier::Weak] {
        for candidate in candidates.iter().filter(|candidate| candidate.tier == tier) {
            for endpoint in [
                &candidate.candidate.left_thread_id,
                &candidate.candidate.right_thread_id,
            ] {
                if tier_degree
                    .get(&(endpoint.clone(), tier))
                    .copied()
                    .unwrap_or(0)
                    > 0
                {
                    continue;
                }
                let other = if endpoint == &candidate.candidate.left_thread_id {
                    &candidate.candidate.right_thread_id
                } else {
                    &candidate.candidate.left_thread_id
                };
                if bounded_degree.get(endpoint).copied().unwrap_or(0) >= NEIGHBOR_LIMIT
                    || bounded_degree.get(other).copied().unwrap_or(0) >= NEIGHBOR_LIMIT
                {
                    continue;
                }
                if selected
                    .iter()
                    .any(|item: &GlobalCandidate| item.candidate.id == candidate.candidate.id)
                {
                    continue;
                }
                selected.push(candidate.clone());
                for id in [
                    &candidate.candidate.left_thread_id,
                    &candidate.candidate.right_thread_id,
                ] {
                    *bounded_degree.entry(id.clone()).or_default() += 1;
                    *tier_degree.entry((id.clone(), tier)).or_default() += 1;
                }
            }
        }
    }
    for candidate in &candidates {
        if candidate.tier == CandidateTier::Strong
            || selected
                .iter()
                .any(|item| item.candidate.id == candidate.candidate.id)
        {
            continue;
        }
        if bounded_degree
            .get(&candidate.candidate.left_thread_id)
            .copied()
            .unwrap_or(0)
            >= NEIGHBOR_LIMIT
            || bounded_degree
                .get(&candidate.candidate.right_thread_id)
                .copied()
                .unwrap_or(0)
                >= NEIGHBOR_LIMIT
        {
            continue;
        }
        selected.push(candidate.clone());
        *bounded_degree
            .entry(candidate.candidate.left_thread_id.clone())
            .or_default() += 1;
        *bounded_degree
            .entry(candidate.candidate.right_thread_id.clone())
            .or_default() += 1;
    }
    Ok(selected)
}

impl SourceService {
    fn summary_records(&self) -> Result<(Vec<SummaryRecord>, u64), AppError> {
        let projects = self.sessions.projects()?;
        let projects_by_id: BTreeMap<_, _> = projects
            .into_iter()
            .map(|project| (project.id.clone(), project))
            .collect();
        let mut records = Vec::new();
        let mut unavailable = 0u64;
        for project_id in projects_by_id.keys() {
            let snapshot = self.sessions.project_thread_query_snapshot(project_id)?;
            for record in snapshot.threads {
                let Some(summary) = record.summary else {
                    unavailable += 1;
                    continue;
                };
                if summary.source_updated_at != record.thread.thread.updated_at
                    || summary.history_generation != record.history_generation
                {
                    unavailable += 1;
                    continue;
                }
                let facts = self.sessions.facts(&record.thread.thread.id, 0, 100)?.facts;
                let artifact_summaries = facts
                    .iter()
                    .filter(|fact| matches!(fact.kind, codexflow_domain::FactKind::Artifact))
                    .map(|fact| {
                        format!(
                            "{}；操作 {}；结果 {:?}",
                            fact.subject, fact.operation, fact.outcome
                        )
                    })
                    .collect::<Vec<_>>();
                let artifacts = facts
                    .iter()
                    .filter(|fact| matches!(fact.kind, codexflow_domain::FactKind::Artifact))
                    .map(|fact| fact.subject.clone())
                    .collect::<Vec<_>>();
                let files = facts
                    .iter()
                    .filter(|fact| matches!(fact.kind, codexflow_domain::FactKind::File))
                    .map(|fact| fact.subject.clone())
                    .collect::<Vec<_>>();
                let embedding_text =
                    summary_text(&record.thread.thread, &summary, &artifact_summaries);
                if embedding_text.trim().is_empty() {
                    unavailable += 1;
                    continue;
                }
                let project = projects_by_id
                    .get(project_id)
                    .ok_or_else(|| AppError::store("全局主题项目快照不完整。"))?
                    .clone();
                records.push(SummaryRecord {
                    thread: record.thread.thread,
                    project,
                    history_generation: record.history_generation,
                    text_digest: text_digest(&embedding_text),
                    embedding_text,
                    artifacts,
                    files,
                });
            }
        }
        records.sort_by(|a, b| (&a.project.id, &a.thread.id).cmp(&(&b.project.id, &b.thread.id)));
        Ok((records, unavailable))
    }

    pub fn global_topic_view(&self) -> Result<GlobalTopicView, AppError> {
        let (records, _unavailable) = self.summary_records()?;
        let preferences = self.store.load()?;
        let labels = self.sessions.topic_labels()?;
        let vectors = self.sessions.semantic_vectors()?;
        let assignments = self.sessions.topic_assignments()?;
        let assignments: BTreeMap<_, _> = assignments
            .into_iter()
            .map(|assignment| (assignment.thread_id.clone(), assignment))
            .collect();
        let pending_topic_assignments = if labels.is_empty() {
            0
        } else {
            records
                .iter()
                .filter(|record| {
                    !assignments
                        .get(&record.thread.id)
                        .is_some_and(|assignment| {
                            assignment.manual
                                || (assignment.summary_digest == record.text_digest
                                    && assignment.threshold == TOPIC_ASSIGNMENT_THRESHOLD
                                    && assignment.service_base_url == preferences.jev.base_url
                                    && assignment.requested_model == preferences.jev.model)
                        })
                })
                .count() as u64
        };
        let reviewed_topics = assignments
            .values()
            .filter(|assignment| {
                assignment.manual
                    && assignment.suggested_topic_id.is_some()
                    && records.iter().any(|record| {
                        record.thread.id == assignment.thread_id
                            && record.text_digest == assignment.summary_digest
                    })
            })
            .collect::<Vec<_>>();
        let manual_topic_sample_count = reviewed_topics.len() as u64;
        let manual_topic_agreement = (!reviewed_topics.is_empty()).then(|| {
            reviewed_topics
                .iter()
                .filter(|assignment| assignment.topic_id == assignment.suggested_topic_id)
                .count() as f64
                / reviewed_topics.len() as f64
        });
        let vector_by_thread: BTreeMap<_, _> = vectors
            .into_iter()
            .filter(|vector| {
                vector.base_url == preferences.embedding.base_url
                    && vector.requested_model == preferences.embedding.model
            })
            .map(|vector| (vector.thread_id.clone(), vector))
            .collect();
        let current_models: std::collections::BTreeSet<_> = records
            .iter()
            .filter_map(|record| {
                vector_by_thread.get(&record.thread.id).and_then(|vector| {
                    (vector.text_digest == record.text_digest
                        && vector.source_updated_at == record.thread.updated_at
                        && vector.history_generation == record.history_generation)
                        .then_some(vector.actual_model.as_str())
                })
            })
            .collect();
        let record_by_id: BTreeMap<_, _> = records
            .iter()
            .map(|record| (record.thread.id.as_str(), record))
            .collect();
        let current_vectors: BTreeMap<String, SemanticVector> = records
            .iter()
            .filter_map(|record| {
                vector_by_thread.get(&record.thread.id).and_then(|vector| {
                    (vector.text_digest == record.text_digest
                        && vector.source_updated_at == record.thread.updated_at
                        && vector.history_generation == record.history_generation
                        && current_models.len() == 1
                        && current_models.contains(vector.actual_model.as_str()))
                    .then_some((record.thread.id.clone(), vector.clone()))
                })
            })
            .collect();
        let now = (super::now_ms() / 1000) as i64;
        let mut threads = Vec::with_capacity(records.len());
        for record in &records {
            let neighbors = if let Some(vector) = current_vectors.get(&record.thread.id) {
                let mut scored = current_vectors
                    .iter()
                    .filter(|(other_id, _)| **other_id != record.thread.id)
                    .filter_map(|(other_id, other_vector)| {
                        let other = record_by_id.get(other_id.as_str())?;
                        let similarity = cosine(&vector.values, &other_vector.values)?;
                        let left_age_days =
                            now.saturating_sub(record.thread.updated_at).max(0) as f64 / 86_400.0;
                        let right_age_days =
                            now.saturating_sub(other.thread.updated_at).max(0) as f64 / 86_400.0;
                        let recency = 2.0f64.powf(-((left_age_days + right_age_days) / 2.0) / 14.0);
                        Some((
                            SemanticNeighbor {
                                thread_id: other_id.clone(),
                                project_id: other.project.id.clone(),
                                score: similarity * recency,
                            },
                            other.thread.updated_at,
                        ))
                    })
                    .collect::<Vec<_>>();
                scored.sort_by(|(a, a_time), (b, b_time)| {
                    b.score
                        .total_cmp(&a.score)
                        .then_with(|| b_time.cmp(a_time))
                        .then_with(|| a.thread_id.cmp(&b.thread_id))
                });
                scored
                    .into_iter()
                    .take(NEIGHBOR_LIMIT)
                    .map(|(neighbor, _)| neighbor)
                    .collect()
            } else {
                Vec::new()
            };
            threads.push(GlobalTopicThread {
                thread: record.thread.clone(),
                project: record.project.clone(),
                assignment: assignments
                    .get(&record.thread.id)
                    .filter(|assignment| {
                        assignment.manual || assignment.summary_digest == record.text_digest
                    })
                    .cloned(),
                semantic_neighbors: neighbors,
                summary_available: true,
                indexed: current_vectors.contains_key(&record.thread.id),
            });
        }
        let indexed_count = threads.iter().filter(|thread| thread.indexed).count() as u64;
        let mut threads_without_summary = self.topic_threads_without_summary()?;
        threads.append(&mut threads_without_summary);
        let pending_index_count = threads
            .iter()
            .filter(|thread| thread.summary_available && !thread.indexed)
            .count() as u64;
        let current_jev = preferences.jev;
        let mut cross_project_relations = Vec::new();
        for outcome in self
            .sessions
            .inferred_pair_outcomes(GLOBAL_RELATION_SCOPE)?
        {
            if !outcome.jev_identity.as_ref().is_some_and(|identity| {
                identity.base_url == current_jev.base_url
                    && identity.requested_model == current_jev.model
                    && identity.rules_version == codexflow_jev::RELATION_RULES_VERSION
            }) {
                continue;
            }
            let (Some(left), Some(right)) = (
                record_by_id.get(outcome.left_thread_id.as_str()),
                record_by_id.get(outcome.right_thread_id.as_str()),
            ) else {
                continue;
            };
            if left.project.id == right.project.id
                || stable_input_version(
                    left,
                    right,
                    &current_vectors,
                    &preferences.embedding.base_url,
                    &preferences.embedding.model,
                ) != outcome.input_version
            {
                continue;
            }
            cross_project_relations.push(outcome);
        }
        let semantic_pair_ids: std::collections::BTreeSet<_> = threads
            .iter()
            .flat_map(|thread| {
                thread.semantic_neighbors.iter().filter_map(|neighbor| {
                    if thread.project.id == neighbor.project_id {
                        return None;
                    }
                    let (left, right) = if thread.thread.id < neighbor.thread_id {
                        (thread.thread.id.as_str(), neighbor.thread_id.as_str())
                    } else {
                        (neighbor.thread_id.as_str(), thread.thread.id.as_str())
                    };
                    Some((left.to_owned(), right.to_owned()))
                })
            })
            .collect();
        let completed_pairs: std::collections::BTreeSet<_> = cross_project_relations
            .iter()
            .map(|outcome| {
                if outcome.left_thread_id < outcome.right_thread_id {
                    (
                        outcome.left_thread_id.clone(),
                        outcome.right_thread_id.clone(),
                    )
                } else {
                    (
                        outcome.right_thread_id.clone(),
                        outcome.left_thread_id.clone(),
                    )
                }
            })
            .collect();
        let pending_cross_project_pairs = semantic_pair_ids
            .iter()
            .filter(|pair| !completed_pairs.contains(*pair))
            .count() as u64;
        Ok(GlobalTopicView {
            labels,
            threads,
            indexed_count,
            pending_index_count,
            pending_topic_assignments,
            topic_assignment_threshold: TOPIC_ASSIGNMENT_THRESHOLD,
            manual_topic_sample_count,
            manual_topic_agreement,
            cross_project_candidate_count: semantic_pair_ids.len() as u64,
            pending_cross_project_pairs,
            cross_project_relations,
        })
    }

    fn topic_threads_without_summary(&self) -> Result<Vec<GlobalTopicThread>, AppError> {
        let projects = self.sessions.projects()?;
        let assignments: BTreeMap<_, _> = self
            .sessions
            .topic_assignments()?
            .into_iter()
            .map(|assignment| (assignment.thread_id.clone(), assignment))
            .collect();
        let mut output = Vec::new();
        for project in projects {
            let snapshot = self.sessions.project_thread_query_snapshot(&project.id)?;
            for record in snapshot.threads {
                if record.summary.as_ref().is_some_and(|summary| {
                    summary.source_updated_at == record.thread.thread.updated_at
                        && summary.history_generation == record.history_generation
                }) {
                    continue;
                }
                output.push(GlobalTopicThread {
                    assignment: assignments
                        .get(&record.thread.thread.id)
                        .filter(|assignment| assignment.manual)
                        .cloned(),
                    thread: record.thread.thread,
                    project: project.clone(),
                    semantic_neighbors: Vec::new(),
                    summary_available: false,
                    indexed: false,
                });
            }
        }
        Ok(output)
    }

    pub async fn create_topic_label(
        &self,
        name: String,
        description: String,
    ) -> Result<GlobalTopicView, AppError> {
        let name = name.trim().to_owned();
        let description = description.trim().to_owned();
        if name.is_empty() || name.chars().count() > 80 || description.chars().count() > 500 {
            return Err(AppError::store(
                "主题名称需为 1–80 字，说明不得超过 500 字。",
            ));
        }
        if self.sessions.topic_labels()?.len() >= 80 {
            return Err(AppError::store(
                "全局主题最多支持 80 个，请先合并或删除标签。",
            ));
        }
        let now = super::now_ms() as i64;
        let digest = Sha256::digest(format!("{name}\0{now}").as_bytes());
        self.sessions.save_topic_label(&TopicLabel {
            id: format!("topic:{digest:x}"),
            name,
            description,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
        })?;
        self.global_topic_view()
    }

    pub async fn update_topic_label(
        &self,
        id: String,
        name: String,
        description: String,
    ) -> Result<GlobalTopicView, AppError> {
        let name = name.trim().to_owned();
        let description = description.trim().to_owned();
        if name.is_empty() || name.chars().count() > 80 || description.chars().count() > 500 {
            return Err(AppError::store(
                "主题名称需为 1–80 字，说明不得超过 500 字。",
            ));
        }
        let mut label = self
            .sessions
            .topic_labels()?
            .into_iter()
            .find(|label| label.id == id)
            .ok_or_else(|| AppError::store("找不到要修改的主题标签。"))?;
        label.name = name;
        label.description = description;
        label.updated_at_unix_ms = super::now_ms() as i64;
        self.sessions.save_topic_label(&label)?;
        self.global_topic_view()
    }

    pub async fn delete_topic_label(&self, id: &str) -> Result<GlobalTopicView, AppError> {
        self.sessions.delete_topic_label(id)?;
        self.global_topic_view()
    }

    pub async fn set_thread_topic(
        &self,
        thread_id: &str,
        topic_id: Option<String>,
    ) -> Result<GlobalTopicView, AppError> {
        let project_id = self
            .sessions
            .thread_project_id(thread_id)?
            .ok_or_else(|| AppError::store("该会话当前没有项目归属。"))?;
        if let Some(topic_id) = topic_id.as_deref() {
            if !self
                .sessions
                .topic_labels()?
                .iter()
                .any(|topic| topic.id == topic_id)
            {
                return Err(AppError::store("所选主题标签不存在。"));
            }
        }
        let old = self
            .sessions
            .topic_assignments()?
            .into_iter()
            .find(|item| item.thread_id == thread_id);
        let jev_config = self.preferences.lock().await.jev.clone();
        let summary_digest = old
            .as_ref()
            .map(|item| item.summary_digest.clone())
            .unwrap_or_default();
        let assignment = ThreadTopicAssignment {
            thread_id: thread_id.into(),
            project_id,
            topic_id: topic_id.clone(),
            suggested_topic_id: old.and_then(|item| item.suggested_topic_id),
            confidence: 1.0,
            threshold: TOPIC_ASSIGNMENT_THRESHOLD,
            manual: true,
            summary_digest,
            model: "user".into(),
            service_base_url: jev_config.base_url,
            requested_model: jev_config.model,
            assigned_at_unix_ms: super::now_ms() as i64,
        };
        self.sessions.save_topic_assignment(&assignment, false)?;
        self.global_topic_view()
    }

    pub async fn cancel_semantic_index(&self) {
        self.semantic_index_cancel.lock().await.cancel();
    }

    pub async fn rebuild_global_semantic_index(&self) -> Result<SemanticIndexResult, AppError> {
        let cancel = {
            let mut token = self.semantic_index_cancel.lock().await;
            token.cancel();
            *token = CancellationToken::new();
            token.clone()
        };
        let config = self.embedding_config().await;
        let embedding_revision = self.preferences.lock().await.embedding_revision;
        if config.base_url.is_empty() || config.model.is_empty() {
            return Err(AppError::embedding(
                ErrorCode::EmbeddingNotConfigured,
                "请先配置嵌入服务，再生成全局语义索引。",
                false,
            ));
        }
        let (records, unavailable_summaries) = self.summary_records()?;
        let existing: BTreeMap<_, _> = self
            .sessions
            .semantic_vectors()?
            .into_iter()
            .map(|vector| (vector.thread_id.clone(), vector))
            .collect();
        let cached_models: std::collections::BTreeSet<_> = records
            .iter()
            .filter_map(|record| {
                existing.get(&record.thread.id).and_then(|vector| {
                    (vector.base_url == config.base_url
                        && vector.requested_model == config.model
                        && vector.text_digest == record.text_digest
                        && vector.source_updated_at == record.thread.updated_at
                        && vector.history_generation == record.history_generation)
                        .then_some(vector.actual_model.as_str())
                })
            })
            .collect();
        let cached_actual_model = (cached_models.len() == 1)
            .then(|| cached_models.iter().next().copied())
            .flatten();
        let mut pending = Vec::new();
        let mut reused = 0u64;
        for record in &records {
            if existing.get(&record.thread.id).is_some_and(|vector| {
                vector.base_url == config.base_url
                    && vector.requested_model == config.model
                    && Some(vector.actual_model.as_str()) == cached_actual_model
                    && vector.text_digest == record.text_digest
                    && vector.source_updated_at == record.thread.updated_at
                    && vector.history_generation == record.history_generation
            }) {
                reused += 1;
            } else {
                pending.push(record.clone());
            }
        }
        let mut indexed = 0u64;
        let mut actual_model: Option<String> = cached_actual_model.map(str::to_owned);
        for batch in pending.chunks(EMBEDDING_BATCH_SIZE) {
            if cancel.is_cancelled() {
                return Err(AppError::embedding(
                    ErrorCode::EmbeddingCancelled,
                    "全局语义索引已取消；已完成的向量保留。",
                    false,
                ));
            }
            let texts = batch
                .iter()
                .map(|record| record.embedding_text.clone())
                .collect::<Vec<_>>();
            let output = self
                .embedding_complete(&config, Some(embedding_revision), &texts, cancel.clone())
                .await?;
            if actual_model
                .as_ref()
                .is_some_and(|model| model != &output.actual_model)
            {
                return Err(AppError::embedding(
                    ErrorCode::EmbeddingProtocolInvalid,
                    "同一批嵌入请求返回了不一致的实际模型版本。",
                    false,
                ));
            }
            actual_model = Some(output.actual_model.clone());
            for (record, values) in batch.iter().zip(output.vectors) {
                let vector = SemanticVector {
                    thread_id: record.thread.id.clone(),
                    text_digest: record.text_digest.clone(),
                    base_url: config.base_url.clone(),
                    requested_model: config.model.clone(),
                    actual_model: output.actual_model.clone(),
                    source_updated_at: record.thread.updated_at,
                    history_generation: record.history_generation,
                    values,
                    indexed_at_unix_ms: super::now_ms() as i64,
                };
                if self.sessions.save_semantic_vector_if_current(&vector)? {
                    indexed += 1;
                }
            }
        }
        let labels = self.sessions.topic_labels()?;
        let mut topic_assignments = 0u64;
        let mut pending_topic_assignments = 0u64;
        let mut resolved_jev_model: Option<String> = None;
        let mut alias_probe_used = false;
        let mut topic_calls_used = 0usize;
        if !labels.is_empty() && !records.is_empty() {
            let (credential, requested_model) = self.jev_request_settings().await?;
            let jev_revision = self.preferences.lock().await.jev_revision;
            let jev_model = if super::analysis_batch::pinned_jev_model(&requested_model) {
                requested_model.clone()
            } else {
                let probe = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(AppError::embedding(
                        ErrorCode::EmbeddingCancelled, "全局索引已取消。", false
                    )),
                    value = self.test_jev_inference() => value?,
                };
                alias_probe_used = true;
                if !super::analysis_batch::pinned_jev_model(&probe.actual_model) {
                    return Err(AppError::jev(
                        ErrorCode::JevProtocolInvalid,
                        "Jev 合成探测未返回可固定的版本化模型 ID。",
                        false,
                    ));
                }
                probe.actual_model
            };
            resolved_jev_model = Some(jev_model.clone());
            if self.preferences.lock().await.jev_revision != jev_revision {
                return Err(AppError::jev(
                    ErrorCode::AnalysisConfigChanged,
                    "Jev 设置已变化，请重新启动全局索引。",
                    false,
                ));
            }
            let analyzer = JevRelationAnalyzer::with_timeout_and_material_limit(
                Duration::from_secs(180),
                40_000,
            )?;
            let existing_assignments: BTreeMap<_, _> = self
                .sessions
                .topic_assignments()?
                .into_iter()
                .map(|assignment| (assignment.thread_id.clone(), assignment))
                .collect();
            let mut pending_records = records
                .iter()
                .filter(|record| {
                    !existing_assignments
                        .get(&record.thread.id)
                        .is_some_and(|assignment| {
                            assignment.manual
                                || (assignment.summary_digest == record.text_digest
                                    && assignment.threshold == TOPIC_ASSIGNMENT_THRESHOLD
                                    && assignment.service_base_url == credential.base_url
                                    && assignment.requested_model == requested_model
                                    && assignment.model == jev_model)
                        })
                })
                .collect::<Vec<_>>();
            let pending_count = pending_records.len() as u64;
            pending_records
                .truncate(GLOBAL_RELATION_CALL_LIMIT.saturating_sub(usize::from(alias_probe_used)));
            topic_calls_used = pending_records.len();
            pending_topic_assignments = pending_count.saturating_sub(pending_records.len() as u64);
            for record in pending_records {
                if cancel.is_cancelled() {
                    return Err(AppError::embedding(
                        ErrorCode::EmbeddingCancelled,
                        "全局主题归属已取消；已完成的向量和人工标签保留。",
                        false,
                    ));
                }
                let credential_cancel = self.jev_cancel.lock().await.clone();
                let _jev_guard = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(AppError::embedding(
                        ErrorCode::EmbeddingCancelled, "全局主题归属已取消。", false
                    )),
                    _ = credential_cancel.cancelled() => return Err(AppError::jev(
                        ErrorCode::AnalysisConfigChanged, "Jev 设置已变化，主题结果未保存。", false
                    )),
                    value = self.jev_gate.read() => value,
                };
                if self.preferences.lock().await.jev_revision != jev_revision {
                    return Err(AppError::jev(
                        ErrorCode::AnalysisConfigChanged,
                        "Jev 设置已变化，主题结果未保存。",
                        false,
                    ));
                }
                let classification = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(AppError::embedding(
                        ErrorCode::EmbeddingCancelled, "全局主题归属已取消。", false
                    )),
                    _ = credential_cancel.cancelled() => return Err(AppError::jev(
                        ErrorCode::AnalysisConfigChanged, "Jev 设置已变化，主题结果未保存。", false
                    )),
                    value = analyzer.classify_topic(
                        &credential,
                        &jev_model,
                        &record.thread.id,
                        &record.embedding_text,
                        &labels,
                    ) => value?,
                };
                if classification.actual_model != jev_model {
                    return Err(AppError::jev(
                        ErrorCode::AnalysisConfigChanged,
                        "Jev 主题判断返回的实际模型与本批固定版本不符；该结果未保存。",
                        false,
                    ));
                }
                if self.preferences.lock().await.jev_revision != jev_revision {
                    return Err(AppError::jev(
                        ErrorCode::AnalysisConfigChanged,
                        "Jev 设置已变化，旧主题结果未保存。",
                        false,
                    ));
                }
                let assignment = topic_assignment(
                    record,
                    classification,
                    &credential.base_url,
                    &requested_model,
                );
                if self.sessions.save_topic_assignment(&assignment, true)? {
                    topic_assignments += 1;
                }
                drop(_jev_guard);
            }
        }
        let vector_records: BTreeMap<_, _> = self
            .sessions
            .semantic_vectors()?
            .into_iter()
            .filter(|vector| {
                vector.base_url == config.base_url && vector.requested_model == config.model
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|vector| (vector.thread_id.clone(), vector))
            .collect();
        let current_vectors: BTreeMap<_, _> = records
            .iter()
            .filter_map(|record| {
                vector_records
                    .get(&record.thread.id)
                    .filter(|vector| {
                        vector.text_digest == record.text_digest
                            && vector.source_updated_at == record.thread.updated_at
                            && vector.history_generation == record.history_generation
                    })
                    .map(|vector| (record.thread.id.clone(), vector.clone()))
            })
            .collect();
        let global_candidates = global_candidates(
            &records,
            &current_vectors,
            &config.base_url,
            &config.model,
            (super::now_ms() / 1000) as i64,
        )?;
        let (
            cross_project_relations,
            total_cross_project_candidates,
            pending_cross_project_pairs,
            cross_project_calls,
        ) = if global_candidates.is_empty() {
            (0, 0, 0, 0)
        } else {
            let cached_outcomes = self
                .sessions
                .inferred_pair_outcomes(GLOBAL_RELATION_SCOPE)?;
            let jev = self.jev_request_settings().await?;
            let jev_revision = self.preferences.lock().await.jev_revision;
            let jev_model = if let Some(model) = resolved_jev_model.clone() {
                model
            } else if super::analysis_batch::pinned_jev_model(&jev.1) {
                jev.1.clone()
            } else {
                let probe = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(AppError::embedding(
                        ErrorCode::EmbeddingCancelled, "全局索引已取消。", false
                    )),
                    value = self.test_jev_inference() => value?,
                };
                if !super::analysis_batch::pinned_jev_model(&probe.actual_model) {
                    return Err(AppError::jev(
                        ErrorCode::JevProtocolInvalid,
                        "Jev 合成探测未返回可固定的版本化模型 ID。",
                        false,
                    ));
                }
                alias_probe_used = true;
                probe.actual_model
            };
            if self.preferences.lock().await.jev_revision != jev_revision {
                return Err(AppError::jev(
                    ErrorCode::AnalysisConfigChanged,
                    "Jev 设置已变化，请重新启动全局索引。",
                    false,
                ));
            }
            let analyzer = JevRelationAnalyzer::with_timeout_and_material_limit(
                Duration::from_secs(180),
                40_000,
            )?;
            let mut pending_global = global_candidates
                .iter()
                .filter(|candidate| {
                    !cached_outcomes.iter().any(|outcome| {
                        outcome.candidate_id == candidate.candidate.id
                            && outcome.input_version == candidate.input_version
                            && outcome.jev_identity.as_ref().is_some_and(|identity| {
                                identity.base_url == jev.0.base_url
                                    && identity.requested_model == jev.1
                                    && identity.actual_model == jev_model
                                    && identity.rules_version
                                        == codexflow_jev::RELATION_RULES_VERSION
                            })
                    })
                })
                .collect::<Vec<_>>();
            let pending_count = pending_global.len() as u64;
            pending_global.truncate(
                GLOBAL_RELATION_CALL_LIMIT
                    .saturating_sub(usize::from(alias_probe_used))
                    .saturating_sub(topic_calls_used),
            );
            let mut cross_project_relations = 0u64;
            let credential_cancel = self.jev_cancel.lock().await.clone();
            for candidate in &pending_global {
                if cancel.is_cancelled() {
                    return Err(AppError::embedding(
                        ErrorCode::EmbeddingCancelled,
                        "跨项目关系判断已取消；已保存结果保留。",
                        false,
                    ));
                }
                analyzer.validate_classification(&candidate.candidate, &jev_model)?;
                let _jev_guard = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(AppError::embedding(
                        ErrorCode::EmbeddingCancelled, "跨项目关系判断已取消。", false
                    )),
                    _ = credential_cancel.cancelled() => return Err(AppError::jev(
                        ErrorCode::AnalysisConfigChanged, "Jev 设置已变化，跨项目关系未保存。", false
                    )),
                    value = self.jev_gate.read() => value,
                };
                if self.preferences.lock().await.jev_revision != jev_revision {
                    return Err(AppError::jev(
                        ErrorCode::AnalysisConfigChanged,
                        "Jev 设置已变化，跨项目关系未保存。",
                        false,
                    ));
                }
                let classification = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(AppError::embedding(
                        ErrorCode::EmbeddingCancelled, "跨项目关系判断已取消。", false
                    )),
                    _ = credential_cancel.cancelled() => return Err(AppError::jev(
                        ErrorCode::AnalysisConfigChanged, "Jev 设置已变化，跨项目关系未保存。", false
                    )),
                    value = analyzer.classify(&jev.0, &jev_model, &candidate.candidate) => value?,
                };
                if classification.actual_model != jev_model {
                    return Err(AppError::jev(
                        ErrorCode::AnalysisConfigChanged,
                        "Jev 关系判断返回的实际模型与本批固定版本不符；该结果未保存。",
                        false,
                    ));
                }
                if self.preferences.lock().await.jev_revision != jev_revision {
                    return Err(AppError::jev(
                        ErrorCode::AnalysisConfigChanged,
                        "Jev 设置已变化，旧跨项目关系未保存。",
                        false,
                    ));
                }
                let mut outcome = super::inferred::outcome(
                    self,
                    GLOBAL_RELATION_SCOPE,
                    &candidate.candidate,
                    &candidate.input_version,
                    &classification,
                    None,
                )?;
                for relation in &mut outcome.relations {
                    relation.requested_model = jev.1.clone();
                }
                outcome.jev_identity = Some(codexflow_domain::JevDecisionIdentity {
                    base_url: jev.0.base_url.clone(),
                    requested_model: jev.1.clone(),
                    actual_model: classification.actual_model.clone(),
                    rules_version: codexflow_jev::RELATION_RULES_VERSION.into(),
                    input_character_limit: 40_000,
                });
                cross_project_relations += outcome.relations.len() as u64;
                self.sessions.save_inferred_pair_outcome(&outcome)?;
                drop(_jev_guard);
            }
            if self.preferences.lock().await.jev_revision != jev_revision {
                return Err(AppError::jev(
                    ErrorCode::AnalysisConfigChanged,
                    "Jev 设置已变化，请重新检查全局主题与关系视图。",
                    false,
                ));
            }
            let total_cross_project_candidates = global_candidates.len() as u64;
            let pending_cross_project_pairs =
                pending_count.saturating_sub(pending_global.len() as u64);
            (
                cross_project_relations,
                total_cross_project_candidates,
                pending_cross_project_pairs,
                pending_global.len() as u64,
            )
        };
        Ok(SemanticIndexResult {
            indexed,
            reused,
            unavailable_summaries,
            topic_assignments,
            pending_topic_assignments,
            jev_calls: usize::from(alias_probe_used) as u64
                + topic_calls_used as u64
                + cross_project_calls,
            actual_model,
            cross_project_candidates: total_cross_project_candidates,
            cross_project_relations,
            pending_cross_project_pairs,
        })
    }
}

fn topic_assignment(
    record: &SummaryRecord,
    classification: JevTopicClassification,
    service_base_url: &str,
    requested_model: &str,
) -> ThreadTopicAssignment {
    let suggested_topic_id = classification.selected_topic_id.clone().or_else(|| {
        classification
            .probabilities
            .iter()
            .max_by(|(left_id, left), (right_id, right)| {
                left.total_cmp(right).then_with(|| right_id.cmp(left_id))
            })
            .map(|(topic_id, _)| topic_id.clone())
    });
    let topic_id = (classification.confidence >= TOPIC_ASSIGNMENT_THRESHOLD)
        .then(|| classification.selected_topic_id.clone())
        .flatten();
    ThreadTopicAssignment {
        thread_id: record.thread.id.clone(),
        project_id: record.project.id.clone(),
        topic_id,
        suggested_topic_id,
        confidence: classification.confidence,
        threshold: TOPIC_ASSIGNMENT_THRESHOLD,
        manual: false,
        summary_digest: record.text_digest.clone(),
        model: classification.actual_model,
        service_base_url: service_base_url.into(),
        requested_model: requested_model.into(),
        assigned_at_unix_ms: super::now_ms() as i64,
    }
}
