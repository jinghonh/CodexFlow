use super::SourceService;
use codexflow_domain::{
    AppError, CausalTimeCheck, EvidencePair, InferredPairOutcome, InferredRelation,
    JevEvidenceSelection, JevRelationClassification, RelationCandidate, RelationJudgment,
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

pub(crate) fn evidence_version(
    pair: Option<&EvidencePair>,
    input_version: &str,
) -> Result<String, AppError> {
    let mut hash = Sha256::new();
    hash.update(input_version);
    if let Some(pair) = pair {
        let mut sides = [&pair.left, &pair.right];
        sides.sort_by(|a, b| (&a.thread_id, &a.id).cmp(&(&b.thread_id, &b.id)));
        for side in sides {
            let data =
                serde_json::to_vec(side).map_err(|_| AppError::store("关系来源版本计算失败。"))?;
            hash.update((data.len() as u64).to_be_bytes());
            hash.update(data);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn candidate_version_at(
    candidate: &RelationCandidate,
    revisions: &[(String, i64, i64)],
) -> Result<String, AppError> {
    let mut hash = Sha256::new();
    hash.update(super::candidates::CANDIDATE_RULE_VERSION);
    hash.update(super::facts::RULE_VERSION);
    hash.update(serde_json::to_vec(candidate).map_err(|_| AppError::store("候选版本计算失败。"))?);
    for thread_id in [&candidate.left_thread_id, &candidate.right_thread_id] {
        let (_, updated_at, generation) = revisions
            .iter()
            .find(|(id, _, _)| id == thread_id)
            .ok_or_else(|| AppError::store("候选来源版本缺失。"))?;
        hash.update(updated_at.to_be_bytes());
        hash.update(generation.to_be_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn outcome(
    _service: &SourceService,
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
        let picked = selection.and_then(|selection| {
            selection
                .choices
                .iter()
                .find(|item| item.relation_key == choice.key)
        });
        let evidence_pair = picked
            .and_then(|picked| picked.pair_id.as_ref())
            .and_then(|pair_id| {
                candidate
                    .evidence
                    .pairs
                    .iter()
                    .find(|pair| &pair.id == pair_id)
            })
            .filter(|pair| {
                pair.left.thread_id == candidate.left_thread_id
                    && pair.right.thread_id == candidate.right_thread_id
            })
            .map(|pair| EvidencePair {
                id: pair.id.clone(),
                left: pair.left.clone(),
                right: pair.right.clone(),
            });
        let time_check = CausalTimeCheck::Unverifiable;
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
            execution_stage: classification
                .execution
                .as_ref()
                .map_or(codexflow_domain::ExecutionStage::Unknown, |assessment| {
                    assessment.stage
                }),
            execution_outcome: classification
                .execution
                .as_ref()
                .map_or(codexflow_domain::ExecutionOutcome::Unknown, |assessment| {
                    assessment.outcome
                }),
            probabilities: choice.answer.probabilities.clone(),
            evidence_confidence: picked
                .map_or(choice.answer.confidence, |item| item.answer.confidence),
            evidence_probabilities: picked.map_or_else(
                || choice.answer.probabilities.clone(),
                |item| item.answer.probabilities.clone(),
            ),
            evidence_options: selection
                .map_or_else(Vec::new, |value| value.evidence_options.clone()),
            selected_evidence_option: picked.map(|item| item.answer.choice.clone()),
            evidence: evidence_pair,
            time_check,
            explanation: format!(
                "Jev 的结构化判断支持 {}；执行阶段 {:?}，执行结果 {:?}。",
                choice.kind.as_str(),
                classification
                    .execution
                    .as_ref()
                    .map(|assessment| assessment.stage),
                classification
                    .execution
                    .as_ref()
                    .map(|assessment| assessment.outcome),
            ),
            input_version: input_version.into(),
        });
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
        left_thread_id: candidate.left_thread_id.clone(),
        right_thread_id: candidate.right_thread_id.clone(),
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
