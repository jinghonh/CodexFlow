use crate::SourceService;
use codexflow_domain::{AppError, AttributedThread};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ProjectThreadQuery {
    #[serde(default)]
    pub text: String,
    /// Keeps the selected Thread available when it does not match the filters.
    pub selected_thread_id: Option<String>,
    pub workstream_id: Option<String>,
    pub workspace_root: Option<String>,
    pub archived: Option<bool>,
    /// `true` requires complete metadata, turns and items.
    pub complete: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectThreadQueryResult {
    /// Matching rows in project session order, with no summary payload.
    pub matches: Vec<AttributedThread>,
    /// Selected row from the same snapshot when filters exclude it.
    pub selected: Option<AttributedThread>,
    /// Number of project Threads before filters are applied.
    pub total: usize,
}

fn matches_query(
    item: &AttributedThread,
    summary: Option<&str>,
    owner: Option<&str>,
    query: &ProjectThreadQuery,
) -> bool {
    let thread = &item.thread;
    let complete = thread.metadata_complete
        && thread.turns_complete
        && thread.items_complete
        && thread.content_complete
        && !thread.missing_from_source
        && thread.read_error.is_none();
    if query.archived.is_some_and(|value| value != thread.archived)
        || query.complete.is_some_and(|value| value != complete)
        || query
            .workspace_root
            .as_deref()
            .is_some_and(|value| item.attribution.workspace_root.as_deref() != Some(value))
        || query.workstream_id.as_deref().is_some_and(|value| {
            if value == "ungrouped" {
                owner.is_some()
            } else {
                owner != Some(value)
            }
        })
    {
        return false;
    }
    let needle = query.text.trim().to_lowercase();
    needle.is_empty()
        || [
            thread.title.as_deref(),
            Some(thread.preview.as_str()),
            Some(thread.id.as_str()),
            summary,
        ]
        .into_iter()
        .flatten()
        .any(|value| value.to_lowercase().contains(&needle))
}

impl SourceService {
    pub fn query_project_threads(
        &self,
        project_id: &str,
        query: ProjectThreadQuery,
    ) -> Result<ProjectThreadQueryResult, AppError> {
        let snapshot = self.sessions.project_thread_query_snapshot(project_id)?;
        // Search only summaries that describe this thread's current source and history.
        let summaries: BTreeMap<_, _> = snapshot
            .threads
            .iter()
            .filter_map(|record| {
                let summary = record.summary.as_ref()?;
                (summary.source_updated_at == record.thread.thread.updated_at
                    && summary.history_generation == record.history_generation)
                    .then(|| {
                        let content = &summary.content;
                        (
                            record.thread.thread.id.as_str(),
                            [
                                content.goal.as_str(),
                                content.activity.as_str(),
                                content.outcome.as_str(),
                                content.decisions.as_str(),
                                content.issues.as_str(),
                            ]
                            .join(" "),
                        )
                    })
            })
            .collect();
        // Filtering uses saved automatic groups and user corrections only. It never
        // recomputes groups or persists a new revision.
        let mut owners: BTreeMap<String, String> = snapshot
            .workstreams
            .iter()
            .flat_map(|stream| {
                stream
                    .members
                    .iter()
                    .map(move |id| (id.clone(), stream.id.clone()))
            })
            .collect();
        for (thread_id, target) in snapshot.corrections.members {
            if let Some(target) = target {
                owners.insert(thread_id, target);
            } else {
                owners.remove(&thread_id);
            }
        }
        let mut matches = Vec::new();
        let mut seen = BTreeSet::new();
        for record in &snapshot.threads {
            let item = &record.thread;
            if !seen.insert(item.thread.id.as_str()) {
                continue;
            }
            let summary = summaries.get(item.thread.id.as_str()).map(String::as_str);
            if matches_query(
                item,
                summary,
                owners.get(&item.thread.id).map(String::as_str),
                &query,
            ) {
                matches.push(item.clone());
            }
        }
        let selected = query.selected_thread_id.as_deref().and_then(|thread_id| {
            if matches.iter().any(|item| item.thread.id == thread_id) {
                return None;
            }
            snapshot
                .threads
                .iter()
                .find(|record| record.thread.thread.id == thread_id)
                .map(|record| record.thread.clone())
        });
        Ok(ProjectThreadQueryResult {
            total: snapshot.threads.len(),
            matches,
            selected,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexflow_domain::{
        HistoryCoverage, HistoryReadPath, HistorySnapshot, ThreadAttribution, ThreadMetadata,
        ThreadSummary, ThreadSummaryContent,
    };

    fn domain_snapshot(path: &std::path::Path) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
        let connection = rusqlite::Connection::open(path).unwrap();
        [
            "thread_attributions",
            "source_evidence",
            "observed_relations",
            "inferred_pair_outcomes",
            "relation_reviews",
            "workstreams",
            "workstream_name_corrections",
            "workstream_member_corrections",
        ]
        .into_iter()
        .map(|table| {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
                .unwrap();
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get(index))
                        .collect::<Result<Vec<_>, _>>()
                })
                .unwrap();
            (table.to_owned(), rows.map(Result::unwrap).collect())
        })
        .collect()
    }

    #[test]
    fn filters_are_pure_and_search_saved_summary() {
        let thread = ThreadMetadata {
            id: "t-1".into(),
            session_id: "s-1".into(),
            title: Some("实现索引".into()),
            preview: "读取历史".into(),
            cwd: "/repo/wt".into(),
            project_id: None,
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: None,
            forked_from_id: None,
            git: None,
            created_at: 0,
            updated_at: 0,
            archived: false,
            metadata_complete: true,
            turns_complete: true,
            items_complete: true,
            missing_from_source: false,
            content_complete: true,
            read_error: None,
            observed_at_unix_ms: 0,
        };
        let item = AttributedThread {
            thread,
            attribution: ThreadAttribution {
                thread_id: "t-1".into(),
                project_id: Some("p".into()),
                workspace_root: Some("/repo/wt".into()),
                basis: "git".into(),
                detail: "git".into(),
                diagnostic: None,
                source_project_id: None,
            },
        };
        let before = serde_json::to_value(&item).unwrap();
        let query = ProjectThreadQuery {
            text: "修复缓存".into(),
            selected_thread_id: None,
            workstream_id: Some("w".into()),
            workspace_root: Some("/repo/wt".into()),
            archived: Some(false),
            complete: Some(true),
        };
        assert!(matches_query(
            &item,
            Some("修复缓存失效"),
            Some("w"),
            &query
        ));
        assert!(!matches_query(
            &item,
            Some("修复缓存失效"),
            Some("other"),
            &query
        ));
        assert!(!matches_query(
            &item,
            Some("修复缓存失效"),
            Some("w"),
            &ProjectThreadQuery {
                archived: Some(true),
                ..query
            }
        ));
        assert_eq!(serde_json::to_value(&item).unwrap(), before);
    }

    #[test]
    fn public_query_only_changes_its_result() {
        let root = std::env::temp_dir().join(format!(
            "codexflow-explorer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let workspace = root.join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let service = SourceService::new(root.join("data")).unwrap();
        let thread = ThreadMetadata {
            id: "thread-1".into(),
            session_id: "session-1".into(),
            title: Some("建立索引".into()),
            preview: "检查本地历史".into(),
            cwd: workspace.to_string_lossy().into_owned(),
            project_id: None,
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: None,
            forked_from_id: None,
            git: None,
            created_at: 1,
            updated_at: 2,
            archived: false,
            metadata_complete: true,
            turns_complete: false,
            items_complete: false,
            missing_from_source: false,
            content_complete: false,
            read_error: None,
            observed_at_unix_ms: 3,
        };
        let mut child = thread.clone();
        child.id = "thread-2".into();
        child.session_id = "session-2".into();
        child.title = Some("继续索引".into());
        child.parent_thread_id = Some("thread-1".into());
        let mut source_stale = thread.clone();
        source_stale.id = "thread-3".into();
        source_stale.session_id = "session-3".into();
        source_stale.title = Some("浏览来源".into());
        service
            .sessions
            .save_collection(&[thread, child.clone(), source_stale.clone()], &[])
            .unwrap();
        let project_id = service
            .choose_project(workspace.to_str().unwrap())
            .unwrap()
            .selected_project_id
            .unwrap();
        let summary = ThreadSummary {
            thread_id: "thread-1".into(),
            content: ThreadSummaryContent {
                goal: "修复缓存失效".into(),
                activity: "读取记录".into(),
                outcome: "完成".into(),
                decisions: "保留事实".into(),
                issues: "无".into(),
            },
            evidence_ids: vec![],
            evidence_refs: vec![],
            model: "test".into(),
            requested_model: None,
            service_base_url: None,
            binary_path: None,
            binary_fingerprint: None,
            binary_version: None,
            input_digest: "test".into(),
            source_updated_at: 2,
            history_generation: 0,
            created_at_unix_ms: 4,
        };
        assert!(service
            .sessions
            .save_summary_if_current(&summary, 0)
            .unwrap());
        let mut generation_stale = summary.clone();
        generation_stale.thread_id = "thread-2".into();
        generation_stale.content.goal = "历史代次过期摘要词".into();
        assert!(service
            .sessions
            .save_summary_if_current(&generation_stale, 0)
            .unwrap());
        let mut source_stale_summary = summary.clone();
        source_stale_summary.thread_id = "thread-3".into();
        source_stale_summary.content.goal = "来源版本过期摘要词".into();
        assert!(service
            .sessions
            .save_summary_if_current(&source_stale_summary, 0)
            .unwrap());
        service
            .sessions
            .save_history(&HistorySnapshot {
                coverage: HistoryCoverage {
                    thread_id: "thread-2".into(),
                    source_updated_at: 2,
                    attempted_at_unix_ms: 5,
                    path: HistoryReadPath::FullRead,
                    turns_complete: true,
                    items_complete: true,
                    turn_pages: 0,
                    item_pages: 0,
                    loaded_turns: 0,
                    loaded_items: 0,
                    incompatible: false,
                    error: None,
                },
                turns: vec![],
                items: vec![],
            })
            .unwrap();
        source_stale.updated_at = 4;
        service
            .sessions
            .save_collection(&[source_stale], &[])
            .unwrap();
        let before_sessions =
            serde_json::to_value(service.project_sessions(&project_id).unwrap()).unwrap();
        let before_domain = domain_snapshot(&root.join("data/sessions.sqlite3"));
        let before_streams = service.sessions.workstream_snapshot(&project_id).unwrap();
        let before_relations =
            serde_json::to_value(service.sessions.observed_relations(&project_id).unwrap())
                .unwrap();
        let query = ProjectThreadQuery {
            text: "修复缓存".into(),
            ..Default::default()
        };
        let found = service.query_project_threads(&project_id, query).unwrap();
        assert_eq!(found.total, 3);
        assert_eq!(found.matches.len(), 1);
        assert_eq!(found.matches[0].thread.id, "thread-1");
        let first = &found.matches[0];
        assert_eq!(
            first.attribution.workspace_root,
            Some(
                std::fs::canonicalize(&workspace)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            )
        );
        assert!(service
            .query_project_threads(
                &project_id,
                ProjectThreadQuery {
                    text: "历史代次过期摘要词".into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .matches
            .is_empty());
        assert!(service
            .query_project_threads(
                &project_id,
                ProjectThreadQuery {
                    text: "来源版本过期摘要词".into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .matches
            .is_empty());
        assert_eq!(
            service
                .query_project_threads(
                    &project_id,
                    ProjectThreadQuery {
                        text: "继续索引".into(),
                        ..Default::default()
                    },
                )
                .unwrap()
                .matches
                .iter()
                .map(|item| item.thread.id.as_str())
                .collect::<Vec<_>>(),
            ["thread-2"]
        );
        let selected_while_filtered = service
            .query_project_threads(
                &project_id,
                ProjectThreadQuery {
                    archived: Some(true),
                    selected_thread_id: Some("thread-2".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(selected_while_filtered.matches.is_empty());
        assert_eq!(
            selected_while_filtered.selected.as_ref().unwrap().thread.id,
            "thread-2"
        );
        let hidden = service
            .query_project_threads(
                &project_id,
                ProjectThreadQuery {
                    archived: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(hidden.matches.is_empty());
        assert_eq!(
            serde_json::to_value(service.project_sessions(&project_id).unwrap()).unwrap(),
            before_sessions
        );
        let after_streams = service.sessions.workstream_snapshot(&project_id).unwrap();
        assert_eq!(
            serde_json::to_value(after_streams.0).unwrap(),
            serde_json::to_value(before_streams.0).unwrap()
        );
        assert_eq!(after_streams.1.revision, before_streams.1.revision);
        assert_eq!(after_streams.1.names, before_streams.1.names);
        assert_eq!(after_streams.1.members, before_streams.1.members);
        assert_eq!(
            serde_json::to_value(service.sessions.observed_relations(&project_id).unwrap())
                .unwrap(),
            before_relations
        );
        assert_eq!(
            service
                .sessions
                .summary("thread-1")
                .unwrap()
                .unwrap()
                .content
                .goal,
            "修复缓存失效"
        );
        assert_eq!(
            domain_snapshot(&root.join("data/sessions.sqlite3")),
            before_domain
        );
        let streams = service.project_workstreams(&project_id).unwrap();
        let owner = streams
            .workstreams
            .iter()
            .find(|stream| stream.members.contains(&"thread-1".to_owned()))
            .unwrap()
            .id
            .clone();
        service
            .move_thread_to_workstream(&project_id, "thread-1", None, streams.revision)
            .unwrap();
        let ungrouped = service
            .query_project_threads(
                &project_id,
                ProjectThreadQuery {
                    workstream_id: Some("ungrouped".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            ungrouped
                .matches
                .iter()
                .map(|item| item.thread.id.as_str())
                .collect::<Vec<_>>(),
            ["thread-3", "thread-1"]
        );
        let grouped = service
            .query_project_threads(
                &project_id,
                ProjectThreadQuery {
                    workstream_id: Some(owner),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            grouped
                .matches
                .iter()
                .map(|item| item.thread.id.as_str())
                .collect::<Vec<_>>(),
            ["thread-2"]
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
