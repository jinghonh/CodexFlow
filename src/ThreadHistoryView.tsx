import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

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
const TURN_LIMIT = 20;
const ITEM_LIMIT = 20;

function errorText(error: unknown): string {
  return typeof error === "object" && error !== null && "message" in error && typeof error.message === "string"
    ? error.message : "读取历史失败。";
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

export function ThreadHistoryView({ threadId, updatedAt, connected, onHistoryLoaded }: {
  threadId: string; updatedAt: number; connected: boolean; onHistoryLoaded?: () => void;
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

  useEffect(() => {
    let active = true;
    setTurns(null);
    setItems(null);
    setSelectedTurnId(null);
    setTurnOffset(0);
    setItemOffset(0);
    setError("");
    setLookupTurn("");
    setLookup("");
    setLookupMessage("");
    setHighlight(null);
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
      const nextTurnOffset = Math.floor(location.turnOffset / TURN_LIMIT) * TURN_LIMIT;
      const page = await invoke<TurnPage>("get_history_turns", { threadId, offset: nextTurnOffset, limit: TURN_LIMIT });
      setTurns(page);
      setTurnOffset(nextTurnOffset);
      setSelectedTurnId(location.turnId);
      setItemOffset(Math.floor(location.offset / ITEM_LIMIT) * ITEM_LIMIT);
      setHighlight(id);
      setLookupMessage(`已定位到回合 ${location.turnId}。`);
    } catch (caught) { setLookupMessage(errorText(caught)); }
  }

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
    <div className="history-locator"><label>回合标识<input value={lookupTurn} onChange={(event) => setLookupTurn(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") void locate(); }} placeholder="输入完整 Turn ID" /></label><label>条目标识<input value={lookup} onChange={(event) => setLookup(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") void locate(); }} placeholder="输入完整 Item ID" /></label><button className="browse-button" onClick={() => void locate()}>定位</button></div>
    {lookupMessage && <p className="history-lookup-message" role="status">{lookupMessage}</p>}
    <div className="history-columns">
      <div className="history-turns"><h3>回合 <small>{turns?.total ?? 0}</small></h3>
        {!turns?.total && <p className="empty-list">{coverage?.itemsComplete ? "此会话没有可读取的回合。" : "尚无可显示回合。"}</p>}
        {turns?.turns.map((turn) => <button key={turn.id} className={`history-turn ${selectedTurnId === turn.id ? "selected" : ""}`} onClick={() => { setSelectedTurnId(turn.id); setItemOffset(0); setHighlight(null); }}><strong>{turn.ordinal + 1}. {turn.status}</strong><small>{time(turn.startedAtUnixMs)}</small><code>{turn.id}</code>{turn.sourceUpdatedAt !== updatedAt && <em>旧缓存</em>}</button>)}
        <div className="history-pager"><button disabled={turnOffset === 0} onClick={() => void showTurnPage(Math.max(0, turnOffset - TURN_LIMIT))}>上一页</button><span>{turns?.total ? `${turnOffset + 1}–${Math.min(turnOffset + TURN_LIMIT, turns.total)} / ${turns.total}` : "0 / 0"}</span><button disabled={!turns || turnOffset + TURN_LIMIT >= turns.total} onClick={() => void showTurnPage(turnOffset + TURN_LIMIT)}>下一页</button></div>
      </div>
      <div className="history-items"><h3>条目 <small>{items?.total ?? 0}</small></h3>
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
