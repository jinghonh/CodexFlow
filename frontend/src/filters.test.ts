import { describe, expect, it } from "vitest";

import type { Conversation } from "./types";
import {
  DEFAULT_CONVERSATION_FILTERS,
  filterConversations,
} from "./filters";

const conversation = (overrides: Partial<Conversation> = {}): Conversation => ({
  id: "conversation-id",
  displayTitle: "Display title",
  codex: {
    title: "Codex title",
    preview: "Preview text",
    createdAt: "2024-01-01T00:00:00Z",
    updatedAt: "2024-01-01T01:00:00Z",
    recencyAt: null,
    cwd: "/projects/codexflow",
    source: "cli",
    archived: false,
    historyMode: null,
    status: "idle",
    projectId: null,
    gitInfo: null,
  },
  overlay: {
    title: null,
    tags: [],
    status: "none",
    note: null,
    hidden: false,
    layout: null,
  },
  derived: {
    missing: false,
    unlinked: true,
    sourceAvailable: true,
    validObservationRange: true,
  },
  ...overrides,
});

describe("conversation filters", () => {
  it("searches display title, Codex title, preview, and Conversation ID without changing identity", () => {
    const conversations = [
      conversation(),
      conversation({
        id: "preview-match",
        displayTitle: "Another conversation",
        codex: {
          ...conversation().codex,
          title: "A different title",
          preview: "Need the preview match",
        },
      }),
      conversation({
        id: "id-match",
        displayTitle: "Another conversation",
        codex: { ...conversation().codex, title: "A different title", preview: "No match" },
      }),
    ];

    const previewMatches = filterConversations(conversations, {
      ...DEFAULT_CONVERSATION_FILTERS,
      search: "PREVIEW MATCH",
    });
    expect(previewMatches).toEqual([conversations[1]]);
    expect(previewMatches[0]).toBe(conversations[1]);
    expect(filterConversations(conversations, { ...DEFAULT_CONVERSATION_FILTERS, search: "id-match" }))
      .toEqual([conversations[2]]);
    expect(filterConversations(conversations, { ...DEFAULT_CONVERSATION_FILTERS, search: "DISPLAY TITLE" }))
      .toEqual([conversations[0]]);
    expect(filterConversations(conversations, { ...DEFAULT_CONVERSATION_FILTERS, search: "CODEX TITLE" }))
      .toEqual([conversations[0]]);
  });

  it("filters by tag and User status while preserving the original Conversation IDs", () => {
    const tagged = conversation({
      id: "tagged-id",
      overlay: {
        ...conversation().overlay,
        tags: ["release"],
        status: "done",
      },
    });
    const untagged = conversation({ id: "untagged-id" });

    expect(filterConversations([tagged, untagged], {
      ...DEFAULT_CONVERSATION_FILTERS,
      tag: "release",
    }).map(({ id }) => id)).toEqual(["tagged-id"]);
    expect(filterConversations([tagged, untagged], {
      ...DEFAULT_CONVERSATION_FILTERS,
      userStatus: "done",
    }).map(({ id }) => id)).toEqual(["tagged-id"]);

    const literalAllTag = conversation({
      id: "literal-all-tag",
      overlay: { ...conversation().overlay, tags: ["all"] },
    });
    expect(filterConversations([literalAllTag], {
      ...DEFAULT_CONVERSATION_FILTERS,
      tag: "all",
    }).map(({ id }) => id)).toEqual(["literal-all-tag"]);
  });

  it("filters archived and missing states independently", () => {
    const archived = conversation({
      id: "archived-id",
      codex: { ...conversation().codex, archived: true },
    });
    const missing = conversation({
      id: "missing-id",
      codex: { ...conversation().codex, archived: false },
      derived: { ...conversation().derived, missing: true },
    });

    expect(filterConversations([archived, missing], {
      ...DEFAULT_CONVERSATION_FILTERS,
      archived: "archived",
    }).map(({ id }) => id)).toEqual(["archived-id"]);
    expect(filterConversations([archived, missing], {
      ...DEFAULT_CONVERSATION_FILTERS,
      missing: "missing",
    }).map(({ id }) => id)).toEqual(["missing-id"]);
  });

  it("does not classify missing Conversations as linked or unlinked filter results", () => {
    const linked = conversation({
      id: "linked-id",
      derived: { ...conversation().derived, unlinked: false },
    });
    const missing = conversation({
      id: "missing-id",
      derived: { ...conversation().derived, missing: true, unlinked: false },
    });
    const unlinked = conversation({ id: "unlinked-id" });

    expect(filterConversations([linked, missing, unlinked], {
      ...DEFAULT_CONVERSATION_FILTERS,
      unlinked: "linked",
    }).map(({ id }) => id)).toEqual(["linked-id"]);
    expect(filterConversations([linked, missing, unlinked], {
      ...DEFAULT_CONVERSATION_FILTERS,
      unlinked: "unlinked",
    }).map(({ id }) => id)).toEqual(["unlinked-id"]);
  });

  it("can explicitly filter hidden Conversations without changing their identity", () => {
    const hidden = conversation({
      id: "hidden-id",
      overlay: { ...conversation().overlay, hidden: true },
    });
    const visible = conversation({ id: "visible-id" });

    expect(filterConversations([hidden, visible], {
      ...DEFAULT_CONVERSATION_FILTERS,
      hidden: "hidden",
    }).map(({ id }) => id)).toEqual(["hidden-id"]);
  });

  it("sorts by createdAt or updatedAt without mutating the source collection", () => {
    const older = conversation({
      id: "older-id",
      codex: {
        ...conversation().codex,
        createdAt: "2024-01-01T00:00:00Z",
        updatedAt: "2024-01-02T00:00:00Z",
      },
    });
    const newer = conversation({
      id: "newer-id",
      codex: {
        ...conversation().codex,
        createdAt: "2024-01-03T00:00:00Z",
        updatedAt: "2024-01-04T00:00:00Z",
      },
    });
    const sourceOrder = [older, newer];

    expect(filterConversations(sourceOrder, {
      ...DEFAULT_CONVERSATION_FILTERS,
      sortBy: "createdAt",
    }).map(({ id }) => id)).toEqual(["newer-id", "older-id"]);
    expect(filterConversations(sourceOrder, {
      ...DEFAULT_CONVERSATION_FILTERS,
      sortBy: "updatedAt",
    }).map(({ id }) => id)).toEqual(["newer-id", "older-id"]);
    expect(sourceOrder.map(({ id }) => id)).toEqual(["older-id", "newer-id"]);
  });
});
