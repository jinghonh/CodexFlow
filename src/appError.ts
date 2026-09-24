type AppError = {
  code: string;
  message: string;
  retryable: boolean;
  cachePreserved: boolean;
  nextStep?: string;
};

export function formatAppError(error: unknown, fallback: string): string {
  if (typeof error !== "object" || error === null || !("message" in error) || typeof error.message !== "string") {
    return fallback;
  }
  const value = error as Partial<AppError>;
  if (typeof value.code !== "string") return value.message ?? fallback;
  const status = value.retryable ? "可重试。" : "需先修正原因。";
  const cache = value.cachePreserved ? "已有缓存保留。" : "请重新确认缓存状态。";
  const step = typeof value.nextStep === "string" && value.nextStep ? `下一步：${value.nextStep}` : "";
  return `${value.code}：${value.message} ${status}${cache}${step}`;
}
