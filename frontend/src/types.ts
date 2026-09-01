export type SourceStatus = "ready" | "stale" | "unavailable" | "incompatible";

export interface ApiErrorPayload {
  code: string;
  message: string;
  details: Record<string, unknown> | null;
  retryable: boolean;
}
export interface SourceSummary {
  status: SourceStatus;
  userAgent: string | null;
  error: ApiErrorPayload | null;
}

export interface ProjectView {
  originalPath: string;
  realPath: string;
  gitRoot: string | null;
  worktreeRoot: string | null;
  isGitProject: boolean;
  graphFile: string;
  graphFileStatus: string;
}

export interface HealthResponse {
  status: string;
  listenHost: string;
  source: SourceSummary;
  project: ProjectView | null;
}

export interface CodexMetadata {
  title: string | null;
  preview: string;
  createdAt: string | null;
  updatedAt: string | null;
  recencyAt: string | null;
  cwd: string;
  source: string;
  archived: boolean;
  historyMode: string | null;
  status: string;
  projectId: string | null;
  gitInfo: Record<string, unknown> | null;
}

export interface ConversationOverlay {
  title: string | null;
  tags: string[];
  status: string;
  note: string | null;
  hidden: boolean;
  layout: { x: number; y: number } | null;
}

export interface DerivedConversationState {
  missing: boolean;
  unlinked: boolean;
  sourceAvailable: boolean;
  validObservationRange: boolean;
}

export interface Conversation {
  id: string;
  displayTitle: string;
  codex: CodexMetadata;
  overlay: ConversationOverlay;
  derived: DerivedConversationState;
}

export interface DashboardSnapshot {
  project: ProjectView;
  source: {
    status: SourceStatus;
    generatedAt: string | null;
    userAgent: string | null;
    error: ApiErrorPayload | null;
  };
  graph: { etag: string; fileStatus: string; edges: unknown[] };
  conversations: Conversation[];
}
