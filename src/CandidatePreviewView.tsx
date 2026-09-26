import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatAppError } from "./appError";

type Evidence = { id: string; threadId: string; turnId: string; itemId: string; excerpt: string; contentVersion: string };
type Pair = { id: string; left: Evidence; right: Evidence };
type Candidate = { id: string; leftThreadId: string; rightThreadId: string; score: number;
  leftSummary?: string | null; rightSummary?: string | null;
  reasons: { signal: string; detail: string }[];
  evidence: { leftAvailable: number; rightAvailable: number; combinationsAvailable: number;
    combinationsShown: number; leftSampled: number; rightSampled: number; samplingRule: string; pairs: Pair[] } };
type Preview = { projectId: string; threadCount: number; unavailableThreads?: number; neighborLimit: number; candidateCount: number;
  candidates: Candidate[]; staleCandidates?: { candidate: Candidate; inputVersion: string; reason: string }[] };
const PAGE_SIZE = 25;

export function CandidatePreviewView({ projectId, refreshVersion, onSelectEvidence }: {
  projectId: string; refreshVersion: string; onSelectEvidence: (evidence: Evidence) => void;
}) {
  const loadedProjectId = useRef(projectId);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [error, setError] = useState("");
  const [page, setPage] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    if (loadedProjectId.current !== projectId) {
      loadedProjectId.current = projectId;
      setPreview(null); setPage(0); setSelectedId(null);
    }
    setError("");
    invoke<Preview>("get_candidate_preview", { projectId })
      .then((value) => { if (active) setPreview(value); })
      .catch((caught) => { if (active) setError(formatAppError(caught, "候选清单读取失败。")); });
    return () => { active = false; };
  }, [projectId, refreshVersion]);
  const selectedStale = preview?.staleCandidates?.find((entry) => selectedId === `stale:${entry.candidate.id}`);
  const selected = selectedStale?.candidate ?? preview?.candidates.find((candidate) => candidate.id === selectedId);
  return <section className="panel candidate-panel" aria-label="关系候选预览">
    <div className="panel-kicker">05 / 关系候选预览</div>
    <h2>待分析候选</h2>
    <p className="panel-intro">候选仅用于后续关系判断，排序分数是本地筛选分数，不是关系概率或已确认的语义关系。</p>
    {error && <p className="page-error" role="alert">{error}</p>}
    {!preview && !error && <p>正在计算候选…</p>}
    {preview && <>
      <div className="candidate-summary"><strong>{preview.candidateCount} 对候选</strong><span>{preview.threadCount} 条会话</span><span>明确引用和共享产物候选不受配额限制；其他线索每条会话最多 10 对</span></div>
      {!!preview.unavailableThreads && <p className="thread-warning">{preview.unavailableThreads} 条会话历史不完整或来源版本已变化，仍参与标题、预览、分支和时间线索召回；未读取的来源事实不作为信号。</p>}
      {preview.candidateCount === 0 && <p className="empty-list">当前没有具备筛选信号的候选。</p>}
      <div className="candidate-list">
        {preview.candidates.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE).map((candidate) => <button key={candidate.id}
          className={`candidate-row ${selectedId === candidate.id ? "selected" : ""}`} onClick={() => setSelectedId(candidate.id)}>
          <strong>{candidate.leftThreadId} ↔ {candidate.rightThreadId}</strong><span>筛选分数 {candidate.score}</span>
          <small>{candidate.reasons.map((reason) => reason.detail).join("；")}</small>
        </button>)}
      </div>
      {!!preview.staleCandidates?.length && <div className="candidate-list"><strong>过期候选 · {preview.staleCandidates.length} 对</strong>
        {preview.staleCandidates.map((entry) => <button key={`stale:${entry.candidate.id}`}
          className={`candidate-row ${selectedId === `stale:${entry.candidate.id}` ? "selected" : ""}`}
          onClick={() => setSelectedId(`stale:${entry.candidate.id}`)}>
          <strong>{entry.candidate.leftThreadId} ↔ {entry.candidate.rightThreadId}</strong>
          <small>{entry.reason} · 旧输入版本 {entry.inputVersion.slice(0, 12) || "未记录"}</small>
        </button>)}
      </div>}
      {preview.candidateCount > PAGE_SIZE && <div className="history-pager"><button disabled={page === 0} onClick={() => setPage(page - 1)}>上一页</button>
        <span>{page * PAGE_SIZE + 1}–{Math.min((page + 1) * PAGE_SIZE, preview.candidateCount)} / {preview.candidateCount}</span>
        <button disabled={(page + 1) * PAGE_SIZE >= preview.candidateCount} onClick={() => setPage(page + 1)}>下一页</button></div>}
      {selected && <div className="candidate-detail"><h3>{selectedStale ? "旧候选依据" : "候选依据"}</h3>
        {selectedStale && <p className="thread-warning">{selectedStale.reason} 以下是旧版双侧来源摘录。</p>}
        {selected.reasons.map((reason, index) => <p key={`${reason.signal}-${index}`}><strong>{reason.signal}</strong>：{reason.detail}</p>)}
        <h3>双侧来源证据组合 {selected.evidence.combinationsShown} / {selected.evidence.combinationsAvailable}</h3>
        <p>{selected.evidence.samplingRule} 左侧 {selected.evidence.leftSampled} / {selected.evidence.leftAvailable} 条；右侧 {selected.evidence.rightSampled} / {selected.evidence.rightAvailable} 条。</p>
        {!selected.evidence.pairs.length && <p className="analysis-note">此候选没有双侧来源摘录；关系判断可使用双方当前有效的完整会话总结，Jev 结果不会被伪装成来源引用。</p>}
        {(selected.leftSummary || selected.rightSummary) && <div className="candidate-summaries"><h3>双方完整会话总结</h3>{selected.leftSummary && <article><strong>左侧 · {selected.leftThreadId}</strong><p>{selected.leftSummary}</p></article>}{selected.rightSummary && <article><strong>右侧 · {selected.rightThreadId}</strong><p>{selected.rightSummary}</p></article>}</div>}
        {selected.evidence.pairs.map((pair, index) => <div className="candidate-pair" key={pair.id}>
          <strong>组合 {index + 1}</strong>{([pair.left, pair.right] as const).map((evidence, side) => <div key={evidence.id}>
            <small>{side === 0 ? "左侧" : "右侧"} · 回合 {evidence.turnId} · 条目 {evidence.itemId} · 内容版本 {evidence.contentVersion.slice(0, 12)}</small>
            <blockquote>{evidence.excerpt}</blockquote><button className="browse-button" onClick={() => onSelectEvidence(evidence)}>定位来源条目</button>
          </div>)}
        </div>)}
      </div>}
    </>}
  </section>;
}
