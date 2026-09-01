import type { Conversation, ConversationFilters } from "./types";

export const DEFAULT_CONVERSATION_FILTERS: ConversationFilters = {
  search: "",
  tag: null,
  userStatus: "all",
  archived: "all",
  missing: "all",
  unlinked: "all",
  hidden: "all",
  sortBy: "source",
};

export function filterConversations(
  conversations: readonly Conversation[],
  filters: ConversationFilters,
): Conversation[] {
  const query = filters.search.trim().toLocaleLowerCase();

  const filtered = conversations.filter((conversation) => {
    if (query && !searchableConversationFields(conversation).some((field) =>
      field.toLocaleLowerCase().includes(query),
    )) {
      return false;
    }
    if (filters.tag !== null && !conversation.overlay.tags.includes(filters.tag)) {
      return false;
    }
    if (filters.userStatus !== "all" && conversation.overlay.status !== filters.userStatus) {
      return false;
    }
    if (filters.archived === "archived" && !conversation.codex.archived) {
      return false;
    }
    if (filters.archived === "active" && conversation.codex.archived) {
      return false;
    }
    if (filters.missing === "missing" && !conversation.derived.missing) {
      return false;
    }
    if (filters.missing === "present" && conversation.derived.missing) {
      return false;
    }
    if (filters.unlinked !== "all") {
      if (conversation.derived.missing) return false;
      if (filters.unlinked === "unlinked" && !conversation.derived.unlinked) return false;
      if (filters.unlinked === "linked" && conversation.derived.unlinked) return false;
    }
    if (filters.hidden === "hidden" && !conversation.overlay.hidden) {
      return false;
    }
    if (filters.hidden === "visible" && conversation.overlay.hidden) {
      return false;
    }
    return true;
  });

  const sortBy = filters.sortBy;
  if (sortBy === "source") return filtered;
  return [...filtered].sort((left, right) => compareConversationDates(left, right, sortBy));
}

function compareConversationDates(
  left: Conversation,
  right: Conversation,
  sortBy: "createdAt" | "updatedAt",
): number {
  const leftValue = Date.parse(timestampForSort(left, sortBy));
  const rightValue = Date.parse(timestampForSort(right, sortBy));
  if (Number.isFinite(leftValue) && Number.isFinite(rightValue)) return rightValue - leftValue;
  if (Number.isFinite(leftValue)) return -1;
  if (Number.isFinite(rightValue)) return 1;
  return 0;
}

function timestampForSort(conversation: Conversation, sortBy: "createdAt" | "updatedAt"): string {
  return sortBy === "createdAt"
    ? conversation.codex.createdAt ?? ""
    : conversation.codex.updatedAt ?? "";
}

export function isDefaultConversationFilters(filters: ConversationFilters): boolean {
  return Object.entries(DEFAULT_CONVERSATION_FILTERS).every(([key, defaultValue]) => {
    const currentValue = filters[key as keyof ConversationFilters];
    return key === "search"
      ? filters.search.trim() === defaultValue
      : currentValue === defaultValue;
  });
}

function searchableConversationFields(conversation: Conversation): string[] {
  return [
    conversation.displayTitle,
    conversation.codex.title ?? "",
    conversation.codex.preview,
    conversation.id,
  ];
}
