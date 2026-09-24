//! Deterministic issue #30 fixture and measurements through public core queries.
//! Run with `cargo run --release -p codexflow-core --example performance -- generate DIR`
//! followed by `... -- measure DIR 20`. DIR must be disposable.
use codexflow_core::{ProjectThreadQuery, SourceService};
use codexflow_domain::{
    HistoryCoverage, HistoryFileChange, HistoryItem, HistoryReadPath, HistoryTurn,
    ThreadAttribution, ThreadMetadata,
};
use codexflow_store::SessionStore;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::{env, fs, path::Path, time::Instant};

const THREADS: usize = 500;
const TURNS_PER_THREAD: usize = 20;
const ITEMS_PER_TURN: usize = 10;
const DATASET_VERSION: &str = "issue-30-v1";

fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap()
}

fn generate(dir: &Path) {
    assert!(!dir.exists(), "目标目录已存在，避免覆盖用户数据");
    fs::create_dir_all(dir.join("workspace")).unwrap();
    let _store = SessionStore::new(dir.to_path_buf()).unwrap();
    let service = SourceService::new(dir.to_path_buf()).unwrap();
    let catalog = service
        .choose_project(dir.join("workspace").to_str().unwrap())
        .unwrap();
    let project_id = catalog.selected_project_id.unwrap();
    drop(service);
    let mut db = Connection::open(dir.join("sessions.sqlite3")).unwrap();
    let tx = db.transaction().unwrap();
    for n in 0..THREADS {
        let thread_id = format!("perf-thread-{n:04}");
        let updated_at = 1_700_000_000 + n as i64 * 1_000;
        let metadata = ThreadMetadata {
            id: thread_id.clone(),
            session_id: thread_id.clone(),
            title: Some(format!("性能验收会话 {n:04}")),
            preview: format!("固定输入项目工作记录 {n:04}，覆盖查询、过滤和关系图。"),
            cwd: dir.join("workspace").to_string_lossy().into_owned(),
            project_id: None,
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: Some("cli".into()),
            parent_thread_id: (n > 0).then(|| format!("perf-thread-{:04}", n - 1)),
            forked_from_id: None,
            git: None,
            created_at: updated_at - 100,
            updated_at,
            archived: n % 10 == 0,
            metadata_complete: true,
            turns_complete: true,
            items_complete: true,
            missing_from_source: false,
            content_complete: true,
            read_error: None,
            observed_at_unix_ms: updated_at * 1_000,
        };
        let attribution = ThreadAttribution {
            thread_id: thread_id.clone(),
            project_id: Some(project_id.clone()),
            workspace_root: Some(dir.join("workspace").to_string_lossy().into_owned()),
            basis: "selectedRoot".into(),
            detail: "固定合成工作区".into(),
            diagnostic: None,
            source_project_id: None,
        };
        tx.execute(
            "INSERT INTO threads(id,metadata_json,updated_at) VALUES (?1,?2,?3)",
            params![thread_id, json(&metadata), updated_at],
        )
        .unwrap();
        tx.execute("INSERT INTO thread_attributions(thread_id,project_id,attribution_json) VALUES (?1,?2,?3)",
            params![thread_id, project_id, json(&attribution)]).unwrap();
        let coverage = HistoryCoverage {
            thread_id: thread_id.clone(),
            source_updated_at: updated_at,
            attempted_at_unix_ms: updated_at * 1_000,
            path: HistoryReadPath::Paginated,
            turns_complete: true,
            items_complete: true,
            turn_pages: 1,
            item_pages: 10,
            loaded_turns: TURNS_PER_THREAD as u64,
            loaded_items: (TURNS_PER_THREAD * ITEMS_PER_TURN) as u64,
            incompatible: false,
            error: None,
        };
        tx.execute(
            "INSERT INTO history_coverage(thread_id,coverage_json) VALUES (?1,?2)",
            params![thread_id, json(&coverage)],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO history_revisions(thread_id,generation) VALUES (?1,1)",
            [&thread_id],
        )
        .unwrap();
        for turn_n in 0..TURNS_PER_THREAD {
            let turn_id = format!("turn-{turn_n:02}");
            let started = updated_at * 1_000 + turn_n as i64 * 60_000;
            let turn = HistoryTurn {
                thread_id: thread_id.clone(),
                id: turn_id.clone(),
                ordinal: turn_n as u64,
                status: "completed".into(),
                started_at_unix_ms: Some(started),
                completed_at_unix_ms: Some(started + 30_000),
                duration_ms: Some(30_000),
                time_error: None,
                source_updated_at: updated_at,
                content_version: format!("{DATASET_VERSION}-{n}-{turn_n}"),
            };
            tx.execute(
                "INSERT INTO history_turns(thread_id,id,ordinal,turn_json) VALUES (?1,?2,?3,?4)",
                params![thread_id, turn_id, turn_n as i64, json(&turn)],
            )
            .unwrap();
            for item_n in 0..ITEMS_PER_TURN {
                let item_id = format!("item-{item_n:02}");
                let item = HistoryItem {
                    thread_id: thread_id.clone(), turn_id: turn_id.clone(), id: item_id.clone(),
                    ordinal: item_n as u64, source_type: if turn_n == 0 && item_n == 0 {
                        "fileChange"
                    } else {
                        "agentMessage"
                    }.into(),
                    supported: true,
                    text: Some(format!("合成条目 {n:04}/{turn_n:02}/{item_n:02}，用于可重复的本地查询和证据首屏测量。")),
                    command: None, cwd: None, output: None, exit_code: None, status: None,
                    changes: if turn_n == 0 && item_n == 0 {
                        vec![HistoryFileChange { path: "src/shared.rs".into(), kind: "modified".into(),
                            diff: "+合成文件修改".into() }]
                    } else { Vec::new() },
                    source_updated_at: updated_at,
                    content_version: format!("{DATASET_VERSION}-{n}-{turn_n}-{item_n}"),
                };
                tx.execute("INSERT INTO history_items(thread_id,turn_id,id,ordinal,item_json) VALUES (?1,?2,?3,?4,?5)",
                    params![thread_id, turn_id, item_id, item_n as i64, json(&item)]).unwrap();
            }
        }
    }
    tx.commit().unwrap();
    println!(
        "dataset={DATASET_VERSION} project={project_id} threads={THREADS} turns={} items={}",
        THREADS * TURNS_PER_THREAD,
        THREADS * TURNS_PER_THREAD * ITEMS_PER_TURN
    );
}

fn timed<T>(name: &str, samples: &mut Vec<(String, f64)>, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    samples.push((name.into(), start.elapsed().as_secs_f64() * 1_000.0));
    result
}

fn measure(dir: &Path, runs: usize) {
    assert!(runs >= 20, "缓存场景至少测量 20 次");
    let db = Connection::open(dir.join("sessions.sqlite3")).unwrap();
    for (table, expected) in [
        ("threads", 500),
        ("history_turns", 10_000),
        ("history_items", 100_000),
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, expected, "数据集规模不符：{table}");
    }
    drop(db);
    let start = Instant::now();
    let service = SourceService::new(dir.to_path_buf()).unwrap();
    let startup_ms = start.elapsed().as_secs_f64() * 1_000.0;
    let project_id = service
        .project_catalog()
        .unwrap()
        .selected_project_id
        .unwrap();
    let thread_id = "perf-thread-0250";
    let mut samples = Vec::new();
    for run in 0..runs {
        let start_sample = samples.len();
        let catalog = timed("catalog", &mut samples, || {
            service.project_catalog().unwrap()
        });
        assert_eq!(
            catalog.selected_project_id.as_deref(),
            Some(project_id.as_str())
        );
        let sessions = timed("sessions", &mut samples, || {
            service.project_sessions(&project_id).unwrap()
        });
        assert_eq!(sessions.threads.len(), THREADS);
        let timeline = timed("timeline", &mut samples, || {
            service.project_timeline(&project_id).unwrap()
        });
        assert_eq!(timeline.threads.len(), THREADS);
        let graph = timed("graph", &mut samples, || {
            service.project_graph(&project_id).unwrap()
        });
        assert_eq!(graph.nodes.len(), THREADS);
        let visible_relations =
            graph.relations.len() + graph.derived_relations.len() + graph.inferred_relations.len();
        assert!((2_500..=3_000).contains(&visible_relations));
        let workstreams = timed("workstreams", &mut samples, || {
            service.project_workstreams(&project_id).unwrap()
        });
        assert_eq!(
            workstreams
                .workstreams
                .iter()
                .map(|stream| stream.members.len())
                .sum::<usize>()
                + workstreams.ungrouped_thread_ids.len(),
            THREADS
        );
        let preview = timed("candidate_preview", &mut samples, || {
            service.candidate_preview(&project_id).unwrap()
        });
        assert_eq!(preview.thread_count, THREADS as u64);
        let query = ProjectThreadQuery {
            text: "性能验收会话".into(),
            ..Default::default()
        };
        let result = timed("query", &mut samples, || {
            service.query_project_threads(&project_id, query).unwrap()
        });
        assert_eq!(result.matches.len(), THREADS);
        let filter = ProjectThreadQuery {
            archived: Some(true),
            ..Default::default()
        };
        let result = timed("filter", &mut samples, || {
            service.query_project_threads(&project_id, filter).unwrap()
        });
        assert_eq!(result.matches.len(), THREADS / 10);
        let turns = timed("evidence_turns_first_page", &mut samples, || {
            service.history_turns(thread_id, 0, 20).unwrap()
        });
        assert_eq!(turns.total, 20);
        let items = timed("evidence_items_first_page", &mut samples, || {
            service.history_items(thread_id, "turn-00", 0, 20).unwrap()
        });
        assert_eq!(items.total, 10);
        let facts = timed("facts_first_page", &mut samples, || {
            service.source_facts(thread_id, 0, 20).unwrap()
        });
        assert!(!facts.facts.is_empty());
        println!(
            "run={run} visible_relations={visible_relations} {}",
            samples[start_sample..]
                .iter()
                .map(|(name, ms)| format!("{name}={ms:.3}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    println!("startup_ms={startup_ms:.3} dataset={DATASET_VERSION} runs={runs}");
    for name in [
        "catalog",
        "sessions",
        "timeline",
        "graph",
        "workstreams",
        "candidate_preview",
        "query",
        "filter",
        "evidence_turns_first_page",
        "evidence_items_first_page",
        "facts_first_page",
    ] {
        let mut values: Vec<_> = samples
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| *value)
            .collect();
        values.sort_by(f64::total_cmp);
        println!(
            "{name}: min={:.3} median={:.3} p95={:.3} max={:.3}",
            values[0],
            values[values.len() / 2],
            values[(values.len() * 95).div_ceil(100) - 1],
            values[values.len() - 1]
        );
    }
}

fn main() {
    let args: Vec<_> = env::args().collect();
    assert_eq!(
        args.len(),
        if args.get(1).is_some_and(|s| s == "measure") {
            4
        } else {
            3
        },
        "usage: performance generate DIR | performance measure DIR RUNS | performance export-graph DIR"
    );
    let dir = Path::new(&args[2]);
    match args[1].as_str() {
        "generate" => generate(dir),
        "measure" => measure(dir, args[3].parse().unwrap()),
        "export-graph" => {
            let service = SourceService::new(dir.to_path_buf()).unwrap();
            let project_id = service
                .project_catalog()
                .unwrap()
                .selected_project_id
                .unwrap();
            serde_json::to_writer(
                std::io::stdout(),
                &service.project_graph(&project_id).unwrap(),
            )
            .unwrap();
        }
        _ => panic!("unknown action"),
    }
}
