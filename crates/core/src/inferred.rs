use super::SourceService;
use codexflow_domain::{
    AppError, CandidateEvidence, CausalTimeCheck, EvidenceField, EvidencePair, EvidenceState,
    InferredPairOutcome, InferredRelation, JevEvidenceSelection, JevRelationClassification,
    RelationCandidate, RelationJudgment,
};
use sha2::{Digest, Sha256};

fn stable_id(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("inferred:{:x}", hash.finalize())
}

pub(crate) fn relation_id(
    from: &str,
    to: &str,
    kind: codexflow_domain::InferredRelationKind,
) -> String {
    let (from, to) = if !kind.directed() && from > to {
        (to, from)
    } else {
        (from, to)
    };
    stable_id(&["inferred", from, to, kind.as_str()])
}

pub(crate) fn evidence_version(pair: &EvidencePair) -> Result<String, AppError> {
    let mut sides = [&pair.left, &pair.right];
    sides.sort_by(|a, b| (&a.thread_id, &a.id).cmp(&(&b.thread_id, &b.id)));
    let mut hash = Sha256::new();
    for side in sides {
        let data = serde_json::to_vec(side).map_err(|_| AppError::store("证据版本计算失败。"))?;
        hash.update((data.len() as u64).to_be_bytes());
        hash.update(data);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn candidate_version(
    service: &SourceService,
    candidate: &RelationCandidate,
) -> Result<String, AppError> {
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(candidate).map_err(|_| AppError::store("候选版本计算失败。"))?);
    for thread_id in [&candidate.left_thread_id, &candidate.right_thread_id] {
        let thread = service
            .sessions
            .thread(thread_id)?
            .ok_or_else(|| AppError::store("候选会话不存在。"))?;
        hash.update(thread.updated_at.to_be_bytes());
        hash.update(
            service
                .sessions
                .history_generation(thread_id)?
                .to_be_bytes(),
        );
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn source_text(
    item: &codexflow_domain::HistoryItem,
    field: EvidenceField,
    index: Option<u32>,
) -> Option<&str> {
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

pub(crate) fn valid_evidence(
    service: &SourceService,
    project_id: &str,
    evidence: &CandidateEvidence,
) -> Result<bool, AppError> {
    if service
        .sessions
        .thread_project_id(&evidence.thread_id)?
        .as_deref()
        != Some(project_id)
    {
        return Ok(false);
    }
    let Some(thread) = service.sessions.thread(&evidence.thread_id)? else {
        return Ok(false);
    };
    let Some(turn) = service
        .sessions
        .history_turn(&evidence.thread_id, &evidence.turn_id)?
    else {
        return Ok(false);
    };
    let Some(item) =
        service
            .sessions
            .history_item(&evidence.thread_id, &evidence.turn_id, &evidence.item_id)?
    else {
        return Ok(false);
    };
    if turn.thread_id != evidence.thread_id
        || item.thread_id != evidence.thread_id
        || item.turn_id != evidence.turn_id
        || item.content_version != evidence.content_version
        || item.source_updated_at != thread.updated_at
        || evidence.excerpt.is_empty()
        || !source_text(&item, evidence.field, evidence.change_index)
            .is_some_and(|text| text.contains(&evidence.excerpt))
        || service
            .sessions
            .locate_history_item(&evidence.thread_id, &evidence.turn_id, &evidence.item_id)?
            .is_none()
    {
        return Ok(false);
    }
    if let Some(fact_id) = &evidence.fact_id {
        let Some(snapshot) = service.sessions.stored_evidence_snapshot(&evidence.id)? else {
            return Ok(false);
        };
        let stored = &snapshot.evidence;
        if &stored.fact_id != fact_id
            || stored.thread_id != evidence.thread_id
            || stored.turn_id != evidence.turn_id
            || stored.item_id != evidence.item_id
            || stored.field != evidence.field
            || stored.change_index != evidence.change_index
            || stored.excerpt != evidence.excerpt
            || stored.content_version != evidence.content_version
            || SourceService::check_evidence_snapshot(snapshot).state != EvidenceState::Valid
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn time_check(
    service: &SourceService,
    from: &CandidateEvidence,
    to: &CandidateEvidence,
) -> Result<Option<CausalTimeCheck>, AppError> {
    let a = service
        .sessions
        .history_turn(&from.thread_id, &from.turn_id)?;
    let b = service.sessions.history_turn(&to.thread_id, &to.turn_id)?;
    let (Some(a), Some(b)) = (a, b) else {
        return Ok(None);
    };
    if a.started_at_unix_ms
        .zip(b.completed_at_unix_ms)
        .is_some_and(|(start, end)| start > end)
    {
        return Ok(None);
    }
    if a.completed_at_unix_ms
        .zip(b.started_at_unix_ms)
        .is_some_and(|(end, start)| end <= start)
    {
        return Ok(Some(CausalTimeCheck::Verified));
    }
    Ok(Some(CausalTimeCheck::Unverifiable))
}

pub(crate) fn outcome(
    service: &SourceService,
    project_id: &str,
    candidate: &RelationCandidate,
    input_version: &str,
    classification: &JevRelationClassification,
    selection: Option<&JevEvidenceSelection>,
) -> Result<InferredPairOutcome, AppError> {
    let supported: Vec<_> = classification
        .choices
        .iter()
        .filter(|choice| choice.judgment == RelationJudgment::Supported)
        .collect();
    let unknown_count = classification
        .choices
        .iter()
        .filter(|choice| choice.judgment == RelationJudgment::Unknown)
        .count() as u32;
    let mut relations = Vec::new();
    if !supported.is_empty() {
        let selected = selection.ok_or_else(|| AppError::store("证据选择尚未完成。"))?;
        if selected.choices.len() != supported.len() {
            return Err(AppError::store("证据选择结果不完整。"));
        }
        for choice in &supported {
            if choice.from_thread_id == choice.to_thread_id {
                continue;
            }
            if !((choice.from_thread_id == candidate.left_thread_id
                && choice.to_thread_id == candidate.right_thread_id)
                || (choice.from_thread_id == candidate.right_thread_id
                    && choice.to_thread_id == candidate.left_thread_id))
            {
                continue;
            }
            let picked = selected
                .choices
                .iter()
                .find(|picked| picked.relation_key == choice.key)
                .ok_or_else(|| AppError::store("证据选择缺少关系。"))?;
            let Some(pair_id) = &picked.pair_id else {
                continue;
            };
            let Some(pair) = candidate
                .evidence
                .pairs
                .iter()
                .find(|pair| &pair.id == pair_id)
            else {
                continue;
            };
            if pair.left.thread_id != candidate.left_thread_id
                || pair.right.thread_id != candidate.right_thread_id
                || !valid_evidence(service, project_id, &pair.left)?
                || !valid_evidence(service, project_id, &pair.right)?
            {
                continue;
            }
            let (from, to) = if choice.from_thread_id == candidate.left_thread_id {
                (&pair.left, &pair.right)
            } else {
                (&pair.right, &pair.left)
            };
            let time_check = if choice.kind.directed() {
                let Some(check) = time_check(service, from, to)? else {
                    continue;
                };
                check
            } else {
                CausalTimeCheck::Unverifiable
            };
            let (from_id, to_id) = if choice.kind.directed() {
                (&choice.from_thread_id, &choice.to_thread_id)
            } else if candidate.left_thread_id <= candidate.right_thread_id {
                (&candidate.left_thread_id, &candidate.right_thread_id)
            } else {
                (&candidate.right_thread_id, &candidate.left_thread_id)
            };
            relations.push(InferredRelation {
                id: relation_id(from_id, to_id, choice.kind),
                project_id: project_id.into(),
                candidate_id: candidate.id.clone(),
                from_thread_id: from_id.clone(),
                to_thread_id: to_id.clone(),
                kind: choice.kind,
                source: "jev".into(),
                requested_model: classification.requested_model.clone(),
                actual_model: classification.actual_model.clone(),
                confidence: choice.answer.confidence,
                probabilities: choice.answer.probabilities.clone(),
                evidence_confidence: picked.answer.confidence,
                evidence_probabilities: picked.answer.probabilities.clone(),
                evidence: EvidencePair {
                    id: pair.id.clone(),
                    left: pair.left.clone(),
                    right: pair.right.clone(),
                },
                time_check,
                explanation: format!(
                    "Jev 判定 {}；本地依据所选双侧来源摘录整理。左侧：{} 右侧：{}",
                    choice.kind.as_str(),
                    pair.left.excerpt,
                    pair.right.excerpt
                ),
                input_version: input_version.into(),
            });
        }
    }
    let status = if !relations.is_empty() {
        "valid"
    } else if supported.is_empty() && unknown_count == 0 {
        "none"
    } else if supported.is_empty() {
        "undetermined"
    } else {
        "insufficientEvidence"
    };
    Ok(InferredPairOutcome {
        candidate_id: candidate.id.clone(),
        project_id: project_id.into(),
        input_version: input_version.into(),
        status: status.into(),
        unknown_count,
        decisions: classification.choices.clone(),
        jev_identity: None,
        relations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexflow_domain::InferredRelationKind;

    #[test]
    fn inferred_identity_normalizes_only_symmetric_endpoints() {
        assert_eq!(
            relation_id("thread-b", "thread-a", InferredRelationKind::Related),
            relation_id("thread-a", "thread-b", InferredRelationKind::Related),
        );
        assert_eq!(
            relation_id("thread-b", "thread-a", InferredRelationKind::AlternativeTo),
            relation_id("thread-a", "thread-b", InferredRelationKind::AlternativeTo),
        );
        assert_ne!(
            relation_id("thread-b", "thread-a", InferredRelationKind::Fixes),
            relation_id("thread-a", "thread-b", InferredRelationKind::Fixes),
        );
        assert_ne!(
            relation_id("thread-a", "thread-b", InferredRelationKind::Related),
            relation_id("thread-a", "thread-b", InferredRelationKind::AlternativeTo),
        );
    }
}
