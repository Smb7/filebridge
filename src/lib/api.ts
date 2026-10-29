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

export type VaultMode = "uninitialized" | "keychain" | "password";

export type VaultStatus = {
  mode: VaultMode;
  locked: boolean;
};

export type CredentialSecret = {
  password?: string | null;
  privateKeyPem?: string | null;
  passphrase?: string | null;
};

export function vaultStatus(): Promise<VaultStatus> {
  return command("vault_status");
}

export function vaultInitPassword(password: string): Promise<void> {
  return command("vault_init_password", { password });
}

export function vaultUnlock(password: string | null): Promise<void> {
  return command("vault_unlock", { password });
}

export function vaultSetCredential(
  connectionId: string,
  secret: CredentialSecret,
): Promise<void> {
  return command("vault_set_credential", { connectionId, secret });
}

export function vaultClearCredential(connectionId: string): Promise<void> {
  return command("vault_clear_credential", { connectionId });
}

export function vaultHasCredential(connectionId: string): Promise<boolean> {
  return command("vault_has_credential", { connectionId });
}

export type CredentialWrite =
  | { action: "refuse"; message: string }
  | { action: "clear" }
  | { action: "set" }
  | { action: "keep" };

export function planCredentialWrite(input: {
  locked: boolean;
  methodChanged: boolean;
  authMethod: AuthMethod;
  hasSecret: boolean;
}): CredentialWrite {
  if (input.locked && input.hasSecret) {
    return {
      action: "refuse",
      message: "Unlock the vault before saving a password or key.",
    };
  }
  if (input.locked && input.methodChanged) {
    return {
      action: "refuse",
      message: "Unlock the vault before changing authentication.",
    };
  }
  if (
    !input.locked &&
    (input.authMethod === "agent" || (input.methodChanged && !input.hasSecret))
  ) {
    return { action: "clear" };
  }
  if (input.hasSecret) {
    return { action: "set" };
  }
  return { action: "keep" };
}

export function secretFromFields(
  authMethod: AuthMethod,
  password: string,
  privateKeyPem: string,
  passphrase: string,
): CredentialSecret | null {
  const secret: CredentialSecret = {};
  if (authMethod === "password" && password.trim() !== "") {
    secret.password = password;
  }
  if (authMethod === "private_key" && privateKeyPem.trim() !== "") {
    secret.privateKeyPem = privateKeyPem;
  }
  if (
    (authMethod === "private_key" || authMethod === "key_file") &&
    passphrase.trim() !== ""
  ) {
    secret.passphrase = passphrase;
  }
  return secret.password || secret.privateKeyPem || secret.passphrase
    ? secret
    : null;
}
