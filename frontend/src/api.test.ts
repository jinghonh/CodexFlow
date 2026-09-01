import { describe, expect, it, vi } from "vitest";

import { createApi } from "./api";

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

describe("Graph persistence API", () => {
  it("sends an explicit overwrite marker with a wildcard ETag", async () => {
    const fetchLike = vi.fn().mockResolvedValue(jsonResponse({}));
    const api = createApi(fetchLike);

    await api.updateNode("thread/id", { title: "Draft" }, "*", true);

    expect(fetchLike).toHaveBeenCalledWith(
      "/api/graph/nodes/thread%2Fid?overwrite=true",
      expect.objectContaining({
        method: "PATCH",
        headers: expect.objectContaining({ "If-Match": "*" }),
      }),
    );
  });

  it("posts a Graph document to the copy endpoint without an ETag", async () => {
    const fetchLike = vi.fn().mockResolvedValue(
      jsonResponse({ copyPath: "/project/.codex/graph.yaml.copy.fixture" }),
    );
    const api = createApi(fetchLike);
    const document = { version: 1, nodes: { "thread/id": { title: "Draft" } } };

    await api.saveCopy(document);

    expect(fetchLike).toHaveBeenCalledWith(
      "/api/graph/copy",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ document }),
      }),
    );
  });
});
