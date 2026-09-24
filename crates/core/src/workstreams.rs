use codexflow_domain::{
    DerivedRelationKind, ParentEndpoint, ProjectGraph, ProjectWorkstreams, UserRelationDecision,
    Workstream,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const ALGORITHM_VERSION: &str = "greedy-modularity-v1-seed-0";

#[derive(Clone)]
struct Edge {
    id: String,
    left: String,
    right: String,
    weight: i64,
}

pub(crate) fn build(graph: &ProjectGraph, old: &[Workstream]) -> ProjectWorkstreams {
    let project_id = graph.project.id.clone();
    let nodes: BTreeSet<_> = graph
        .nodes
        .iter()
        .filter(|node| !node.reference_only)
        .map(|node| node.id.clone())
        .collect();
    let mut edges = Vec::new();
    let mut add = |id: &str, a: &str, b: &str, weight| {
        if a != b && nodes.contains(a) && nodes.contains(b) {
            let (left, right) = if a < b { (a, b) } else { (b, a) };
            edges.push(Edge {
                id: id.into(),
                left: left.into(),
                right: right.into(),
                weight,
            });
        }
    };
    for relation in &graph.relations {
        if relation.parent_endpoint == ParentEndpoint::InProject {
            add(
                &relation.id,
                &relation.from_thread_id,
                &relation.to_thread_id,
                4,
            );
        }
    }
    for relation in &graph.derived_relations {
        let from_has_evidence = relation
            .evidence
            .iter()
            .any(|item| item.thread_id == relation.from_thread_id);
        let to_has_evidence = relation
            .evidence
            .iter()
            .any(|item| item.thread_id == relation.to_thread_id);
        let evidence_valid = match relation.kind {
            DerivedRelationKind::ExplicitReference => from_has_evidence || to_has_evidence,
            DerivedRelationKind::SharedFile | DerivedRelationKind::SharedArtifact => {
                from_has_evidence && to_has_evidence
            }
        };
        if !relation.basis.trim().is_empty() && evidence_valid {
            add(
                &relation.id,
                &relation.from_thread_id,
                &relation.to_thread_id,
                2,
            );
        }
    }
    for relation in &graph.reviewed_relations {
        if relation.evidence_valid
            && relation.review.decision != UserRelationDecision::Rejected
            && (relation.relation.confidence >= 0.70
                || (relation.review.decision == UserRelationDecision::Confirmed
                    && relation.review.confirmed_evidence_version.as_deref()
                        == Some(&relation.evidence_version)))
        {
            add(
                &relation.relation.id,
                &relation.relation.from_thread_id,
                &relation.relation.to_thread_id,
                3,
            );
        }
    }
    edges.sort_by(|a, b| (&a.left, &a.right, &a.id).cmp(&(&b.left, &b.right, &b.id)));
    let total_weight: i64 = edges.iter().map(|edge| edge.weight).sum();
    let mut communities: BTreeMap<String, BTreeSet<String>> = nodes
        .iter()
        .map(|id| (id.clone(), BTreeSet::from([id.clone()])))
        .collect();
    // Greedy modularity gain is 2m*w(A,B) - degree(A)*degree(B).
    // A sparse bridge between internally dense groups therefore remains a cross-group edge.
    while total_weight > 0 {
        let mut degree: BTreeMap<String, i64> =
            communities.keys().map(|id| (id.clone(), 0)).collect();
        let mut between: BTreeMap<(String, String), i64> = BTreeMap::new();
        let owner: BTreeMap<_, _> = communities
            .iter()
            .flat_map(|(id, members)| {
                members
                    .iter()
                    .map(move |member| (member.as_str(), id.as_str()))
            })
            .collect();
        for edge in &edges {
            let left = owner[edge.left.as_str()];
            let right = owner[edge.right.as_str()];
            *degree.get_mut(left).unwrap() += edge.weight;
            *degree.get_mut(right).unwrap() += edge.weight;
            if left != right {
                let pair = if left < right {
                    (left, right)
                } else {
                    (right, left)
                };
                *between.entry((pair.0.into(), pair.1.into())).or_default() += edge.weight;
            }
        }
        let best = between
            .into_iter()
            .filter_map(|((left, right), weight)| {
                let gain = 2 * total_weight * weight - degree[&left] * degree[&right];
                (gain > 0).then_some((gain, left, right))
            })
            .max_by(|a, b| a.0.cmp(&b.0).then_with(|| (&b.1, &b.2).cmp(&(&a.1, &a.2))));
        let Some((_, left, right)) = best else { break };
        let members = communities.remove(&right).unwrap();
        communities.get_mut(&left).unwrap().extend(members);
    }
    let mut groups: Vec<Vec<String>> = communities
        .into_values()
        .filter(|members| members.len() > 1)
        .map(|members| members.into_iter().collect())
        .collect();
    groups.sort();
    let mut matched = BTreeMap::new();
    let mut used_old = BTreeSet::new();
    let mut options = Vec::new();
    for (index, members) in groups.iter().enumerate() {
        for previous in old {
            let overlap = members
                .iter()
                .filter(|id| previous.members.contains(id))
                .count();
            if overlap > 0 {
                options.push((overlap, previous.id.clone(), index));
            }
        }
    }
    options.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| groups[a.2].cmp(&groups[b.2]))
    });
    for (_, old_id, index) in options {
        if !matched.contains_key(&index) && used_old.insert(old_id.clone()) {
            matched.insert(index, old_id);
        }
    }
    let mut workstreams = Vec::new();
    let mut owners = BTreeMap::new();
    for (index, members) in groups.into_iter().enumerate() {
        let mut hasher = Sha256::new();
        hasher.update(ALGORITHM_VERSION.as_bytes());
        hasher.update(project_id.as_bytes());
        for id in &members {
            hasher.update((id.len() as u64).to_be_bytes());
            hasher.update(id.as_bytes());
        }
        let generated = format!("workstream-{:x}", hasher.finalize());
        let id = matched.get(&index).cloned().unwrap_or(generated);
        let prior = old.iter().find(|item| item.id == id);
        let mut predecessor_ids: Vec<_> = old
            .iter()
            .filter(|item| {
                item.id != id && item.members.iter().any(|member| members.contains(member))
            })
            .map(|item| item.id.clone())
            .collect();
        if let Some(prior) = prior {
            predecessor_ids.extend(prior.predecessor_ids.iter().cloned());
        }
        predecessor_ids.sort();
        predecessor_ids.dedup();
        let keep_name =
            prior.is_some_and(|item| item.members == members && item.name_input_version.is_some());
        let name_error = prior
            .filter(|item| item.members == members)
            .and_then(|item| item.name_error.clone());
        let mut item = Workstream {
            id: id.clone(),
            project_id: project_id.clone(),
            name: if keep_name {
                prior.unwrap().name.clone()
            } else {
                format!("工作流 {}", &id[id.len().saturating_sub(8)..])
            },
            members,
            relation_ids: Vec::new(),
            algorithm_version: ALGORITHM_VERSION.into(),
            name_input_version: if keep_name {
                prior.unwrap().name_input_version.clone()
            } else {
                None
            },
            name_actual_model: if keep_name {
                prior.unwrap().name_actual_model.clone()
            } else {
                None
            },
            name_error,
            predecessor_ids,
        };
        for member in &item.members {
            owners.insert(member.clone(), id.clone());
        }
        for edge in &edges {
            if item.members.binary_search(&edge.left).is_ok()
                && item.members.binary_search(&edge.right).is_ok()
            {
                item.relation_ids.push(edge.id.clone());
            }
        }
        item.relation_ids.sort();
        item.relation_ids.dedup();
        workstreams.push(item);
    }
    workstreams.sort_by(|a, b| a.id.cmp(&b.id));
    let ungrouped_thread_ids = nodes
        .into_iter()
        .filter(|id| !owners.contains_key(id))
        .collect();
    let cross_relation_ids = edges
        .into_iter()
        .filter(|edge| owners.get(&edge.left) != owners.get(&edge.right))
        .map(|edge| edge.id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    ProjectWorkstreams {
        project_id,
        workstreams,
        ungrouped_thread_ids,
        cross_relation_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexflow_domain::{
        CandidateEvidence, CausalTimeCheck, DerivedRelation, DerivedRelationKind, EvidenceField,
        EvidencePair, GraphNode, InferredRelation, InferredRelationKind, LocalProject,
        ObservedRelation, ObservedRelationKind, RelationReview, ReviewedInferredRelation,
    };

    fn graph(ids: &[&str]) -> ProjectGraph {
        ProjectGraph {
            project: LocalProject {
                id: "project".into(),
                name: "项目".into(),
                root: "/project".into(),
                git_common_dir: None,
            },
            nodes: ids
                .iter()
                .map(|id| GraphNode {
                    id: (*id).into(),
                    title: None,
                    reference_only: false,
                })
                .collect(),
            relations: Vec::new(),
            derived_relations: Vec::new(),
            inferred_relations: Vec::new(),
            reviewed_relations: Vec::new(),
            inference_outcomes: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn observed(graph: &mut ProjectGraph, id: &str, a: &str, b: &str) {
        graph.relations.push(ObservedRelation {
            id: id.into(),
            project_id: "project".into(),
            from_thread_id: a.into(),
            to_thread_id: b.into(),
            kind: ObservedRelationKind::ForkedFrom,
            source: "observed".into(),
            source_field: "forkedFromId".into(),
            confidence: 1.0,
            parent_endpoint: ParentEndpoint::InProject,
        });
    }

    fn evidence(thread: &str) -> CandidateEvidence {
        CandidateEvidence {
            id: format!("e-{thread}"),
            thread_id: thread.into(),
            turn_id: "turn".into(),
            item_id: "item".into(),
            field: EvidenceField::Text,
            change_index: None,
            excerpt: "具体事实".into(),
            content_version: "v1".into(),
            fact_id: None,
        }
    }

    fn derived(graph: &mut ProjectGraph, id: &str, a: &str, b: &str) {
        graph.derived_relations.push(DerivedRelation {
            id: id.into(),
            project_id: "project".into(),
            from_thread_id: a.into(),
            to_thread_id: b.into(),
            kind: DerivedRelationKind::SharedFile,
            source: "derived".into(),
            basis: "共同修改同一文件".into(),
            evidence: vec![evidence(a), evidence(b)],
        });
    }

    #[test]
    fn explicit_reference_with_source_only_on_referring_thread_forms_a_group() {
        let mut input = graph(&["referrer", "referenced"]);
        input.derived_relations.push(DerivedRelation {
            id: "explicit".into(),
            project_id: "project".into(),
            from_thread_id: "referrer".into(),
            to_thread_id: "referenced".into(),
            kind: DerivedRelationKind::ExplicitReference,
            source: "derived".into(),
            basis: "来源条目明确引用另一条会话标识。".into(),
            evidence: vec![evidence("referrer")],
        });
        let result = build(&input, &[]);
        assert_eq!(result.workstreams.len(), 1);
        assert_eq!(result.workstreams[0].relation_ids, ["explicit"]);
        assert!(result.ungrouped_thread_ids.is_empty());
    }

    fn inferred(
        graph: &mut ProjectGraph,
        id: &str,
        a: &str,
        b: &str,
        confidence: f64,
        decision: UserRelationDecision,
        valid: bool,
    ) {
        let relation = InferredRelation {
            id: id.into(),
            project_id: "project".into(),
            candidate_id: "candidate".into(),
            from_thread_id: a.into(),
            to_thread_id: b.into(),
            kind: InferredRelationKind::Related,
            source: "jev".into(),
            requested_model: "jev".into(),
            actual_model: "jev".into(),
            confidence,
            probabilities: BTreeMap::new(),
            evidence_confidence: 0.9,
            evidence_probabilities: BTreeMap::new(),
            evidence: EvidencePair {
                id: "pair".into(),
                left: evidence(a),
                right: evidence(b),
            },
            time_check: CausalTimeCheck::Unverifiable,
            explanation: "来源事实".into(),
            input_version: "v1".into(),
        };
        graph.reviewed_relations.push(ReviewedInferredRelation {
            relation,
            evidence_version: "ev1".into(),
            evidence_valid: valid,
            review: RelationReview {
                relation_id: id.into(),
                project_id: "project".into(),
                decision,
                revision: 1,
                confirmed_evidence_version: Some("ev1".into()),
            },
        });
    }

    #[test]
    fn communities_keep_sparse_bridge_and_isolated_thread_visible() {
        let mut input = graph(&["a", "b", "c", "d", "alone"]);
        observed(&mut input, "ab", "a", "b");
        observed(&mut input, "cd", "c", "d");
        derived(&mut input, "bridge", "b", "c");
        let result = build(&input, &[]);
        let members: BTreeSet<_> = result
            .workstreams
            .iter()
            .map(|item| item.members.clone())
            .collect();
        assert_eq!(
            members,
            BTreeSet::from([
                vec!["a".to_owned(), "b".to_owned()],
                vec!["c".to_owned(), "d".to_owned()]
            ])
        );
        assert_eq!(result.ungrouped_thread_ids, ["alone"]);
        assert_eq!(result.cross_relation_ids, ["bridge"]);
        assert_eq!(
            build(&input, &result.workstreams).workstreams,
            result.workstreams
        );
        input.nodes.reverse();
        input.relations.reverse();
        input.derived_relations.reverse();
        assert_eq!(build(&input, &[]).workstreams, result.workstreams);
    }

    #[test]
    fn rule_and_valid_inference_group_but_rejection_or_stale_evidence_do_not() {
        let mut input = graph(&["a", "b", "c", "d", "e", "f", "g", "h"]);
        derived(&mut input, "rule", "a", "b");
        inferred(
            &mut input,
            "visible",
            "c",
            "d",
            0.8,
            UserRelationDecision::Pending,
            true,
        );
        inferred(
            &mut input,
            "confirmed",
            "e",
            "f",
            0.2,
            UserRelationDecision::Confirmed,
            true,
        );
        inferred(
            &mut input,
            "rejected",
            "g",
            "h",
            0.9,
            UserRelationDecision::Rejected,
            true,
        );
        inferred(
            &mut input,
            "stale",
            "b",
            "g",
            0.9,
            UserRelationDecision::Confirmed,
            false,
        );
        let result = build(&input, &[]);
        assert_eq!(result.workstreams.len(), 3);
        assert_eq!(result.ungrouped_thread_ids, ["g", "h"]);
        assert!(!result.cross_relation_ids.contains(&"rejected".to_owned()));
        assert!(!result.cross_relation_ids.contains(&"stale".to_owned()));
    }

    #[test]
    fn identity_matching_uses_largest_overlap_once_and_stable_ties() {
        let mut initial = graph(&["a", "b", "c", "d"]);
        observed(&mut initial, "ab", "a", "b");
        observed(&mut initial, "cd", "c", "d");
        let first = build(&initial, &[]);
        let old = first.workstreams.clone();
        let mut changed = graph(&["a", "b", "c", "d"]);
        observed(&mut changed, "ac", "a", "c");
        observed(&mut changed, "bd", "b", "d");
        let next = build(&changed, &old);
        assert_eq!(next.workstreams.len(), 2);
        assert_eq!(
            next.workstreams
                .iter()
                .map(|item| item.id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
        assert_eq!(
            next.workstreams
                .iter()
                .map(|item| item.id.as_str())
                .collect::<BTreeSet<_>>(),
            old.iter().map(|item| item.id.as_str()).collect()
        );
        assert_eq!(
            build(&changed, &next.workstreams).workstreams,
            next.workstreams
        );
        let mut all_connected = graph(&["a", "b", "c", "d"]);
        for (id, a, b) in [
            ("ab", "a", "b"),
            ("ac", "a", "c"),
            ("ad", "a", "d"),
            ("bc", "b", "c"),
            ("bd", "b", "d"),
            ("cd", "c", "d"),
        ] {
            observed(&mut all_connected, id, a, b);
        }
        let merged = build(&all_connected, &old);
        assert_eq!(merged.workstreams.len(), 1);
        assert!(old.iter().any(|item| item.id == merged.workstreams[0].id));
        assert_eq!(merged.workstreams[0].predecessor_ids.len(), 1);
    }
}
