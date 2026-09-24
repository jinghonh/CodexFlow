export function threadDisplayTitle(thread: { title: string | null; sourceKind: string }): string {
  const title = thread.title?.trim();
  if (title) return title;
  return thread.sourceKind === "subAgent" ? "未命名子代理会话" : "未命名会话";
}

export function threadPreviewExcerpt(preview: string): string {
  const characters = Array.from(preview.trim());
  return characters.length > 360 ? `${characters.slice(0, 360).join("")}…` : characters.join("");
}
