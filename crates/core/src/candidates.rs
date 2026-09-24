use codexflow_domain::{
    CandidateEvidence, CandidatePreview, CandidateReason, DerivedRelation, DerivedRelationKind,
    EvidenceField, EvidencePair, EvidenceSample, FactKind, FactOutcome, HistoryItem,
    ProjectSessions, RelationCandidate, SourceEvidence,
};
use codexflow_store::ProjectMaterial;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(crate) const NEIGHBOR_LIMIT: usize = 10;
pub(crate) const CANDIDATE_RULE_VERSION: &str = "candidate-signals-v2";
const EVIDENCE_LIMIT: usize = 20;

fn stable_id(prefix: &str, parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{prefix}:{:x}", hash.finalize())
}

fn source_text<'a>(
    item: &'a HistoryItem,
    field: EvidenceField,
    index: Option<u32>,
) -> Option<&'a str> {
    match field {
        EvidenceField::Text => item.text.as_deref(),
        EvidenceField::Command => item.command.as_deref(),
        EvidenceField::Output => item.output.as_deref(),
        EvidenceField::ChangePath => index
            .and_then(|i| item.changes.get(i as usize))
            .map(|c| c.path.as_str()),
        EvidenceField::ChangeDiff => index
            .and_then(|i| item.changes.get(i as usize))
            .map(|c| c.diff.as_str()),
    }
}

fn excerpt(text: &str, needle: Option<&str>) -> String {
    let start = needle
        .and_then(|word| text.find(word))
        .map(|index| index.saturating_sub(70))
        .unwrap_or(0);
    let mut start = start;
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    text[start..].chars().take(240).collect()
}

fn pointer(
    item: &HistoryItem,
    field: EvidenceField,
    index: Option<u32>,
    needle: Option<&str>,
) -> Option<CandidateEvidence> {
    let text = source_text(item, field, index)?.trim();
    if text.is_empty() {
        return None;
    }
    let excerpt = excerpt(text, needle);
    let id = stable_id(
        "candidate-evidence",
        &[
            &item.thread_id,
            &item.turn_id,
            &item.id,
            &format!("{field:?}"),
            &index.map(|i| i.to_string()).unwrap_or_default(),
            &excerpt,
        ],
    );
    Some(CandidateEvidence {
        id,
        thread_id: item.thread_id.clone(),
        turn_id: item.turn_id.clone(),
        item_id: item.id.clone(),
        field,
        change_index: index,
        excerpt,
        content_version: item.content_version.clone(),
        fact_id: None,
    })
}

fn fact_pointer(
    e: &SourceEvidence,
    items: &BTreeMap<(&str, &str, &str), &HistoryItem>,
) -> Option<CandidateEvidence> {
    let item = items.get(&(e.thread_id.as_str(), e.turn_id.as_str(), e.item_id.as_str()))?;
    if item.content_version != e.content_version
        || e.excerpt.is_empty()
        || !source_text(item, e.field, e.change_index).is_some_and(|text| text.contains(&e.excerpt))
    {
        return None;
    }
    Some(CandidateEvidence {
        id: e.id.clone(),
        thread_id: e.thread_id.clone(),
        turn_id: e.turn_id.clone(),
        item_id: e.item_id.clone(),
        field: e.field,
        change_index: e.change_index,
        excerpt: e.excerpt.clone(),
        content_version: e.content_version.clone(),
        fact_id: Some(e.fact_id.clone()),
    })
}

fn tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|word| word.chars().count() >= 4)
        .map(|word| word.to_lowercase())
        .collect()
}

fn project_file_key(path: &str, workspace_root: Option<&str>) -> String {
    let path = Path::new(path);
    if path.is_absolute() {
        if let Some(root) = workspace_root {
            if let Ok(relative) = path.strip_prefix(root) {
                return relative.to_string_lossy().into_owned();
            }
        }
    }
    path.to_string_lossy().into_owned()
}

struct Signals {
    files: BTreeMap<String, CandidateEvidence>,
    artifacts: BTreeMap<String, CandidateEvidence>,
    references: BTreeMap<String, CandidateEvidence>,
    evidence: Vec<CandidateEvidence>,
    words: BTreeSet<String>,
    branch: Option<String>,
    updated_at: i64,
}

struct ScoredPair {
    left: String,
    right: String,
    score: i32,
    reasons: Vec<CandidateReason>,
    relations: Vec<(DerivedRelationKind, String, Vec<CandidateEvidence>)>,
}

fn shared<'a, T>(left: &'a BTreeMap<String, T>, right: &BTreeMap<String, T>) -> Option<&'a str> {
    left.keys()
        .find(|key| right.contains_key(*key))
        .map(String::as_str)
}

fn score_pair(
    left_id: &str,
    right_id: &str,
    left: &Signals,
    right: &Signals,
) -> Option<ScoredPair> {
    let mut score = 0;
    let mut reasons = Vec::new();
    let mut relations = Vec::new();
    if let Some(path) = shared(&left.files, &right.files) {
        score += 40;
        reasons.push(CandidateReason {
            signal: "file".into(),
            detail: format!("双方来源条目记录同一文件：{path}"),
        });
        relations.push((
            DerivedRelationKind::SharedFile,
            format!("双方来源条目记录同一文件：{path}"),
            vec![left.files[path].clone(), right.files[path].clone()],
        ));
    }
    if let Some(name) = shared(&left.artifacts, &right.artifacts) {
        score += 35;
        reasons.push(CandidateReason {
            signal: "artifact".into(),
            detail: format!("双方来源条目记录同一产物：{name}"),
        });
        relations.push((
            DerivedRelationKind::SharedArtifact,
            format!("双方来源条目记录同一产物：{name}"),
            vec![left.artifacts[name].clone(), right.artifacts[name].clone()],
        ));
    }
    if let Some(proof) = left
        .references
        .get(right_id)
        .or_else(|| right.references.get(left_id))
    {
        score += 100;
        reasons.push(CandidateReason {
            signal: "explicitReference".into(),
            detail: format!("来源条目明确引用另一条会话：{}", proof.excerpt),
        });
        relations.push((
            DerivedRelationKind::ExplicitReference,
            "来源条目明确引用另一条会话标识。".into(),
            vec![proof.clone()],
        ));
    }
    if let Some(branch) = left
        .branch
        .as_ref()
        .filter(|branch| right.branch.as_ref() == Some(*branch))
    {
        score += 4;
        reasons.push(CandidateReason {
            signal: "branch".into(),
            detail: format!("同一分支：{branch}"),
        });
    }
    let gap = left.updated_at.abs_diff(right.updated_at);
    if gap <= 86_400 {
        score += 3;
        reasons.push(CandidateReason {
            signal: "time".into(),
            detail: "最近活动相距不超过一天。".into(),
        });
    } else if gap <= 7 * 86_400 {
        score += 1;
        reasons.push(CandidateReason {
            signal: "time".into(),
            detail: "最近活动相距不超过一周。".into(),
        });
    }
    let overlap = left
        .words
        .intersection(&right.words)
        .take(3)
        .cloned()
        .collect::<Vec<_>>();
    if !overlap.is_empty() {
        score += 6 + overlap.len() as i32;
        reasons.push(CandidateReason {
            signal: "text".into(),
            detail: format!("标题或预览的共同词：{}", overlap.join("、")),
        });
    }
    (score > 0).then(|| ScoredPair {
        left: left_id.into(),
        right: right_id.into(),
        score,
        reasons,
        relations,
    })
}

fn sample(left: &Signals, right: &Signals, reasons: &[CandidateReason]) -> EvidenceSample {
    let relevant = reasons
        .iter()
        .filter(|reason| {
            reason.signal == "file"
                || reason.signal == "artifact"
                || reason.signal == "explicitReference"
        })
        .map(|reason| reason.detail.as_str())
        .collect::<Vec<_>>();
    let ranked = |sources: &[CandidateEvidence]| {
        let mut sources = sources.to_vec();
        sources.sort_by(|a, b| {
            let rank = |item: &CandidateEvidence| {
                relevant
                    .iter()
                    .any(|text| text.contains(&item.excerpt) || item.excerpt.contains(text))
            };
            rank(b).cmp(&rank(a)).then_with(|| a.id.cmp(&b.id))
        });
        sources
    };
    let left_all = ranked(&left.evidence);
    let right_all = ranked(&right.evidence);
    let left_sampled = left_all.len().min(EVIDENCE_LIMIT);
    let right_sampled = right_all.len().min(EVIDENCE_LIMIT);
    let available = (left_all.len() as u64).saturating_mul(right_all.len() as u64);
    let count = available.min(EVIDENCE_LIMIT as u64) as usize;
    let mut pairs = Vec::with_capacity(count);
    for i in 0..count {
        let a = left_all[i % left_sampled].clone();
        let b = right_all[i % right_sampled].clone();
        pairs.push(EvidencePair {
            id: stable_id(
                "evidence-pair",
                &[&a.id, &a.content_version, &b.id, &b.content_version],
            ),
            left: a,
            right: b,
        });
    }
    EvidenceSample {
        left_available: left_all.len() as u64,
        right_available: right_all.len() as u64,
        combinations_available: available,
        combinations_shown: pairs.len() as u32,
        left_sampled: left_sampled as u32,
        right_sampled: right_sampled as u32,
        sampling_rule:
            "双方有效来源摘录按相关性和稳定标识排序，各取前 20 条；按稳定轮转抽取最多 20 组。"
                .into(),
        pairs,
    }
}

pub(crate) fn build(
    sessions: &ProjectSessions,
    material: ProjectMaterial,
) -> (CandidatePreview, Vec<DerivedRelation>) {
    let thread_ids: BTreeSet<&str> = sessions
        .threads
        .iter()
        .map(|t| t.thread.id.as_str())
        .collect();
    let item_map: BTreeMap<_, _> = material
        .items
        .iter()
        .filter(|item| thread_ids.contains(item.thread_id.as_str()) && item.supported)
        .map(|item| {
            (
                (
                    item.thread_id.as_str(),
                    item.turn_id.as_str(),
                    item.id.as_str(),
                ),
                item,
            )
        })
        .collect();
    let fact_proofs: BTreeMap<_, _> = material
        .evidence
        .iter()
        .filter_map(|e| fact_pointer(e, &item_map).map(|proof| (e.fact_id.as_str(), proof)))
        .collect();
    let workspaces: BTreeMap<_, _> = sessions
        .threads
        .iter()
        .map(|t| {
            (
                t.thread.id.as_str(),
                t.attribution.workspace_root.as_deref(),
            )
        })
        .collect();
    let mut signals: BTreeMap<String, Signals> = sessions
        .threads
        .iter()
        .map(|t| {
            let meta = &t.thread;
            (
                meta.id.clone(),
                Signals {
                    files: BTreeMap::new(),
                    artifacts: BTreeMap::new(),
                    references: BTreeMap::new(),
                    evidence: Vec::new(),
                    words: tokens(&format!(
                        "{} {}",
                        meta.title.as_deref().unwrap_or(""),
                        meta.preview
                    )),
                    branch: meta.git.as_ref().and_then(|git| git.branch.clone()),
                    updated_at: meta.updated_at,
                },
            )
        })
        .collect();
    for fact in &material.facts {
        let Some(signal) = signals.get_mut(&fact.thread_id) else {
            continue;
        };
        let Some(proof) = fact_proofs.get(fact.id.as_str()) else {
            continue;
        };
        if fact.outcome != FactOutcome::Failed {
            match fact.kind {
                FactKind::File => {
                    signal
                        .files
                        .entry(project_file_key(
                            &fact.subject,
                            workspaces.get(fact.thread_id.as_str()).copied().flatten(),
                        ))
                        .or_insert_with(|| proof.clone());
                }
                FactKind::Artifact => {
                    signal
                        .artifacts
                        .entry(fact.subject.clone())
                        .or_insert_with(|| proof.clone());
                }
                FactKind::Branch => {
                    if signal.branch.is_none() {
                        signal.branch = Some(fact.subject.clone());
                    }
                }
                _ => {}
            }
        }
        signal.evidence.push(proof.clone());
    }
    for item in item_map.values() {
        let signal = signals
            .get_mut(&item.thread_id)
            .expect("项目条目必须属于项目会话");
        for field in [
            EvidenceField::Text,
            EvidenceField::Command,
            EvidenceField::Output,
        ] {
            let Some(raw) = source_text(item, field, None) else {
                continue;
            };
            if let Some(proof) = pointer(item, field, None, None) {
                signal.evidence.push(proof);
            }
            for word in raw.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_') {
                if word.len() >= 8 && thread_ids.contains(word) && word != item.thread_id {
                    if let Some(proof) = pointer(item, field, None, Some(word)) {
                        signal
                            .references
                            .entry(word.to_owned())
                            .or_insert_with(|| proof.clone());
                        signal.evidence.push(proof);
                    }
                }
            }
        }
    }
    for signal in signals.values_mut() {
        signal.evidence.sort_by(|a, b| a.id.cmp(&b.id));
        signal.evidence.dedup_by(|a, b| a.id == b.id);
    }
    let ids = signals.keys().cloned().collect::<Vec<_>>();
    let mut scored = Vec::new();
    for (index, left_id) in ids.iter().enumerate() {
        for right_id in ids.iter().skip(index + 1) {
            if let Some(pair) = score_pair(left_id, right_id, &signals[left_id], &signals[right_id])
            {
                scored.push(pair);
            }
        }
    }
    let mut selected = BTreeSet::new();
    for id in &ids {
        let mut neighbors = scored
            .iter()
            .enumerate()
            .filter(|(_, pair)| &pair.left == id || &pair.right == id)
            .map(|(index, pair)| (index, pair))
            .collect::<Vec<_>>();
        neighbors.sort_by(|(_, a), (_, b)| {
            b.score.cmp(&a.score).then_with(|| {
                if &a.left == id { &a.right } else { &a.left }.cmp(if &b.left == id {
                    &b.right
                } else {
                    &b.left
                })
            })
        });
        selected.extend(
            neighbors
                .into_iter()
                .take(NEIGHBOR_LIMIT)
                .map(|(index, _)| index),
        );
    }
    let mut selected = selected
        .into_iter()
        .map(|index| &scored[index])
        .collect::<Vec<_>>();
    selected.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| (&a.left, &a.right).cmp(&(&b.left, &b.right)))
    });
    let mut degree: BTreeMap<&str, usize> = BTreeMap::new();
    selected.retain(|pair| {
        if degree.get(pair.left.as_str()).copied().unwrap_or(0) >= NEIGHBOR_LIMIT
            || degree.get(pair.right.as_str()).copied().unwrap_or(0) >= NEIGHBOR_LIMIT
        {
            return false;
        }
        *degree.entry(pair.left.as_str()).or_default() += 1;
        *degree.entry(pair.right.as_str()).or_default() += 1;
        true
    });
    let mut candidates = Vec::new();
    let mut relations = Vec::new();
    for pair in selected {
        let id = stable_id(
            "candidate",
            &[&sessions.project.id, &pair.left, &pair.right],
        );
        candidates.push(RelationCandidate {
            id,
            left_thread_id: pair.left.clone(),
            right_thread_id: pair.right.clone(),
            score: pair.score,
            reasons: pair.reasons.clone(),
            evidence: sample(&signals[&pair.left], &signals[&pair.right], &pair.reasons),
        });
        for (kind, basis, proofs) in &pair.relations {
            relations.push(DerivedRelation {
                id: stable_id(
                    "derived",
                    &[
                        &sessions.project.id,
                        &pair.left,
                        &pair.right,
                        &format!("{kind:?}"),
                    ],
                ),
                project_id: sessions.project.id.clone(),
                from_thread_id: pair.left.clone(),
                to_thread_id: pair.right.clone(),
                kind: *kind,
                source: "derived".into(),
                basis: basis.clone(),
                evidence: proofs.clone(),
            });
        }
    }
    relations.sort_by(|a, b| a.id.cmp(&b.id));
    let preview = CandidatePreview {
        project_id: sessions.project.id.clone(),
        thread_count: ids.len() as u64,
        unavailable_threads: 0,
        neighbor_limit: NEIGHBOR_LIMIT as u32,
        candidate_count: candidates.len() as u64,
        candidates,
        candidate_versions: std::collections::BTreeMap::new(),
        stale_candidates: Vec::new(),
    };
    (preview, relations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexflow_domain::{
        AttributedThread, GitMetadata, HistoryFileChange, LocalProject, SourceFact,
        ThreadAttribution, ThreadMetadata,
    };

    fn session(id: &str, branch: Option<&str>, updated_at: i64) -> AttributedThread {
        AttributedThread {
            thread: ThreadMetadata {
                id: id.into(),
                session_id: id.into(),
                title: None,
                preview: String::new(),
                cwd: "/project".into(),
                project_id: None,
                source_kind: "cli".into(),
                source_detail: None,
                thread_source: None,
                parent_thread_id: None,
                forked_from_id: None,
                git: branch.map(|name| GitMetadata {
                    branch: Some(name.into()),
                    sha: None,
                    origin_url: None,
                }),
                created_at: updated_at,
                updated_at,
                archived: false,
                metadata_complete: true,
                turns_complete: true,
                items_complete: true,
                missing_from_source: false,
                content_complete: true,
                read_error: None,
                observed_at_unix_ms: 0,
            },
            attribution: ThreadAttribution {
                thread_id: id.into(),
                project_id: Some("project".into()),
                workspace_root: Some("/project".into()),
                basis: "git".into(),
                detail: String::new(),
                diagnostic: None,
                source_project_id: None,
            },
        }
    }

    fn project(threads: Vec<AttributedThread>) -> ProjectSessions {
        ProjectSessions {
            project: LocalProject {
                id: "project".into(),
                name: "项目".into(),
                root: "/project".into(),
                git_common_dir: None,
            },
            workspaces: vec!["/project".into()],
            threads,
            scopes: Vec::new(),
        }
    }

    fn item(thread: &str, text: Option<&str>, path: Option<&str>) -> HistoryItem {
        HistoryItem {
            thread_id: thread.into(),
            turn_id: "turn".into(),
            id: format!("item-{thread}"),
            ordinal: 0,
            source_type: if path.is_some() {
                "fileChange"
            } else {
                "agentMessage"
            }
            .into(),
            supported: true,
            text: text.map(str::to_owned),
            command: None,
            cwd: None,
            output: None,
            exit_code: None,
            status: Some("completed".into()),
            changes: path
                .map(|p| {
                    vec![HistoryFileChange {
                        path: p.into(),
                        kind: "modified".into(),
                        diff: "diff".into(),
                    }]
                })
                .unwrap_or_default(),
            source_updated_at: 0,
            content_version: format!("v-{thread}"),
        }
    }

    fn file_material(items: Vec<HistoryItem>) -> ProjectMaterial {
        let mut facts = Vec::new();
        let mut evidence = Vec::new();
        for item in &items {
            if let Some(change) = item.changes.first() {
                let id = format!("fact-{}", item.thread_id);
                let evidence_id = format!("evidence-{}", item.thread_id);
                facts.push(SourceFact {
                    id: id.clone(),
                    thread_id: item.thread_id.clone(),
                    turn_id: item.turn_id.clone(),
                    item_id: item.id.clone(),
                    kind: FactKind::File,
                    subject: change.path.clone(),
                    operation: "modified".into(),
                    outcome: FactOutcome::Succeeded,
                    evidence_id: evidence_id.clone(),
                    content_version: item.content_version.clone(),
                    rule_version: "v1".into(),
                });
                evidence.push(SourceEvidence {
                    id: evidence_id,
                    fact_id: id,
                    thread_id: item.thread_id.clone(),
                    turn_id: item.turn_id.clone(),
                    item_id: item.id.clone(),
                    field: EvidenceField::ChangePath,
                    change_index: Some(0),
                    excerpt: change.path.clone(),
                    content_version: item.content_version.clone(),
                });
            }
        }
        ProjectMaterial {
            facts,
            evidence,
            items,
        }
    }

    #[test]
    fn shared_file_is_factual_and_has_both_locatable_sides() {
        let sessions = project(vec![
            session("thread-a", None, 0),
            session("thread-b", None, 100_000_000),
        ]);
        let material = file_material(vec![
            item("thread-a", None, Some("src/lib.rs")),
            item("thread-b", None, Some("src/lib.rs")),
        ]);
        let (preview, relations) = build(&sessions, material);
        assert_eq!(preview.candidate_count, 1);
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].kind, DerivedRelationKind::SharedFile);
        assert_eq!(relations[0].evidence.len(), 2);
        assert_eq!(preview.candidates[0].evidence.pairs.len(), 1);
        assert!(preview.candidates[0]
            .evidence
            .pairs
            .iter()
            .all(|pair| pair.left.thread_id == "thread-a" && pair.right.thread_id == "thread-b"));
        assert!(!relations[0].basis.contains("修复"));
    }

    #[test]
    fn large_same_branch_is_bounded_and_does_not_create_visible_rules() {
        let sessions = project(
            (0..40)
                .map(|i| session(&format!("thread-{i:03}"), Some("main"), 0))
                .collect(),
        );
        let (preview, relations) = build(&sessions, file_material(Vec::new()));
        assert!(relations.is_empty());
        assert!(preview.candidate_count <= 40 * 10 / 2);
        let mut degree = BTreeMap::<&str, usize>::new();
        for pair in &preview.candidates {
            *degree.entry(&pair.left_thread_id).or_default() += 1;
            *degree.entry(&pair.right_thread_id).or_default() += 1;
            assert!(pair.evidence.pairs.is_empty());
        }
        assert!(degree.values().all(|count| *count <= 10));
        let again = build(&sessions, file_material(Vec::new())).0;
        assert_eq!(
            serde_json::to_string(&preview).unwrap(),
            serde_json::to_string(&again).unwrap()
        );
    }

    #[test]
    fn explicit_reference_and_union_are_unique() {
        let sessions = project(vec![
            session("thread-aaa", None, 0),
            session("thread-bbb", None, 100_000_000),
        ]);
        let material = file_material(vec![
            item("thread-aaa", Some("请查看 thread-bbb 的结果"), None),
            item("thread-bbb", Some("独立记录"), None),
        ]);
        let (preview, relations) = build(&sessions, material);
        assert_eq!(preview.candidate_count, 1);
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].kind, DerivedRelationKind::ExplicitReference);
        assert!(relations[0].evidence[0].excerpt.contains("thread-bbb"));
        assert_eq!(preview.candidates[0].evidence.combinations_shown, 1);
    }

    #[test]
    fn independent_rule_kinds_can_coexist_on_one_pair() {
        let sessions = project(vec![
            session("thread-aaa", None, 0),
            session("thread-bbb", None, 100_000_000),
        ]);
        let material = file_material(vec![
            item("thread-aaa", Some("查看 thread-bbb"), Some("src/lib.rs")),
            item("thread-bbb", None, Some("src/lib.rs")),
        ]);
        let (preview, relations) = build(&sessions, material);
        assert_eq!(preview.candidate_count, 1);
        assert_eq!(relations.len(), 2);
        assert!(relations
            .iter()
            .any(|r| r.kind == DerivedRelationKind::SharedFile));
        assert!(relations
            .iter()
            .any(|r| r.kind == DerivedRelationKind::ExplicitReference));
    }

    #[test]
    fn empty_candidates_and_evidence_sampling_limit() {
        let sessions = project(vec![
            session("thread-a", None, 0),
            session("thread-b", None, 100_000_000),
        ]);
        assert!(build(&sessions, file_material(Vec::new()))
            .0
            .candidates
            .is_empty());
        let mut left = item("thread-a", None, Some("src/lib.rs"));
        left.text = Some("有更多文字条目".into());
        let mut right = item("thread-b", None, Some("src/lib.rs"));
        right.text = Some("还有更多文字条目".into());
        let (preview, _) = build(&sessions, file_material(vec![left, right]));
        assert!(preview.candidates[0].evidence.pairs.len() <= 20);
        assert_eq!(preview.candidates[0].evidence.combinations_available, 4);
    }

    #[test]
    fn source_change_recomputes_rules_and_caps_twenty_evidence_pairs() {
        let sessions = project(vec![
            session("thread-a", None, 0),
            session("thread-b", None, 100_000_000),
        ]);
        let mut items = vec![
            item("thread-a", None, Some("src/lib.rs")),
            item("thread-b", None, Some("src/lib.rs")),
        ];
        for thread in ["thread-a", "thread-b"] {
            for index in 0..25 {
                let mut extra = item(thread, Some("来源消息内容"), None);
                extra.id = format!("message-{thread}-{index:02}");
                items.push(extra);
            }
        }
        let (preview, rules) = build(&sessions, file_material(items.clone()));
        assert_eq!(rules.len(), 1);
        assert_eq!(preview.candidates[0].evidence.combinations_shown, 20);
        assert_eq!(preview.candidates[0].evidence.left_sampled, 20);
        assert_eq!(preview.candidates[0].evidence.right_sampled, 20);
        items[1].changes[0].path = "src/other.rs".into();
        items[1].content_version = "new-version".into();
        let (next, next_rules) = build(&sessions, file_material(items));
        assert!(next.candidates.is_empty());
        assert!(next_rules.is_empty());
    }
}
