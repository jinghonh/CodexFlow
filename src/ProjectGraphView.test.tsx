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
