import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ThreadSummaryView } from "./ThreadSummaryView";
import { formatAppError } from "./appError";

type Coverage = {
  threadId: string; sourceUpdatedAt: number; attemptedAtUnixMs: number;
  path: "none" | "paginated" | "fullRead"; turnsComplete: boolean; itemsComplete: boolean;
  turnPages: number; itemPages: number; loadedTurns: number; loadedItems: number;
  incompatible: boolean; error: string | null;
};
type Turn = {
  id: string; ordinal: number; status: string; startedAtUnixMs: number | null;
  completedAtUnixMs: number | null; durationMs: number | null;
  sourceUpdatedAt: number; contentVersion: string;
};
type Item = {
  id: string; turnId: string; ordinal: number; sourceType: string; supported: boolean;
  text: string | null; command: string | null; cwd: string | null; output: string | null;
  exitCode: number | null; status: string | null;
  changes: { path: string; kind: string; diff: string }[];
  sourceUpdatedAt: number; contentVersion: string;
};
type TurnPage = { coverage: Coverage | null; turns: Turn[]; total: number; offset: number; limit: number };
type ItemPage = { coverage: Coverage | null; items: Item[]; total: number; offset: number; limit: number };
type Location = { turnId: string; turnOffset: number; offset: number };
type Fact = { id: string; turnId: string; itemId: string; kind: "file" | "command" | "branch" | "artifact";
  subject: string; operation: string; outcome: "succeeded" | "failed" | "unknown";
  evidenceId: string; contentVersion: string; ruleVersion: string };
type FactPage = { facts: Fact[]; total: number; offset: number; limit: number; coverage: Coverage | null };
type Evidence = { id: string; excerpt: string; contentVersion: string; field: string };
type EvidencePage = { evidence: Evidence[]; total: number; offset: number; limit: number };
type EvidenceCheck = { state: "valid" | "missingThread" | "missingTurn" | "missingItem" | "missingFact" | "wrongHierarchy" | "excerptMissing" | "staleVersion";
  message: string; location: Location | null };
const TURN_LIMIT = 20;
const ITEM_LIMIT = 20;
const FACT_LIMIT = 20;

function errorText(error: unknown): string {
  return formatAppError(error, "读取历史失败。");
}

function time(value: number | null): string {
  return value === null ? "时间未知" : new Date(value).toLocaleString("zh-CN");
}

function status(coverage: Coverage | null, currentVersion: number): string {
  if (!coverage) return "尚未读取历史";
  if (coverage.sourceUpdatedAt !== currentVersion) return "旧缓存，来源已有更新";
  if (coverage.turnsComplete && coverage.itemsComplete) return coverage.error ? "完整缓存，最近重读失败" : "回合与条目完整";
  if (coverage.incompatible) return "来源接口不兼容";
  return coverage.loadedTurns || coverage.loadedItems ? "部分完整" : "内容不可用";
}

function SourceFactsView({ threadId, updatedAt, coverage, revision, onLocate }: {
  threadId: string; updatedAt: number; coverage: Coverage | null; revision: number;
  onLocate: (location: Location, itemId: string) => Promise<void>;
}) {
  const [offset, setOffset] = useState(0);
  const [page, setPage] = useState<FactPage | null>(null);
  const [evidence, setEvidence] = useState<EvidencePage | null>(null);
  const [checks, setChecks] = useState<Record<string, EvidenceCheck>>({});
  const [message, setMessage] = useState("");
  useEffect(() => { setOffset(0); setPage(null); setEvidence(null); setChecks({}); setMessage(""); }, [threadId]);
  useEffect(() => {
    let active = true;
    invoke<FactPage>("get_source_facts", { threadId, offset, limit: FACT_LIMIT }).then(async (facts) => {
      if (!active) return;
      const sources = await invoke<EvidencePage>("get_source_evidence", { threadId, offset, limit: FACT_LIMIT });
      if (!active) return;
      setPage(facts); setEvidence(sources);
      const results = await Promise.all(facts.facts.map(async (fact) => {
        try { return [fact.evidenceId, await invoke<EvidenceCheck>("validate_source_evidence", { evidenceId: fact.evidenceId })] as const; }
        catch (caught) { return [fact.evidenceId, { state: "missingItem", message: errorText(caught), location: null }] as const; }
      }));
      if (active) setChecks(Object.fromEntries(results));
    }).catch((caught) => { if (active) setMessage(errorText(caught)); });
    return () => { active = false; };
  }, [threadId, offset, revision, updatedAt]);

  async function inspect(fact: Fact) {
    setMessage("");
    try {
      const check = await invoke<EvidenceCheck>("validate_source_evidence", { evidenceId: fact.evidenceId });
      setChecks((previous) => ({ ...previous, [fact.evidenceId]: check }));
      if (check.state !== "valid" || !check.location) { setMessage(check.message); return; }
      await onLocate(check.location, fact.itemId);
      setMessage(`已定位到回合 ${fact.turnId} 的来源条目。`);
    } catch (caught) { setMessage(errorText(caught)); }
  }
  const labels = { file: "文件", command: "命令", branch: "分支", artifact: "产物" };
  const outcomes = { succeeded: "成功", failed: "失败", unknown: "结果未知" };
  const operations: Record<string, string> = { executed: "已执行", created: "已创建", createdAndSwitched: "已创建并切换",
    add: "新增", added: "新增", create: "创建", modify: "修改", modified: "修改", delete: "删除", deleted: "删除" };
  const sources = new Map(evidence?.evidence.map((item) => [item.id, item]) ?? []);
  return <div className="source-facts"><h3>结构化事实 <small>{page?.total ?? 0}</small></h3>
    <p className="source-facts-note">仅提取结构化来源条目；分支与产物只在明确成功的操作中显示。</p>
    {coverage && (!coverage.turnsComplete || !coverage.itemsComplete) && <p className="thread-warning">来源仅部分完整，事实可能缺失。</p>}
    {!page?.total && <p className="empty-list">暂无可提取的文件或命令事实。</p>}
    {page?.facts.map((fact) => {
      const check = checks[fact.evidenceId];
      const source = sources.get(fact.evidenceId);
      return <article className="source-fact" key={fact.id}>
        <div><strong>{labels[fact.kind]}</strong><span>{operations[fact.operation] ?? fact.operation}</span><span className={fact.outcome === "unknown" ? "thread-warning" : ""}>{outcomes[fact.outcome]}</span>
          {check && check.state !== "valid" && <span className="thread-warning">{check.state === "staleVersion" ? "证据过期" : "来源缺失或无效"}</span>}</div>
        <code>{fact.subject}</code><small>回合 {fact.turnId} · 条目 {fact.itemId}</small>
        {source && <blockquote>{source.excerpt}</blockquote>}
        {check && check.state !== "valid" && <small>{check.message}</small>}
        <button className="browse-button" onClick={() => void inspect(fact)}>检查证据并定位条目</button>
      </article>;
    })}
    {message && <p className="history-lookup-message" role="status">{message}</p>}
    <div className="history-pager"><button disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - FACT_LIMIT))}>上一页</button>
      <span>{page?.total ? `${offset + 1}–${Math.min(offset + FACT_LIMIT, page.total)} / ${page.total}` : "0 / 0"}</span>
      <button disabled={!page || offset + FACT_LIMIT >= page.total} onClick={() => setOffset(offset + FACT_LIMIT)}>下一页</button></div>
  </div>;
}

export function ThreadHistoryView({ threadId, updatedAt, connected, onHistoryLoaded, locationRequest }: {
  threadId: string; updatedAt: number; connected: boolean; onHistoryLoaded?: () => void;
  locationRequest?: { threadId: string; turnId: string; itemId: string; nonce: number } | null;
}) {
  const [turns, setTurns] = useState<TurnPage | null>(null);
  const [items, setItems] = useState<ItemPage | null>(null);
  const [selectedTurnId, setSelectedTurnId] = useState<string | null>(null);
  const [turnOffset, setTurnOffset] = useState(0);
  const [itemOffset, setItemOffset] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [lookupTurn, setLookupTurn] = useState("");
  const [lookup, setLookup] = useState("");
  const [lookupMessage, setLookupMessage] = useState("");
  const [highlight, setHighlight] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const locatedNonce = useRef<number | null>(null);
  const loadedThreadId = useRef(threadId);

  useEffect(() => {
    let active = true;
    if (loadedThreadId.current !== threadId) {
      loadedThreadId.current = threadId;
      setTurns(null);
      setItems(null);
      setSelectedTurnId(null);
      setTurnOffset(0);
      setItemOffset(0);
      setLookupTurn("");
      setLookup("");
      setLookupMessage("");
      setHighlight(null);
    }
    setError("");
    async function openHistory() {
      try {
        let page = await invoke<TurnPage>("get_history_turns", { threadId, offset: 0, limit: TURN_LIMIT });
        if (!active) return;
        setTurns(page);
        setSelectedTurnId(page.turns[0]?.id ?? null);
        if ((!page.coverage || page.coverage.sourceUpdatedAt !== updatedAt) && connected) {
          setLoading(true);
          await invoke<Coverage>("load_thread_history", { threadId });
          page = await invoke<TurnPage>("get_history_turns", { threadId, offset: 0, limit: TURN_LIMIT });
          if (active) {
            setTurns(page);
            setSelectedTurnId(page.turns[0]?.id ?? null);
            setRevision((value) => value + 1);
            onHistoryLoaded?.();
          }
        }
      } catch (caught) { if (active) setError(errorText(caught)); }
      finally { if (active) setLoading(false); }
    }
    void openHistory();
    return () => { active = false; };
  }, [threadId, updatedAt, connected]);

  useEffect(() => {
    let active = true;
    if (!selectedTurnId) { setItems(null); return; }
    invoke<ItemPage>("get_history_items", { threadId, turnId: selectedTurnId, offset: itemOffset, limit: ITEM_LIMIT })
      .then((page) => { if (active) setItems(page); })
      .catch((caught) => { if (active) setError(errorText(caught)); });
    return () => { active = false; };
  }, [threadId, selectedTurnId, itemOffset, revision]);

  async function showTurnPage(offset: number) {
    try {
      const page = await invoke<TurnPage>("get_history_turns", { threadId, offset, limit: TURN_LIMIT });
      setTurns(page);
      setTurnOffset(offset);
      setSelectedTurnId(page.turns[0]?.id ?? null);
      setItemOffset(0);
      setHighlight(null);
    } catch (caught) { setError(errorText(caught)); }
  }

  async function reload() {
    setLoading(true);
    setError("");
    try {
      await invoke<Coverage>("load_thread_history", { threadId });
      const page = await invoke<TurnPage>("get_history_turns", { threadId, offset: turnOffset, limit: TURN_LIMIT });
      setTurns(page);
      setSelectedTurnId((previous) => page.turns.some((turn) => turn.id === previous) ? previous : page.turns[0]?.id ?? null);
      setRevision((value) => value + 1);
      onHistoryLoaded?.();
    } catch (caught) { setError(errorText(caught)); }
    finally { setLoading(false); }
  }

  async function locate() {
    setLookupMessage("");
    const turnId = lookupTurn.trim();
    const id = lookup.trim();
    if (!turnId || !id) {
      setLookupMessage("请输入回合标识和条目标识。");
      return;
    }
    try {
      const location = await invoke<Location | null>("locate_history_item", { threadId, turnId, itemId: id });
      if (!location) {
        setLookupMessage(turns?.coverage?.itemsComplete ? "本次完整历史中没有这个条目标识。" : "缓存中没有这个条目；当前历史可能不完整。");
        return;
      }
      await showLocation(location, id);
      setLookupMessage(`已定位到回合 ${location.turnId}。`);
    } catch (caught) { setLookupMessage(errorText(caught)); }
  }

  async function showLocation(location: Location, itemId: string) {
    const nextTurnOffset = Math.floor(location.turnOffset / TURN_LIMIT) * TURN_LIMIT;
    const page = await invoke<TurnPage>("get_history_turns", { threadId, offset: nextTurnOffset, limit: TURN_LIMIT });
    setTurns(page);
    setTurnOffset(nextTurnOffset);
    setSelectedTurnId(location.turnId);
    setItemOffset(Math.floor(location.offset / ITEM_LIMIT) * ITEM_LIMIT);
    setHighlight(itemId);
    document.getElementById("thread-history-items")?.scrollIntoView?.({ behavior: "smooth", block: "start" });
  }

  useEffect(() => {
    if (!locationRequest || locationRequest.threadId !== threadId || !turns || loading || locatedNonce.current === locationRequest.nonce) return;
    locatedNonce.current = locationRequest.nonce;
    invoke<Location | null>("locate_history_item", { threadId, turnId: locationRequest.turnId, itemId: locationRequest.itemId })
      .then(async (location) => {
        if (!location) { setLookupMessage("来源条目已失效或当前历史不完整。"); return; }
        await showLocation(location, locationRequest.itemId);
        setLookupMessage(`已定位到回合 ${location.turnId}。`);
      }).catch((caught) => setLookupMessage(errorText(caught)));
  }, [locationRequest?.nonce, threadId, turns?.total, loading]);

  const coverage = turns?.coverage ?? null;
  return <section id="thread-history" className="panel history-panel" aria-label="会话历史">
    <div className="history-heading"><div><div className="panel-kicker">04 / 会话详情</div><h2>回合与条目</h2><small className="history-thread-id">{threadId}</small></div>
      <button className="browse-button" disabled={!connected || loading} onClick={() => void reload()}>{loading ? "正在读取…" : coverage ? "重新读取历史" : "读取历史"}</button></div>
    <div className="history-status" role="status"><strong>{status(coverage, updatedAt)}</strong>
      <span>{coverage ? `读取方式：${coverage.path === "paginated" ? "分页" : coverage.path === "fullRead" ? "完整读取兼容路径" : "未取得内容"}；已取得 ${coverage.loadedTurns} 回合、${coverage.loadedItems} 条目` : "打开详情后按需读取，按页浏览本地缓存。"}</span>
      {coverage && <small>来源版本：{coverage.sourceUpdatedAt}；采集时间：{time(coverage.attemptedAtUnixMs)}</small>}
      {coverage?.error && <em>{coverage.error}</em>}
      {!connected && <em>来源当前不可用；已保存的历史仍可浏览。</em>}
    </div>
    {error && <div className="page-error" role="alert">{error}</div>}
    <ThreadSummaryView threadId={threadId} connected={connected} revision={revision} onLocate={showLocation} />
    <SourceFactsView threadId={threadId} updatedAt={updatedAt} coverage={coverage} revision={revision} onLocate={showLocation} />
    <div className="history-locator"><label>回合标识<input value={lookupTurn} onChange={(event) => setLookupTurn(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") void locate(); }} placeholder="输入完整 Turn ID" /></label><label>条目标识<input value={lookup} onChange={(event) => setLookup(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") void locate(); }} placeholder="输入完整 Item ID" /></label><button className="browse-button" onClick={() => void locate()}>定位</button></div>
    {lookupMessage && <p className="history-lookup-message" role="status">{lookupMessage}</p>}
    <div className="history-columns">
      <div className="history-turns"><h3>回合 <small>{turns?.total ?? 0}</small></h3>
        {!turns?.total && <p className="empty-list">{coverage?.itemsComplete ? "此会话没有可读取的回合。" : "尚无可显示回合。"}</p>}
        {turns?.turns.map((turn) => <button key={turn.id} className={`history-turn ${selectedTurnId === turn.id ? "selected" : ""}`} onClick={() => { setSelectedTurnId(turn.id); setItemOffset(0); setHighlight(null); }}><strong>{turn.ordinal + 1}. {turn.status}</strong><small>{time(turn.startedAtUnixMs)}</small><code>{turn.id}</code>{turn.sourceUpdatedAt !== updatedAt && <em>旧缓存</em>}</button>)}
        <div className="history-pager"><button disabled={turnOffset === 0} onClick={() => void showTurnPage(Math.max(0, turnOffset - TURN_LIMIT))}>上一页</button><span>{turns?.total ? `${turnOffset + 1}–${Math.min(turnOffset + TURN_LIMIT, turns.total)} / ${turns.total}` : "0 / 0"}</span><button disabled={!turns || turnOffset + TURN_LIMIT >= turns.total} onClick={() => void showTurnPage(turnOffset + TURN_LIMIT)}>下一页</button></div>
      </div>
      <div id="thread-history-items" className="history-items"><h3>条目 <small>{items?.total ?? 0}</small></h3>
        {!selectedTurnId && <p className="empty-list">选择回合后查看条目。</p>}
        {selectedTurnId && !items?.total && <p className="empty-list">{coverage?.itemsComplete ? "此回合没有条目。" : "此回合的条目尚未完整取得。"}</p>}
        {items?.items.map((item) => <article key={item.id} className={`history-item ${highlight === item.id ? "located" : ""}`}><div><strong>{item.sourceType}</strong>{!item.supported && <span className="thread-warning">内容类型暂不支持</span>}{item.sourceUpdatedAt !== updatedAt && <span className="thread-warning">旧缓存</span>}</div><code>{item.id}</code><small>内容版本 {item.contentVersion.slice(0, 12)}</small>
          {item.text && <pre>{item.text}</pre>}{item.command && <><code className="history-command">{item.command}</code><small>{item.cwd ?? ""} · 状态 {item.status ?? "未知"} · 退出码 {item.exitCode ?? "未知"}</small></>}
          {item.output && <details><summary>命令输出</summary><pre>{item.output}</pre></details>}
          {item.changes.map((change, index) => <details key={`${change.path}-${index}`}><summary>{change.kind} · {change.path}</summary><pre>{change.diff}</pre></details>)}
        </article>)}
        <div className="history-pager"><button disabled={itemOffset === 0} onClick={() => setItemOffset(Math.max(0, itemOffset - ITEM_LIMIT))}>上一页</button><span>{items?.total ? `${itemOffset + 1}–${Math.min(itemOffset + ITEM_LIMIT, items.total)} / ${items.total}` : "0 / 0"}</span><button disabled={!items || itemOffset + ITEM_LIMIT >= items.total} onClick={() => setItemOffset(itemOffset + ITEM_LIMIT)}>下一页</button></div>
      </div>
    </div>
  </section>;
}
