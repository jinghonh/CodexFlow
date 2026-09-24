use codexflow_domain::{
    GraphDiagnostic, GraphNode, ObservedRelation, ObservedRelationKind, ParentEndpoint,
    ProjectGraph, ProjectSessions, ThreadAttribution, ThreadMetadata,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn observe(
    threads: &[ThreadMetadata],
    attributions: &[ThreadAttribution],
) -> Vec<ObservedRelation> {
    let ownership: BTreeMap<_, _> = attributions
        .iter()
        .map(|item| (item.thread_id.as_str(), item.project_id.as_deref()))
        .collect();
    let known: BTreeSet<_> = threads.iter().map(|thread| thread.id.as_str()).collect();
    let mut relations = BTreeMap::new();
    for thread in threads {
        let Some(project_id) = ownership.get(thread.id.as_str()).copied().flatten() else {
            continue;
        };
        for (kind, field, parent) in references(thread) {
            let Some(parent) =
                parent.filter(|id| !id.trim().is_empty() && *id != thread.id.as_str())
            else {
                continue;
            };
            let parent_endpoint = if !known.contains(parent) {
                ParentEndpoint::Missing
            } else {
                match ownership.get(parent).copied().flatten() {
                    Some(id) if id == project_id => ParentEndpoint::InProject,
                    Some(_) => ParentEndpoint::OutsideProject,
                    None => ParentEndpoint::Unassigned,
                }
            };
            // Length prefixes make the identity unambiguous even if source IDs contain ':'.
            let id = format!(
                "observed:{}:{}:{}:{}:{}",
                kind.as_str(),
                parent.len(),
                parent,
                thread.id.len(),
                thread.id
            );
            relations.insert(
                id.clone(),
                ObservedRelation {
                    id,
                    project_id: project_id.to_owned(),
                    from_thread_id: parent.to_owned(),
                    to_thread_id: thread.id.clone(),
                    kind,
                    source: "observed".into(),
                    source_field: field.into(),
                    confidence: 1.0,
                    parent_endpoint,
                },
            );
        }
    }
    relations.into_values().collect()
}

pub(crate) fn project_graph(
    sessions: ProjectSessions,
    relations: Vec<ObservedRelation>,
) -> ProjectGraph {
    let mut nodes: BTreeMap<_, _> = sessions
        .threads
        .iter()
        .map(|item| {
            (
                item.thread.id.clone(),
                GraphNode {
                    id: item.thread.id.clone(),
                    title: item.thread.title.clone().or_else(|| {
                        (!item.thread.preview.is_empty()).then(|| item.thread.preview.clone())
                    }),
                    reference_only: false,
                },
            )
        })
        .collect();
    let mut diagnostics = Vec::new();
    for item in &sessions.threads {
        for (_, field, parent) in references(&item.thread) {
            if parent == Some(item.thread.id.as_str()) {
                diagnostics.push(GraphDiagnostic {
                    thread_id: item.thread.id.clone(),
                    source_field: field.into(),
                    referenced_thread_id: item.thread.id.clone(),
                    message: "来源引用了会话自身，已忽略自环。".into(),
                });
            }
        }
    }
    for relation in &relations {
        if relation.parent_endpoint != ParentEndpoint::InProject {
            nodes
                .entry(relation.from_thread_id.clone())
                .or_insert_with(|| GraphNode {
                    id: relation.from_thread_id.clone(),
                    title: None,
                    reference_only: true,
                });
        }
    }
    diagnostics
        .sort_by(|a, b| (&a.thread_id, &a.source_field).cmp(&(&b.thread_id, &b.source_field)));
    ProjectGraph {
        project: sessions.project,
        nodes: nodes.into_values().collect(),
        relations,
        derived_relations: Vec::new(),
        inferred_relations: Vec::new(),
        reviewed_relations: Vec::new(),
        inference_outcomes: Vec::new(),
        diagnostics,
    }
}

fn references(thread: &ThreadMetadata) -> [(ObservedRelationKind, &'static str, Option<&str>); 2] {
    [
        (
            ObservedRelationKind::ForkedFrom,
            "forkedFromId",
            thread.forked_from_id.as_deref(),
        ),
        (
            ObservedRelationKind::SubagentOf,
            "parentThreadId",
            thread.parent_thread_id.as_deref(),
        ),
    ]
}
