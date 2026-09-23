import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { automaticPositions, edgeGeometry, NODE_HEIGHT, NODE_WIDTH } from "./graphGeometry";
import { rangeStyle, TimelineView } from "./Timeline";
import { Workspace } from "./Workspace";
import type { GraphNode, ObservationRange, TimelineSnapshot } from "./types";
import { snapshot } from "./test/dashboard";

const node = (id: string): GraphNode => ({ id, displayTitle: id, hidden: false, missing: false, layout: null });
describe("可视化几何与工作区", () => {
  it("关联任务相邻，输入顺序变化不改变自动布局", () => {
    const nodes = [node("c"), node("a"), node("b")];
    const edges = [{ id: "e", source: "a", target: "c", type: "depends_on" }];
    const positions = automaticPositions(nodes, edges);
    expect(positions).toEqual(automaticPositions([...nodes].reverse(), edges));
    expect(positions.get("a")!.y).toBe(positions.get("c")!.y);
    expect(positions.get("b")!.y).toBeGreaterThan(positions.get("a")!.y);
  });
  it("箭头终点位于节点边缘外，多条关系路径不同", () => {
    const geometry = edgeGeometry({ x: 0, y: 0 }, { x: 400, y: 0 });
    expect(geometry.path).toBe(`M ${NODE_WIDTH + 7} ${NODE_HEIGHT / 2} Q 310 58 393 58`);
    expect(edgeGeometry({ x: 0, y: 0 }, { x: 400, y: 0 }, 45).path).not.toBe(edgeGeometry({ x: 0, y: 0 }, { x: 400, y: 0 }, -45).path);
  });
  it("按真实桶时长计算区间，裁剪窗口外部分", () => {
    const timeline: TimelineSnapshot = { granularity: "day", timezone: "UTC", warnings: [], ranges: [], buckets: [
      { start: "2024-01-01T00:00:00Z", end: "2024-01-02T00:00:00Z", label: "1", overlapCount: 1 },
      { start: "2024-01-02T00:00:00Z", end: "2024-01-03T00:00:00Z", label: "2", overlapCount: 1 },
    ] };
    const range: ObservationRange = { conversationId: "a", start: "2024-01-01T06:00:00Z", end: "2024-01-01T12:00:00Z", valid: true, isPoint: false, error: null };
    expect(rangeStyle(range, timeline)).toEqual({ left: "12.5%", width: "12.5%" });
    expect(rangeStyle({ ...range, start: "2023-12-01T00:00:00Z", end: "2024-02-01T00:00:00Z" }, timeline)).toEqual({ left: "0%", width: "100%" });
    expect(rangeStyle({ ...range, isPoint: true }, timeline)).toEqual({ left: "12.5%", width: "10px" });
  });
  it("时间线从最近一周开始，滑块可连续浏览，选中任务后定位其区间", () => {
    const buckets = Array.from({ length: 15 }, (_, index) => ({
      start: new Date(Date.UTC(2024, 0, index + 1)).toISOString(),
      end: new Date(Date.UTC(2024, 0, index + 2)).toISOString(),
      label: `第${index + 1}日`,
      overlapCount: 1,
    }));
    const timeline: TimelineSnapshot = {
      granularity: "day", timezone: "UTC", buckets, warnings: [],
      ranges: [{ conversationId: "active-id", start: buckets[1].start, end: buckets[2].start, valid: true, isPoint: false, error: null }],
    };
    const view = render(<TimelineView timeline={timeline} conversations={snapshot.conversations} selectedId={null} loading={false} error={null} onSelect={() => undefined} onOptionsChange={async () => undefined} />);
    expect(screen.getByText("第9日")).toBeInTheDocument();
    expect(screen.queryByText("第1日")).not.toBeInTheDocument();
    fireEvent.change(screen.getByRole("slider", { name: "时间线位置" }), { target: { value: "0" } });
    expect(screen.getByText("第1日")).toBeInTheDocument();
    view.rerender(<TimelineView timeline={timeline} conversations={snapshot.conversations} selectedId="active-id" loading={false} error={null} onSelect={() => undefined} onOptionsChange={async () => undefined} />);
    expect(screen.getByText("第1日")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "回到最近" })).toBeEnabled();
  });
  it("面板开关、顺序、尺寸持久化，至少保留一个视图", () => {
    const panels = { graph: <div>图内容</div>, timeline: <div>时间内容</div>, list: <div>列表内容</div> };
    const view = render(<Workspace panels={panels} detail={null} />);
    fireEvent.click(screen.getByRole("button", { name: "关系图后移" }));
    fireEvent.change(screen.getByRole("slider", { name: "关系图高度" }), { target: { value: "800" } });
    fireEvent.click(screen.getByRole("button", { name: "时间线" }));
    fireEvent.click(screen.getByRole("button", { name: "任务列表" }));
    expect(screen.getByRole("button", { name: "关系图" })).toBeDisabled();
    view.unmount();
    render(<Workspace panels={panels} detail={null} />);
    expect(screen.getByRole("slider", { name: "关系图高度" })).toHaveValue("800");
    expect(screen.getByText("时间内容")).not.toBeVisible();
    expect(JSON.parse(localStorage.getItem("codexflow.workspace.v1")!).order).toEqual(["timeline", "graph", "list"]);
  });
  it("无效的保存布局回退到可用默认值", () => {
    localStorage.setItem("codexflow.workspace.v1", '{"order":[]}');
    render(<Workspace panels={{ graph: "图", timeline: "时间", list: "列表" }} detail={null} />);
    expect(screen.getByRole("slider", { name: "关系图高度" })).toHaveValue("620");
  });
});
