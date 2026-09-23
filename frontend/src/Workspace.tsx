import { ReactNode, useEffect, useState } from "react";

type Panel = "graph" | "timeline" | "list";
const names: Record<Panel, string> = { graph: "关系图", timeline: "时间线", list: "任务列表" };
interface Layout { order: Panel[]; visible: Panel[]; widths: Record<Panel, number>; heights: Record<Panel, number> }
const initial: Layout = { order: ["graph", "timeline", "list"], visible: ["graph", "timeline", "list"], widths: { graph: 100, timeline: 100, list: 100 }, heights: { graph: 620, timeline: 400, list: 520 } };
function readLayout(): Layout {
  try {
    const value = JSON.parse(localStorage.getItem("codexflow.workspace.v1") ?? "null");
    if (value && initial.order.every(id => value.order.includes(id)) && value.order.length === 3 && value.visible.length && value.visible.every((id: Panel) => initial.order.includes(id)) && initial.order.every(id => [50, 100].includes(value.widths[id]) && value.heights[id] >= 280 && value.heights[id] <= 1000)) return value;
  } catch { /* 使用默认布局，存储不可用不影响操作。 */ }
  return initial;
}
export function Workspace({ panels, detail }: { panels: Record<Panel, ReactNode>; detail: ReactNode }) {
  const [layout, setLayout] = useState(readLayout);
  useEffect(() => { try { localStorage.setItem("codexflow.workspace.v1", JSON.stringify(layout)); } catch { /* 存储被禁用时仍可使用工作区。 */ } }, [layout]);
  function move(id: Panel, delta: number) {
    setLayout(current => { const order = [...current.order]; const from = order.indexOf(id); const to = from + delta; if (to < 0 || to >= order.length) return current; [order[from], order[to]] = [order[to], order[from]]; return { ...current, order }; });
  }
  return <>
    <div className="workspace-toolbar" aria-label="工作区布局">
      <div><strong>工作区</strong><span>关联、时间与任务，同步查看</span></div>
      <div className="workspace-actions">
        {initial.order.map(id => <button key={id} aria-pressed={layout.visible.includes(id)} disabled={layout.visible.length === 1 && layout.visible.includes(id)} onClick={() => setLayout(current => ({ ...current, visible: current.visible.includes(id) ? current.visible.filter(item => item !== id) : [...current.visible, id] }))}>{names[id]}</button>)}
        <select aria-label="布局预设" value="" onChange={event => {
          const preset = event.target.value;
          setLayout({ ...initial, visible: preset === "focus" ? ["graph"] : initial.visible, widths: preset === "compare" ? { graph: 50, timeline: 50, list: 100 } : initial.widths });
        }}><option value="" disabled>布局预设</option><option value="focus">专注关系</option><option value="compare">关系与时间对照</option><option value="all">纵向总览</option></select>
      </div>
    </div>
    <div className={`workspace ${detail ? "has-detail" : ""}`}>
      <div className="workspace-panels">
        {layout.order.map((id, index) => <section key={id} hidden={!layout.visible.includes(id)} className="workspace-panel" style={{ gridColumn: layout.widths[id] === 100 ? "1 / -1" : undefined }}>
          <div className="panel-handle"><strong>{names[id]}</strong><div>
            <button aria-label={`${names[id]}前移`} disabled={index === 0} onClick={() => move(id, -1)}>↑</button>
            <button aria-label={`${names[id]}后移`} disabled={index === 2} onClick={() => move(id, 1)}>↓</button>
            <button aria-label={`调整${names[id]}宽度`} onClick={() => setLayout(current => ({ ...current, widths: { ...current.widths, [id]: current.widths[id] === 100 ? 50 : 100 } }))}>{layout.widths[id] === 100 ? "半宽" : "全宽"}</button>
            <label>高度<input aria-label={`${names[id]}高度`} type="range" min="280" max="1000" step="20" value={layout.heights[id]} onChange={event => setLayout(current => ({ ...current, heights: { ...current.heights, [id]: Number(event.target.value) } }))} /></label>
          </div></div>
          <div className="panel-content" style={{ height: layout.heights[id] }}>{panels[id]}</div>
        </section>)}
      </div>
      {detail && <aside className="workspace-detail">{detail}</aside>}
    </div>
  </>;
}
