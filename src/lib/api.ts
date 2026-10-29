import { invoke } from "@tauri-apps/api/core";

export type AppError = {
  code: string;
  message: string;
};

export type AuthMethod = "password" | "private_key" | "key_file" | "agent";

export type Connection = {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  authMethod: AuthMethod;
  keyPath: string | null;
  defaultPath: string | null;
  notes: string | null;
  createdAt: string;
  updatedAt: string;
  lastUsedAt: string | null;
};

export type ConnectionDraft = {
  id?: string;
  name: string;
  host: string;
  port: number;
  username: string;
  authMethod: AuthMethod;
  keyPath?: string | null;
  defaultPath?: string | null;
  notes?: string | null;
};

export function isAppError(value: unknown): value is AppError {
  if (typeof value !== "object" || value === null) {
    return false;
  }

  const record = value as Record<string, unknown>;
  return typeof record.code === "string" && typeof record.message === "string";
}

export function keyPathForMethod(
  authMethod: AuthMethod,
  keyPath: string,
): string | null {
  if (authMethod !== "key_file") {
    return null;
  }
  const trimmed = keyPath.trim();
  return trimmed === "" ? null : trimmed;
}

export function blankToNull(value: string): string | null {
  const trimmed = value.trim();
  return trimmed === "" ? null : trimmed;
}

function toAppError(error: unknown): AppError {
  if (isAppError(error)) {
    return error;
  }
  return { code: "internal", message: "The request failed." };
}

async function command<T>(
  name: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw toAppError(error);
  }
}

export function connectionsList(): Promise<Connection[]> {
  return command("connections_list");
}

export function connectionsSave(draft: ConnectionDraft): Promise<Connection> {
  return command("connections_save", { draft });
}

export function connectionsDelete(id: string): Promise<void> {
  return command("connections_delete", { id });
}
