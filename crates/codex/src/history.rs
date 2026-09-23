use super::{ProbeError, Session};
use codexflow_domain::{
    HistoryCoverage, HistoryFileChange, HistoryItem, HistoryReadPath, HistorySnapshot, HistoryTurn,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

const PAGE_SIZE: u32 = 100;
const MAX_PAGES: u32 = 10_000;

struct ReadFailure {
    detail: String,
    incompatible: bool,
}

impl From<&str> for ReadFailure {
    fn from(detail: &str) -> Self {
        Self {
            detail: detail.into(),
            incompatible: false,
        }
    }
}

impl From<String> for ReadFailure {
    fn from(detail: String) -> Self {
        Self {
            detail,
            incompatible: false,
        }
    }
}

fn version(value: &Value) -> String {
    let digest = Sha256::digest(serde_json::to_vec(value).expect("JSON value serializes"));
    format!("{digest:x}")
}

fn status(value: &Value) -> String {
    value
        .as_str()
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("unknown")
        .to_owned()
}

fn seconds_to_ms(value: Option<i64>) -> Option<i64> {
    value.and_then(|seconds| seconds.checked_mul(1000))
}

fn parse_turn(
    value: &Value,
    thread_id: &str,
    ordinal: u64,
    source_updated_at: i64,
) -> Result<HistoryTurn, String> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("回合缺少稳定标识。")?;
    let state = status(value.get("status").unwrap_or(&Value::Null));
    let started_at_unix_ms = seconds_to_ms(value.get("startedAt").and_then(Value::as_i64));
    let completed_at_unix_ms = seconds_to_ms(value.get("completedAt").and_then(Value::as_i64));
    let duration_ms = value.get("durationMs").and_then(Value::as_i64);
    let content_version = version(
        &json!({"id":id,"status":state,"startedAt":started_at_unix_ms,
        "completedAt":completed_at_unix_ms,"durationMs":duration_ms}),
    );
    Ok(HistoryTurn {
        thread_id: thread_id.into(),
        id: id.into(),
        ordinal,
        status: state,
        started_at_unix_ms,
        completed_at_unix_ms,
        duration_ms,
        source_updated_at,
        content_version,
    })
}

fn parse_item(
    value: &Value,
    thread_id: &str,
    turn_id: &str,
    ordinal: u64,
    source_updated_at: i64,
) -> Result<HistoryItem, String> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("条目缺少稳定标识。")?;
    let source_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let mut item = HistoryItem {
        thread_id: thread_id.into(),
        turn_id: turn_id.into(),
        id: id.into(),
        ordinal,
        source_type: source_type.clone(),
        supported: false,
        text: None,
        command: None,
        cwd: None,
        output: None,
        exit_code: None,
        status: None,
        changes: Vec::new(),
        source_updated_at,
        content_version: version(value),
    };
    match source_type.as_str() {
        "userMessage" => {
            if let Some(parts) = value.get("content").and_then(Value::as_array) {
                let text: Vec<&str> = parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect();
                item.text = (!text.is_empty()).then(|| text.join("\n"));
                item.supported = parts
                    .iter()
                    .all(|part| part.get("type").and_then(Value::as_str) == Some("text"));
            }
        }
        "agentMessage" | "plan" => {
            item.text = value.get("text").and_then(Value::as_str).map(str::to_owned);
            item.supported = item.text.is_some();
        }
        "reasoning" => {
            let summary = value.get("summary").and_then(Value::as_array).map(|parts| {
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            item.text = summary.filter(|text| !text.is_empty());
            item.supported = item.text.is_some();
        }
        "commandExecution" => {
            item.command = value
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_owned);
            item.cwd = value.get("cwd").and_then(Value::as_str).map(str::to_owned);
            item.output = value
                .get("aggregatedOutput")
                .and_then(Value::as_str)
                .map(str::to_owned);
            item.exit_code = value.get("exitCode").and_then(Value::as_i64);
            item.status = value.get("status").map(status);
            item.supported = item.command.is_some();
        }
        "fileChange" => {
            item.status = value.get("status").map(status);
            if let Some(changes) = value.get("changes").and_then(Value::as_array) {
                item.changes = changes
                    .iter()
                    .filter_map(|change| {
                        Some(HistoryFileChange {
                            path: change.get("path")?.as_str()?.to_owned(),
                            kind: change.get("kind")?.as_str()?.to_owned(),
                            diff: change.get("diff")?.as_str()?.to_owned(),
                        })
                    })
                    .collect();
                item.supported = item.changes.len() == changes.len();
            }
        }
        _ => {} // The identity and source type remain available for future adapters.
    }
    Ok(item)
}

fn page<'a>(response: &'a Value) -> Result<(&'a [Value], Option<String>), String> {
    let data = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or("分页响应缺少 data 数组。")?;
    let next = match response.get("nextCursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(cursor)) if !cursor.is_empty() => Some(cursor.clone()),
        _ => return Err("分页响应游标无效。".into()),
    };
    Ok((data, next))
}

impl Session {
    /// Reads one source thread without resuming it. A failure returns known pages and coverage.
    pub async fn collect_history(
        &mut self,
        thread_id: &str,
        source_updated_at: i64,
        attempted_at_unix_ms: i64,
    ) -> HistorySnapshot {
        let mut snapshot = HistorySnapshot {
            coverage: HistoryCoverage {
                thread_id: thread_id.into(),
                source_updated_at,
                attempted_at_unix_ms,
                path: HistoryReadPath::None,
                turns_complete: false,
                items_complete: false,
                turn_pages: 0,
                item_pages: 0,
                loaded_turns: 0,
                loaded_items: 0,
                incompatible: false,
                error: None,
            },
            turns: Vec::new(),
            items: Vec::new(),
        };
        let paginated = self
            .collect_paginated(thread_id, source_updated_at, &mut snapshot)
            .await;
        if paginated.is_ok() {
            snapshot.coverage.path = HistoryReadPath::Paginated;
            snapshot.coverage.turns_complete = true;
            snapshot.coverage.items_complete = true;
        } else {
            let page_error = paginated.unwrap_err();
            if let Err(read_error) = self
                .collect_full_read(thread_id, source_updated_at, &mut snapshot)
                .await
            {
                snapshot.coverage.incompatible = page_error.incompatible && read_error.incompatible;
                snapshot.coverage.error = Some(format!(
                    "分页读取：{} 完整读取：{}",
                    page_error.detail, read_error.detail
                ));
                if !snapshot.turns.is_empty() || !snapshot.items.is_empty() {
                    snapshot.coverage.path = HistoryReadPath::Paginated;
                }
            }
        }
        snapshot.coverage.loaded_turns = snapshot.turns.len() as u64;
        snapshot.coverage.loaded_items = snapshot.items.len() as u64;
        snapshot
    }

    async fn collect_paginated(
        &mut self,
        thread_id: &str,
        source_updated_at: i64,
        snapshot: &mut HistorySnapshot,
    ) -> Result<(), ReadFailure> {
        let mut cursor = None;
        let mut seen = HashSet::new();
        let mut turn_ids = HashSet::new();
        for _ in 0..MAX_PAGES {
            let mut params = json!({"threadId":thread_id,"limit":PAGE_SIZE,"sortDirection":"asc","itemsView":"notLoaded"});
            if let Some(ref cursor) = cursor {
                params["cursor"] = json!(cursor);
            }
            let response = self
                .request("thread/turns/list", params)
                .await
                .map_err(rpc_error)?;
            let (data, next) = page(&response)?;
            for value in data {
                let turn = parse_turn(
                    value,
                    thread_id,
                    snapshot.turns.len() as u64,
                    source_updated_at,
                )?;
                if !turn_ids.insert(turn.id.clone()) {
                    return Err("回合分页出现重复标识。".into());
                }
                snapshot.turns.push(turn);
            }
            snapshot.coverage.turn_pages += 1;
            match next {
                None => {
                    snapshot.coverage.turns_complete = true;
                    break;
                }
                Some(next) if seen.insert(next.clone()) => cursor = Some(next),
                Some(_) => return Err("回合分页游标重复。".into()),
            }
        }
        if !snapshot.coverage.turns_complete {
            return Err("回合分页超过安全页数。".into());
        }

        cursor = None;
        seen.clear();
        let mut item_ids = HashSet::new();
        for _ in 0..MAX_PAGES {
            let mut params = json!({"threadId":thread_id,"limit":PAGE_SIZE,"sortDirection":"asc"});
            if let Some(ref cursor) = cursor {
                params["cursor"] = json!(cursor);
            }
            let response = self
                .request("thread/items/list", params)
                .await
                .map_err(rpc_error)?;
            let (data, next) = page(&response)?;
            for entry in data {
                let turn_id = entry
                    .get("turnId")
                    .and_then(Value::as_str)
                    .ok_or("条目分页缺少 turnId。")?;
                let value = entry.get("item").ok_or("条目分页缺少 item。")?;
                if !turn_ids.contains(turn_id) {
                    return Err("条目对应的回合不在完整回合列表中。".into());
                }
                let item = parse_item(
                    value,
                    thread_id,
                    turn_id,
                    snapshot.items.len() as u64,
                    source_updated_at,
                )?;
                if !item_ids.insert(item.id.clone()) {
                    return Err("条目分页出现重复标识。".into());
                }
                snapshot.items.push(item);
            }
            snapshot.coverage.item_pages += 1;
            match next {
                None => return Ok(()),
                Some(next) if seen.insert(next.clone()) => cursor = Some(next),
                Some(_) => return Err("条目分页游标重复。".into()),
            }
        }
        Err("条目分页超过安全页数。".into())
    }

    async fn collect_full_read(
        &mut self,
        thread_id: &str,
        source_updated_at: i64,
        snapshot: &mut HistorySnapshot,
    ) -> Result<(), ReadFailure> {
        let response = self
            .request(
                "thread/read",
                json!({"threadId":thread_id,"includeTurns":true}),
            )
            .await
            .map_err(rpc_error)?;
        let thread = response.get("thread").ok_or("完整读取缺少 thread。")?;
        if thread.get("id").and_then(Value::as_str) != Some(thread_id) {
            return Err("完整读取的会话身份不匹配。".into());
        }
        let values = thread
            .get("turns")
            .and_then(Value::as_array)
            .ok_or("完整读取缺少 turns 数组。")?;
        let mut turns = Vec::with_capacity(values.len());
        let mut items = Vec::new();
        let mut ids = HashSet::new();
        for value in values {
            let turn = parse_turn(value, thread_id, turns.len() as u64, source_updated_at)?;
            if !ids.insert(turn.id.clone()) {
                return Err("完整读取出现重复回合。".into());
            }
            let view = value
                .get("itemsView")
                .and_then(Value::as_str)
                .unwrap_or("full");
            if view != "full" {
                return Err("完整读取只返回条目摘要，不能作为完整内容。".into());
            }
            let entries = value
                .get("items")
                .and_then(Value::as_array)
                .ok_or("完整读取缺少条目数组。")?;
            for entry in entries {
                items.push(parse_item(
                    entry,
                    thread_id,
                    &turn.id,
                    items.len() as u64,
                    source_updated_at,
                )?);
            }
            turns.push(turn);
        }
        let unique_items: HashSet<&str> = items.iter().map(|item| item.id.as_str()).collect();
        if unique_items.len() != items.len() {
            return Err("完整读取出现重复条目。".into());
        }
        snapshot.turns = turns;
        snapshot.items = items;
        snapshot.coverage.path = HistoryReadPath::FullRead;
        snapshot.coverage.turns_complete = true;
        snapshot.coverage.items_complete = true;
        snapshot.coverage.error = None;
        Ok(())
    }
}

fn rpc_error(error: ProbeError) -> ReadFailure {
    ReadFailure {
        detail: error.description().to_owned(),
        incompatible: error.unsupported(),
    }
}
