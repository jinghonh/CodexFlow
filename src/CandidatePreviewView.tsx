import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Evidence = { id: string; threadId: string; turnId: string; itemId: string; excerpt: string; contentVersion: string };
type Pair = { id: string; left: Evidence; right: Evidence };
type Candidate = { id: string; leftThreadId: string; rightThreadId: string; score: number;
  reasons: { signal: string; detail: string }[];
  evidence: { leftAvailable: number; rightAvailable: number; combinationsAvailable: number;
    combinationsShown: number; leftSampled: number; rightSampled: number; samplingRule: string; pairs: Pair[] } };
type Preview = { projectId: string; threadCount: number; neighborLimit: number; candidateCount: number; candidates: Candidate[] };
const PAGE_SIZE = 25;

export function CandidatePreviewView({ projectId, refreshVersion, onSelectEvidence }: {
  projectId: string; refreshVersion: string; onSelectEvidence: (evidence: Evidence) => void;
}) {
  const [preview, setPreview] = useState<Preview | null>(null);
  const [error, setError] = useState("");
  const [page, setPage] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    setPreview(null); setError(""); setPage(0); setSelectedId(null);
    invoke<Preview>("get_candidate_preview", { projectId })
      .then((value) => { if (active) setPreview(value); })
      .catch((caught) => { if (active) setError(typeof caught?.message === "string" ? caught.message : "候选清单读取失败。"); });
    return () => { active = false; };
  }, [projectId, refreshVersion]);
  const selected = preview?.candidates.find((candidate) => candidate.id === selectedId);
  return <section className="panel candidate-panel" aria-label="关系候选预览">
    <div className="panel-kicker">05 / 关系候选预览</div>
    <h2>待分析候选</h2>
    <p className="panel-intro">候选仅用于后续关系判断，排序分数是本地筛选分数，不是关系概率或已确认的语义关系。</p>
    {error && <p className="page-error" role="alert">{error}</p>}
    {!preview && !error && <p>正在计算候选…</p>}
    {preview && <>
      <div className="candidate-summary"><strong>{preview.candidateCount} 对候选</strong><span>{preview.threadCount} 条会话</span><span>每条会话最多选取 {preview.neighborLimit} 个邻居；无序候选对去重</span></div>
      {preview.candidateCount === 0 && <p className="empty-list">当前没有具备筛选信号的候选。</p>}
      <div className="candidate-list">
        {preview.candidates.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE).map((candidate) => <button key={candidate.id}
          className={`candidate-row ${selectedId === candidate.id ? "selected" : ""}`} onClick={() => setSelectedId(candidate.id)}>
          <strong>{candidate.leftThreadId} ↔ {candidate.rightThreadId}</strong><span>筛选分数 {candidate.score}</span>
          <small>{candidate.reasons.map((reason) => reason.detail).join("；")}</small>
        </button>)}
      </div>
      {preview.candidateCount > PAGE_SIZE && <div className="history-pager"><button disabled={page === 0} onClick={() => setPage(page - 1)}>上一页</button>
        <span>{page * PAGE_SIZE + 1}–{Math.min((page + 1) * PAGE_SIZE, preview.candidateCount)} / {preview.candidateCount}</span>
        <button disabled={(page + 1) * PAGE_SIZE >= preview.candidateCount} onClick={() => setPage(page + 1)}>下一页</button></div>}
      {selected && <div className="candidate-detail"><h3>候选依据</h3>
        {selected.reasons.map((reason, index) => <p key={`${reason.signal}-${index}`}><strong>{reason.signal}</strong>：{reason.detail}</p>)}
        <h3>双侧来源证据组合 {selected.evidence.combinationsShown} / {selected.evidence.combinationsAvailable}</h3>
        <p>{selected.evidence.samplingRule} 左侧 {selected.evidence.leftSampled} / {selected.evidence.leftAvailable} 条；右侧 {selected.evidence.rightSampled} / {selected.evidence.rightAvailable} 条。</p>
        {!selected.evidence.pairs.length && <p className="thread-warning">证据不足：至少一侧没有可定位的来源条目。后续判断不得凭总结补造证据。</p>}
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
