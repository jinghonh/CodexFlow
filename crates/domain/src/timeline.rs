use crate::{HistoryTurn, ProjectSessions};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// JavaScript Date's supported range; the desktop must be able to show every returned instant.
const MAX_DISPLAY_UNIX_MS: i64 = 8_640_000_000_000_000;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TimelineQuality {
    Complete,
    Partial,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnTimeState {
    Complete,
    DurationOnly,
    Running,
    Incomplete,
    Invalid,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LastActivityBasis {
    TurnEnd,
    TurnStart,
    MetadataUpdate,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineTurn {
    pub id: String,
    pub ordinal: u64,
    pub source_status: String,
    pub time_state: TurnTimeState,
    pub started_at_unix_ms: Option<i64>,
    pub completed_at_unix_ms: Option<i64>,
    /// Present only when both source bounds form a valid interval.
    pub interval_start_unix_ms: Option<i64>,
    pub interval_end_unix_ms: Option<i64>,
    /// Source duration only; never derived from bounds.
    pub duration_ms: Option<i64>,
    pub time_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineThread {
    pub thread_id: String,
    pub title: String,
    pub turns_complete: bool,
    pub quality: TimelineQuality,
    pub known_duration_ms: Option<i64>,
    pub last_activity_at_unix_ms: Option<i64>,
    pub last_activity_basis: LastActivityBasis,
    pub turns: Vec<TimelineTurn>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTimeline {
    pub project_id: String,
    pub threads: Vec<TimelineThread>,
}

fn valid_instant(value: Option<i64>) -> Option<i64> {
    value.filter(|value| (-MAX_DISPLAY_UNIX_MS..=MAX_DISPLAY_UNIX_MS).contains(value))
}

fn interpret(turn: HistoryTurn) -> TimelineTurn {
    let started = valid_instant(turn.started_at_unix_ms);
    let completed = valid_instant(turn.completed_at_unix_ms);
    let duration = turn
        .duration_ms
        .filter(|value| (0..=MAX_SAFE_INTEGER).contains(value));
    let mut errors = Vec::new();
    if let Some(error) = turn.time_error {
        errors.push(error);
    }
    if turn.started_at_unix_ms.is_some() && started.is_none() {
        errors.push("开始时间超出可显示范围".to_owned());
    }
    if turn.completed_at_unix_ms.is_some() && completed.is_none() {
        errors.push("结束时间超出可显示范围".to_owned());
    }
    if turn.duration_ms.is_some() && duration.is_none() {
        errors.push("时长为负数或超出可精确显示范围".to_owned());
    }
    if let (Some(start), Some(end)) = (started, completed) {
        if end < start {
            errors.push("结束时间早于开始时间".to_owned());
        }
    }
    let interval = match (started, completed) {
        (Some(start), Some(end)) if end >= start => Some((start, end)),
        _ => None,
    };
    let running = turn.status == "inProgress";
    let time_state = if !errors.is_empty() {
        TurnTimeState::Invalid
    } else if interval.is_some() {
        TurnTimeState::Complete
    } else if running && completed.is_none() {
        TurnTimeState::Running
    } else if started.is_some() || completed.is_some() {
        TurnTimeState::Incomplete
    } else if duration.is_some() {
        TurnTimeState::DurationOnly
    } else {
        TurnTimeState::Unknown
    };
    TimelineTurn {
        id: turn.id,
        ordinal: turn.ordinal,
        source_status: turn.status,
        time_state,
        started_at_unix_ms: started,
        completed_at_unix_ms: completed,
        interval_start_unix_ms: interval.map(|(start, _)| start),
        interval_end_unix_ms: interval.map(|(_, end)| end),
        duration_ms: duration,
        time_error: (!errors.is_empty()).then(|| errors.join("；")),
    }
}

pub fn build_project_timeline(
    sessions: ProjectSessions,
    turns: Vec<HistoryTurn>,
) -> ProjectTimeline {
    let mut by_thread: HashMap<String, Vec<HistoryTurn>> = HashMap::new();
    for turn in turns {
        by_thread
            .entry(turn.thread_id.clone())
            .or_default()
            .push(turn);
    }
    let threads = sessions
        .threads
        .into_iter()
        .map(|item| {
            let metadata = item.thread;
            let mut turns = by_thread.remove(&metadata.id).unwrap_or_default();
            turns.sort_by(|a, b| (a.ordinal, &a.id).cmp(&(b.ordinal, &b.id)));
            let turns: Vec<_> = turns.into_iter().map(interpret).collect();
            let turns_complete = metadata.turns_complete;
            let mut known_duration = None::<i128>;
            for turn in &turns {
                if let Some(duration) = turn.duration_ms {
                    known_duration = Some(known_duration.unwrap_or(0) + i128::from(duration));
                }
            }
            let known_duration_ms = known_duration
                .filter(|sum| *sum <= i128::from(MAX_SAFE_INTEGER))
                .and_then(|sum| i64::try_from(sum).ok());
            let all_complete = turns_complete
                && !turns.is_empty()
                && known_duration_ms.is_some()
                && turns.iter().all(|turn| {
                    turn.time_state == TurnTimeState::Complete && turn.duration_ms.is_some()
                });
            let quality = if all_complete {
                TimelineQuality::Complete
            } else if turns.iter().any(|turn| {
                turn.duration_ms.is_some()
                    || turn.interval_start_unix_ms.is_some()
                    || turn.started_at_unix_ms.is_some()
                    || turn.completed_at_unix_ms.is_some()
            }) {
                TimelineQuality::Partial
            } else {
                TimelineQuality::Unknown
            };
            let last_end = turns
                .iter()
                .filter_map(|turn| turn.interval_end_unix_ms)
                .max();
            let last_start = turns
                .iter()
                .filter(|turn| turn.time_error.is_none())
                .filter_map(|turn| turn.started_at_unix_ms)
                .max();
            let (last_activity_at_unix_ms, last_activity_basis) = if let Some(end) = last_end {
                if last_start.is_some_and(|start| start > end) {
                    (last_start, LastActivityBasis::TurnStart)
                } else {
                    (Some(end), LastActivityBasis::TurnEnd)
                }
            } else if let Some(start) = last_start {
                (Some(start), LastActivityBasis::TurnStart)
            } else if let Some(updated) = metadata
                .updated_at
                .checked_mul(1000)
                .and_then(|value| valid_instant(Some(value)))
            {
                (Some(updated), LastActivityBasis::MetadataUpdate)
            } else {
                (None, LastActivityBasis::Unknown)
            };
            TimelineThread {
                thread_id: metadata.id.clone(),
                title: metadata
                    .title
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| {
                        if metadata.preview.is_empty() {
                            metadata.id
                        } else {
                            metadata.preview
                        }
                    }),
                turns_complete,
                quality,
                known_duration_ms,
                last_activity_at_unix_ms,
                last_activity_basis,
                turns,
            }
        })
        .collect();
    ProjectTimeline {
        project_id: sessions.project.id,
        threads,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttributedThread, LocalProject, ThreadAttribution, ThreadMetadata};

    fn turn(
        id: &str,
        ordinal: u64,
        start: Option<i64>,
        end: Option<i64>,
        duration: Option<i64>,
        status: &str,
    ) -> HistoryTurn {
        HistoryTurn {
            thread_id: "thread".into(),
            id: id.into(),
            ordinal,
            status: status.into(),
            started_at_unix_ms: start,
            completed_at_unix_ms: end,
            duration_ms: duration,
            time_error: None,
            source_updated_at: 1,
            content_version: "v".into(),
        }
    }

    fn sessions(complete: bool) -> ProjectSessions {
        ProjectSessions {
            project: LocalProject {
                id: "project".into(),
                name: "项目".into(),
                root: "/tmp/project".into(),
                git_common_dir: None,
            },
            workspaces: vec![],
            scopes: vec![],
            threads: vec![AttributedThread {
                thread: ThreadMetadata {
                    id: "thread".into(),
                    session_id: "session".into(),
                    title: Some("会话".into()),
                    preview: "".into(),
                    cwd: "/tmp/project".into(),
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
                    turns_complete: complete,
                    items_complete: false,
                    missing_from_source: false,
                    content_complete: false,
                    read_error: None,
                    observed_at_unix_ms: 0,
                },
                attribution: ThreadAttribution {
                    thread_id: "thread".into(),
                    project_id: Some("project".into()),
                    workspace_root: None,
                    basis: "test".into(),
                    detail: "test".into(),
                    diagnostic: None,
                    source_project_id: None,
                },
            }],
        }
    }

    #[test]
    fn separated_and_overlapping_turns_keep_source_intervals_and_duration_sum() {
        let day = 86_400_000;
        let view = build_project_timeline(
            sessions(true),
            vec![
                turn(
                    "first",
                    0,
                    Some(1_000),
                    Some(3_000),
                    Some(2_000),
                    "completed",
                ),
                turn(
                    "second",
                    1,
                    Some(day + 1_000),
                    Some(day + 4_000),
                    Some(3_000),
                    "completed",
                ),
                turn(
                    "parallel",
                    2,
                    Some(day + 2_000),
                    Some(day + 3_000),
                    Some(1_000),
                    "completed",
                ),
            ],
        );
        let thread = &view.threads[0];
        assert_eq!(thread.quality, TimelineQuality::Complete);
        assert_eq!(thread.known_duration_ms, Some(6_000));
        assert_eq!(thread.turns[0].interval_end_unix_ms, Some(3_000));
        assert_eq!(thread.turns[1].interval_start_unix_ms, Some(day + 1_000));
        assert_eq!(thread.turns[2].interval_start_unix_ms, Some(day + 2_000));
        assert_eq!(thread.last_activity_at_unix_ms, Some(day + 4_000));
        assert_eq!(thread.last_activity_basis, LastActivityBasis::TurnEnd);
    }

    #[test]
    fn partial_missing_running_and_invalid_time_do_not_invent_positions_or_duration() {
        let mut invalid = turn(
            "invalid",
            5,
            Some(8_000),
            Some(7_000),
            Some(400),
            "completed",
        );
        invalid.time_error = Some("来源开始时间无效".into());
        let view = build_project_timeline(
            sessions(false),
            vec![
                turn("duration", 0, None, None, Some(1_500), "completed"),
                turn("bounds", 1, Some(1_000), Some(2_000), None, "completed"),
                turn("same", 2, Some(3_000), Some(3_000), Some(0), "completed"),
                turn("running", 3, Some(4_000), None, None, "inProgress"),
                turn("ended", 4, Some(5_000), None, None, "completed"),
                invalid,
                turn("negative", 6, None, None, Some(-4), "completed"),
                turn(
                    "bad-duration",
                    7,
                    Some(900),
                    Some(1_000),
                    Some(-1),
                    "completed",
                ),
            ],
        );
        let thread = &view.threads[0];
        assert_eq!(thread.quality, TimelineQuality::Partial);
        assert_eq!(thread.known_duration_ms, Some(1_900));
        assert_eq!(thread.turns[0].time_state, TurnTimeState::DurationOnly);
        assert_eq!(thread.turns[0].interval_start_unix_ms, None);
        assert_eq!(thread.turns[1].interval_start_unix_ms, Some(1_000));
        assert_eq!(thread.turns[1].duration_ms, None);
        assert_eq!(thread.turns[2].interval_start_unix_ms, Some(3_000));
        assert_eq!(thread.turns[2].interval_end_unix_ms, Some(3_000));
        assert_eq!(thread.turns[3].time_state, TurnTimeState::Running);
        assert_eq!(thread.turns[4].time_state, TurnTimeState::Incomplete);
        assert_eq!(thread.turns[5].time_state, TurnTimeState::Invalid);
        assert_eq!(thread.turns[5].interval_start_unix_ms, None);
        assert_eq!(thread.turns[6].duration_ms, None);
        assert_eq!(thread.turns[7].interval_start_unix_ms, Some(900));
        assert_eq!(thread.turns[7].duration_ms, None);
        assert_eq!(thread.last_activity_at_unix_ms, Some(5_000));
        assert_eq!(thread.last_activity_basis, LastActivityBasis::TurnStart);
    }

    #[test]
    fn no_verifiable_time_is_unknown_even_when_turn_list_is_complete() {
        let view = build_project_timeline(
            sessions(true),
            vec![turn("empty", 0, None, None, None, "completed")],
        );
        assert_eq!(view.threads[0].quality, TimelineQuality::Unknown);
        assert_eq!(view.threads[0].known_duration_ms, None);
        assert_eq!(view.threads[0].last_activity_at_unix_ms, Some(2_000));
        assert_eq!(
            view.threads[0].last_activity_basis,
            LastActivityBasis::MetadataUpdate
        );
    }
}
