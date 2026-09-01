import type {
  ApiErrorPayload,
  DashboardSnapshot,
  ConversationOverlayUpdate,
  GraphCopyResult,
  GraphDocument,
  GraphEdgeCreate,
  GraphEdgeUpdate,
  GraphMigrationResponse,
  HealthResponse,
  ProjectView,
  SourceSummary,
  TimelineGranularity,
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

export interface TimelineOptions {
  granularity: TimelineGranularity;
  timezone: string;
}

export interface DashboardApi {
  health(): Promise<HealthResponse>;
  selectProject(path: string): Promise<{ project: ProjectView; source: SourceSummary }>;
  snapshot(options?: TimelineOptions): Promise<DashboardSnapshot>;
  refresh(options?: TimelineOptions): Promise<DashboardSnapshot>;
  migrateGraph(etag: string, overwrite?: boolean): Promise<GraphMigrationResponse>;
  saveCopy(document: GraphDocument): Promise<GraphCopyResult>;
  updateNode(
    conversationId: string,
    changes: ConversationOverlayUpdate,
    etag: string,
    overwrite?: boolean,
  ): Promise<DashboardSnapshot>;
  createEdge(edge: GraphEdgeCreate, etag: string, overwrite?: boolean): Promise<DashboardSnapshot>;
  updateEdge(edgeId: string, changes: GraphEdgeUpdate, etag: string, overwrite?: boolean): Promise<DashboardSnapshot>;
  deleteEdge(edgeId: string, etag: string, overwrite?: boolean): Promise<DashboardSnapshot>;
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
    snapshot: (options) => request<DashboardSnapshot>(fetchLike, withTimelineOptions("/api/snapshot", options)),
    refresh: (options) => request<DashboardSnapshot>(fetchLike, withTimelineOptions("/api/refresh", options), { method: "POST" }),
    migrateGraph: (etag, overwrite = false) =>
      request<GraphMigrationResponse>(fetchLike, withOverwrite("/api/graph/migrate", overwrite), {
        method: "POST",
        headers: { "If-Match": etag },
      }),
    saveCopy: (document) =>
      request<GraphCopyResult>(fetchLike, "/api/graph/copy", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ document }),
      }),
    updateNode: (conversationId, changes, etag, overwrite = false) =>
      request<DashboardSnapshot>(fetchLike, withOverwrite(`/api/graph/nodes/${encodeURIComponent(conversationId)}`, overwrite), {
        method: "PATCH",
        headers: {
          "Content-Type": "application/json",
          "If-Match": etag,
        },
        body: JSON.stringify(changes),
      }),
    createEdge: (edge, etag, overwrite = false) =>
      request<DashboardSnapshot>(fetchLike, withOverwrite("/api/graph/edges", overwrite), {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "If-Match": etag,
        },
        body: JSON.stringify(edge),
      }),
    updateEdge: (edgeId, changes, etag, overwrite = false) =>
      request<DashboardSnapshot>(fetchLike, withOverwrite(`/api/graph/edges/${encodeURIComponent(edgeId)}`, overwrite), {
        method: "PATCH",
        headers: {
          "Content-Type": "application/json",
          "If-Match": etag,
        },
        body: JSON.stringify(changes),
      }),
    deleteEdge: (edgeId, etag, overwrite = false) =>
      request<DashboardSnapshot>(fetchLike, withOverwrite(`/api/graph/edges/${encodeURIComponent(edgeId)}`, overwrite), {
        method: "DELETE",
        headers: { "If-Match": etag },
      }),
  };
}

function withTimelineOptions(path: string, options?: TimelineOptions): string {
  if (!options) return path;
  const params = new URLSearchParams({
    granularity: options.granularity,
    timezone: options.timezone,
  });
  return `${path}?${params.toString()}`;
}

function withOverwrite(path: string, overwrite: boolean): string {
  return overwrite ? `${path}?overwrite=true` : path;
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
