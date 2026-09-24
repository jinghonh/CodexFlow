// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ProjectGraphView } from "./ProjectGraphView";

const layoutControl = vi.hoisted(() => ({ fail: false }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("elkjs/lib/elk.bundled.js", () => ({
  default: class {
    async layout(graph: { children: { id: string }[] }) {
      if (layoutControl.fail) throw new Error("布局失败");
      return { children: graph.children.map((node, index) => ({ ...node, x: index * 300, y: 0 })) };
    }
  },
}));
vi.mock("@xyflow/react", () => ({
  MarkerType: { ArrowClosed: "arrowclosed" },
  Position: { Left: "left", Right: "right" },
  Handle: () => null,
  Background: () => null,
  Controls: () => null,
  ReactFlow: ({ nodes, edges, onNodeClick, onEdgeClick, onPaneClick }: {
    nodes: { id: string; data: { referenceOnly: boolean } }[];
    edges: { id: string; source: string; target: string; label: string; style: { strokeDasharray?: string } }[];
    onNodeClick: (event: MouseEvent, node: { id: string }) => void;
    onEdgeClick: (event: MouseEvent, edge: { id: string }) => void;
    onPaneClick: () => void;
  }) => <div>
    {nodes.map((node) => <button key={node.id} onClick={(event) => onNodeClick(event.nativeEvent, node)}>{node.id}{node.data.referenceOnly ? "（引用）" : ""}</button>)}
    {edges.map((edge) => <button key={edge.id} data-source={edge.source} data-target={edge.target}
      data-dashed={Boolean(edge.style.strokeDasharray)} onClick={(event) => onEdgeClick(event.nativeEvent, edge)}>{edge.label}</button>)}
    <button onClick={onPaneClick}>画布空白</button>
  </div>,
}));

afterEach(() => { cleanup(); vi.clearAllMocks(); layoutControl.fail = false; });

test("固定项目图展示来源方向、双关系、缺失端点诊断和选择详情", async () => {
  vi.mocked(invoke).mockResolvedValue({
    project: { id: "project", name: "示例项目" },
    nodes: [
      { id: "root", title: "父会话", referenceOnly: false },
      { id: "child", title: "后续会话", referenceOnly: false },
      { id: "missing", title: null, referenceOnly: true },
    ],
    relations: [
      { id: "fork", projectId: "project", fromThreadId: "root", toThreadId: "child", kind: "FORKED_FROM", source: "observed", sourceField: "forkedFromId", confidence: 1.0, parentEndpoint: "inProject" },
      { id: "subagent", projectId: "project", fromThreadId: "root", toThreadId: "child", kind: "SUBAGENT_OF", source: "observed", sourceField: "parentThreadId", confidence: 1.0, parentEndpoint: "inProject" },
      { id: "unresolved", projectId: "project", fromThreadId: "missing", toThreadId: "root", kind: "FORKED_FROM", source: "observed", sourceField: "forkedFromId", confidence: 1.0, parentEndpoint: "missing" },
    ],
    diagnostics: [{ threadId: "root", sourceField: "parentThreadId", referencedThreadId: "root", message: "来源引用了会话自身，已忽略自环。" }],
  });
  render(<ProjectGraphView projectId="project" refreshVersion={0} />);

  const fork = (await screen.findAllByRole("button", { name: "观察 · 派生 · 1.0" }))
    .find((button) => button.dataset.source === "root")!;
  expect(fork.dataset.source).toBe("root");
  expect(fork.dataset.target).toBe("child");
  expect(fork.dataset.dashed).toBe("false");
  expect(screen.getByRole("button", { name: "观察 · 子代理 · 1.0" }).dataset.source).toBe("root");
  expect(screen.getByText(/父会话尚未出现在本机缓存/)).toBeTruthy();
  expect(screen.getByText(/已忽略自环/)).toBeTruthy();

  fireEvent.click(fork);
  expect(screen.getByRole("heading", { name: "观察关系 · 派生" })).toBeTruthy();
  expect(within(document.querySelector(".graph-details") as HTMLElement).getByText("forkedFromId")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "missing（引用）" }));
  expect(screen.getByRole("heading", { name: "仅有引用的端点" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "画布空白" }));
  expect(screen.getByText("选择节点或关系以查看来源详情。")).toBeTruthy();
});

test("布局器失败时仍显示全部结构关系", async () => {
  layoutControl.fail = true;
  vi.mocked(invoke).mockResolvedValue({
    project: { id: "project", name: "示例项目" },
    nodes: [
      { id: "first", title: "甲", referenceOnly: false },
      { id: "second", title: "乙", referenceOnly: false },
    ],
    relations: [{ id: "cycle", projectId: "project", fromThreadId: "second", toThreadId: "first", kind: "SUBAGENT_OF", source: "observed", sourceField: "parentThreadId", confidence: 1.0, parentEndpoint: "inProject" }],
    diagnostics: [],
  });
  render(<ProjectGraphView projectId="project" refreshVersion={0} />);
  const edge = await screen.findByRole("button", { name: "观察 · 子代理 · 1.0" });
  expect(edge.dataset.source).toBe("second");
  expect(edge.dataset.target).toBe("first");
  expect(screen.getByText("自动布局未完成，已按固定顺序展示全部会话和关系。")).toBeTruthy();
});

test("规则关系显示灰色事实依据和来源定位", async () => {
  vi.mocked(invoke).mockResolvedValue({
    project: { id: "project", name: "示例项目" },
    nodes: [{ id: "thread-a", title: "甲", referenceOnly: false }, { id: "thread-b", title: "乙", referenceOnly: false }],
    relations: [],
    derivedRelations: [{ id: "derived", projectId: "project", fromThreadId: "thread-a", toThreadId: "thread-b",
      kind: "SHARED_FILE", source: "derived", basis: "双方来源条目记录同一文件：src/lib.rs",
      evidence: [{ id: "proof", threadId: "thread-a", turnId: "turn-1", itemId: "item-1", excerpt: "src/lib.rs", contentVersion: "version-1" }] }],
    diagnostics: [],
  });
  const onSelectEvidence = vi.fn();
  render(<ProjectGraphView projectId="project" refreshVersion={0} onSelectEvidence={onSelectEvidence} />);
  const edge = await screen.findByRole("button", { name: "规则 · 共同文件" });
  expect(edge.dataset.dashed).toBe("true");
  fireEvent.click(edge);
  expect(screen.getByRole("heading", { name: "规则关系 · 共同文件" })).toBeTruthy();
  expect(screen.getByText("双方来源条目记录同一文件：src/lib.rs")).toBeTruthy();
  expect(screen.getByText(/不是模型判断或概率/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "定位来源条目" }));
  expect(onSelectEvidence).toHaveBeenCalledWith(expect.objectContaining({ turnId: "turn-1", itemId: "item-1" }));
});

test("推断关系按置信度隐藏并能查看双侧证据", async () => {
  const proof = (id: string, threadId: string) => ({ id, threadId, turnId: `turn-${threadId}`, itemId: `item-${threadId}`, excerpt: `${threadId} 的真实摘录`, contentVersion: "v1" });
  vi.mocked(invoke).mockResolvedValue({
    project: { id: "project", name: "示例项目" },
    nodes: [{ id: "thread-a", title: "甲", referenceOnly: false }, { id: "thread-b", title: "乙", referenceOnly: false }],
    relations: [], derivedRelations: [], diagnostics: [],
    inferredRelations: [{ id: "inferred", projectId: "project", candidateId: "candidate", fromThreadId: "thread-a", toThreadId: "thread-b",
      kind: "FIXES", source: "jev", requestedModel: "jev-latest", actualModel: "jev-1.13.0", confidence: 0.65,
      probabilities: { SUPPORTS: 0.7, REJECTS: 0.2, UNKNOWN: 0.1 }, evidenceConfidence: 0.9,
      evidenceProbabilities: { p0: 0.9, INSUFFICIENT: 0.1 }, evidence: { id: "pair", left: proof("left", "thread-a"), right: proof("right", "thread-b") },
      timeCheck: "unverifiable", explanation: "Jev 判定 FIXES；本地依据摘录整理。", inputVersion: "v1" }],
    inferenceOutcomes: [{ candidateId: "candidate", status: "valid", unknownCount: 0 }, { candidateId: "none", status: "none", unknownCount: 0 }],
  });
  const onSelectEvidence = vi.fn();
  render(<ProjectGraphView projectId="project" refreshVersion={0} onSelectEvidence={onSelectEvidence} />);
  expect(await screen.findByText(/低于 0.70 的推断关系/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: /推断 · 修复/ })).toBeNull();
  fireEvent.click(screen.getByRole("checkbox", { name: /低于 0.70/ }));
  const edge = await screen.findByRole("button", { name: "推断 · 修复 · 0.65" });
  expect(edge.dataset.source).toBe("thread-a");
  expect(edge.dataset.target).toBe("thread-b");
  fireEvent.click(edge);
  expect(screen.getByRole("heading", { name: "推断关系 · 修复" })).toBeTruthy();
  expect(screen.getByText(/Jev · jev-1.13.0/)).toBeTruthy();
  expect(screen.getByText("来源时间不足，无法验证顺序")).toBeTruthy();
  const buttons = screen.getAllByRole("button", { name: "定位来源条目" });
  expect(buttons).toHaveLength(2);
  fireEvent.click(buttons[1]);
  expect(onSelectEvidence).toHaveBeenCalledWith(expect.objectContaining({ threadId: "thread-b", itemId: "item-thread-b" }));
  expect(screen.getByText(/无关系 1/)).toBeTruthy();
});

test("确认、拒绝和显式恢复立即更新图边且保留推断来源", async () => {
  const proof = (id: string, threadId: string) => ({ id, threadId, turnId: "turn", itemId: id, excerpt: "来源摘录", contentVersion: "v1" });
  const relation = { id: "inferred", projectId: "project", candidateId: "candidate", fromThreadId: "thread-a", toThreadId: "thread-b",
    kind: "RELATED" as const, source: "jev" as const, requestedModel: "jev-latest", actualModel: "jev-1.13.0", confidence: 0.55,
    probabilities: {}, evidenceConfidence: 0.9, evidenceProbabilities: {},
    evidence: { id: "pair", left: proof("left", "thread-a"), right: proof("right", "thread-b") },
    timeCheck: "unverifiable" as const, explanation: "本地整理的依据", inputVersion: "input-v1" };
  let decision: "pending" | "confirmed" | "rejected" = "pending";
  let revision = 0;
  const graph = () => ({ project: { id: "project", name: "项目" },
    nodes: [{ id: "thread-a", title: "甲", referenceOnly: false }, { id: "thread-b", title: "乙", referenceOnly: false }],
    relations: [], derivedRelations: [], diagnostics: [],
    inferredRelations: decision === "rejected" ? [] : [relation],
    reviewedRelations: [{ ...relation, evidenceVersion: "evidence-v1", evidenceValid: true,
      review: { relationId: relation.id, projectId: "project", decision, revision,
        confirmedEvidenceVersion: decision === "confirmed" ? "evidence-v1" : null } }],
  });
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "get_project_graph") return graph();
    if (command === "decide_inferred_relation") {
      const request = args as { decision: typeof decision; expectedRevision: number; expectedEvidenceVersion: string };
      expect(request.expectedRevision).toBe(revision);
      expect(request.expectedEvidenceVersion).toBe("evidence-v1");
      decision = request.decision;
      revision += 1;
      return { relationId: relation.id, decision, revision };
    }
    throw new Error(`unexpected command ${command}`);
  });
  render(<ProjectGraphView projectId="project" refreshVersion={0} />);
  fireEvent.click(await screen.findByRole("checkbox", { name: /低于 0.70/ }));
  fireEvent.click(await screen.findByRole("button", { name: "推断 · 相关 · 0.55" }));
  fireEvent.click(screen.getByRole("button", { name: "确认关系" }));
  expect(await screen.findByText("已确认")).toBeTruthy();
  fireEvent.click(screen.getByRole("checkbox", { name: /低于 0.70/ }));
  expect(await screen.findByRole("button", { name: "推断 · 相关 · 0.55" })).toBeTruthy();
  expect(screen.getByText(/Jev · jev-1.13.0/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "拒绝关系" }));
  expect(await screen.findByText("已拒绝")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "推断 · 相关 · 0.55" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "恢复待裁决" }));
  expect(await screen.findByText("待裁决")).toBeTruthy();
  expect(revision).toBe(3);
});

test("过期证据不能确认，过期写入提示刷新", async () => {
  const proof = (threadId: string) => ({ id: threadId, threadId, turnId: "turn", itemId: "item", excerpt: "摘录", contentVersion: "old" });
  const relation = { id: "stale", projectId: "project", candidateId: "candidate", fromThreadId: "a", toThreadId: "b",
    kind: "FIXES" as const, source: "jev" as const, requestedModel: "jev-latest", actualModel: "jev-1.13.0", confidence: 0.8,
    probabilities: {}, evidenceConfidence: 0.9, evidenceProbabilities: {}, evidence: { id: "pair", left: proof("a"), right: proof("b") },
    timeCheck: "verified" as const, explanation: "旧依据", inputVersion: "old" };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_project_graph") return { project: { id: "project", name: "项目" }, nodes: [], relations: [], diagnostics: [],
      inferredRelations: [], reviewedRelations: [{ ...relation, evidenceVersion: "old-evidence", evidenceValid: false,
        review: { relationId: "stale", projectId: "project", decision: "confirmed", revision: 2, confirmedEvidenceVersion: "old-evidence" } }] };
    if (command === "decide_inferred_relation") throw { code: "CONCURRENT_MODIFICATION", message: "冲突" };
    throw new Error(`unexpected command ${command}`);
  });
  render(<ProjectGraphView projectId="project" refreshVersion={0} />);
  fireEvent.click(await screen.findByRole("button", { name: /修复 · 已确认；当前关系未验证或证据已过期/ }));
  expect(screen.getByRole("button", { name: "确认关系" }).hasAttribute("disabled")).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "恢复待裁决" }));
  expect(await screen.findByText(/关系裁决已被更新，请刷新关系图后重试/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "刷新关系图" })).toBeTruthy();
});

test("旧确认不会让新证据的低分关系默认显示", async () => {
  const proof = (threadId: string) => ({ id: threadId, threadId, turnId: "turn", itemId: "item", excerpt: "新证据", contentVersion: "new" });
  const relation = { id: "same-identity", projectId: "project", candidateId: "candidate", fromThreadId: "a", toThreadId: "b",
    kind: "RELATED" as const, source: "jev" as const, requestedModel: "jev-new", actualModel: "jev-2.0.0", confidence: 0.51,
    probabilities: {}, evidenceConfidence: 0.9, evidenceProbabilities: {}, evidence: { id: "pair", left: proof("a"), right: proof("b") },
    timeCheck: "unverifiable" as const, explanation: "新分析", inputVersion: "input-new" };
  vi.mocked(invoke).mockResolvedValue({ project: { id: "project", name: "项目" },
    nodes: [{ id: "a", title: "甲", referenceOnly: false }, { id: "b", title: "乙", referenceOnly: false }],
    relations: [], diagnostics: [], inferredRelations: [relation], reviewedRelations: [{ ...relation,
      evidenceVersion: "new-version", evidenceValid: true, review: { relationId: relation.id, projectId: "project",
        decision: "confirmed", revision: 1, confirmedEvidenceVersion: "old-version" } }] });
  render(<ProjectGraphView projectId="project" refreshVersion={0} />);
  await screen.findByRole("checkbox", { name: /低于 0.70/ });
  expect(screen.queryByRole("button", { name: "推断 · 相关 · 0.51" })).toBeNull();
  fireEvent.click(screen.getByRole("checkbox", { name: /低于 0.70/ }));
  fireEvent.click(await screen.findByRole("button", { name: "推断 · 相关 · 0.51" }));
  expect(screen.getByText("已确认；当前证据尚未确认")).toBeTruthy();
  expect(screen.getByRole("button", { name: "确认关系" }).hasAttribute("disabled")).toBe(false);
});
