export type AppError = {
  code: string;
  message: string;
};

export function isAppError(value: unknown): value is AppError {
  if (typeof value !== "object" || value === null) {
    return false;
  }

  const record = value as Record<string, unknown>;
  return typeof record.code === "string" && typeof record.message === "string";
}
