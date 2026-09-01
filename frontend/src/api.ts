import type {
  ApiErrorPayload,
  DashboardSnapshot,
  ConversationOverlayUpdate,
  GraphEdgeCreate,
  GraphEdgeUpdate,
  HealthResponse,
  ProjectView,
  SourceSummary,
} from "./types";

export class ApiError extends Error {
  readonly status: number;
  readonly payload: ApiErrorPayload;

  constructor(status: number, payload: ApiErrorPayload) {
    super(payload.message);
    this.name = "ApiError";
    this.status = status;
    this.payload = payload;
  }
}

export interface DashboardApi {
  health(): Promise<HealthResponse>;
  selectProject(path: string): Promise<{ project: ProjectView; source: SourceSummary }>;
  snapshot(): Promise<DashboardSnapshot>;
  refresh(): Promise<DashboardSnapshot>;
  updateNode(
    conversationId: string,
    changes: ConversationOverlayUpdate,
    etag: string,
  ): Promise<DashboardSnapshot>;
  createEdge(edge: GraphEdgeCreate, etag: string): Promise<DashboardSnapshot>;
  updateEdge(edgeId: string, changes: GraphEdgeUpdate, etag: string): Promise<DashboardSnapshot>;
  deleteEdge(edgeId: string, etag: string): Promise<DashboardSnapshot>;
}

type FetchLike = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export function createApi(fetchLike: FetchLike = globalThis.fetch.bind(globalThis)): DashboardApi {
  return {
    health: () => request<HealthResponse>(fetchLike, "/api/health"),
    selectProject: (path) =>
      request<{ project: ProjectView; source: SourceSummary }>(fetchLike, "/api/project/select", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ path }),
      }),
    snapshot: () => request<DashboardSnapshot>(fetchLike, "/api/snapshot"),
    refresh: () => request<DashboardSnapshot>(fetchLike, "/api/refresh", { method: "POST" }),
    updateNode: (conversationId, changes, etag) =>
      request<DashboardSnapshot>(fetchLike, `/api/graph/nodes/${encodeURIComponent(conversationId)}`, {
        method: "PATCH",
        headers: {
          "Content-Type": "application/json",
          "If-Match": etag,
        },
        body: JSON.stringify(changes),
      }),
    createEdge: (edge, etag) =>
      request<DashboardSnapshot>(fetchLike, "/api/graph/edges", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "If-Match": etag,
        },
        body: JSON.stringify(edge),
      }),
    updateEdge: (edgeId, changes, etag) =>
      request<DashboardSnapshot>(fetchLike, `/api/graph/edges/${encodeURIComponent(edgeId)}`, {
        method: "PATCH",
        headers: {
          "Content-Type": "application/json",
          "If-Match": etag,
        },
        body: JSON.stringify(changes),
      }),
    deleteEdge: (edgeId, etag) =>
      request<DashboardSnapshot>(fetchLike, `/api/graph/edges/${encodeURIComponent(edgeId)}`, {
        method: "DELETE",
        headers: { "If-Match": etag },
      }),
  };
}

async function request<T>(fetchLike: FetchLike, url: string, init?: RequestInit): Promise<T> {
  const response = await fetchLike(url, init);
  const body = (await response.json().catch(() => null)) as T | { error?: ApiErrorPayload } | null;
  if (!response.ok) {
    const payload = isErrorEnvelope(body) ? body.error : fallbackError(response.status);
    throw new ApiError(response.status, payload);
  }
  return body as T;
}

function isErrorEnvelope(value: unknown): value is { error: ApiErrorPayload } {
  return typeof value === "object" && value !== null && "error" in value && Boolean(value.error);
}

function fallbackError(status: number): ApiErrorPayload {
  return {
    code: "http_error",
    message: `请求失败（HTTP ${status}）`,
    details: null,
    retryable: status >= 500,
  };
}
