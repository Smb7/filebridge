import { invoke } from "@tauri-apps/api/core";
import type { DirEntry } from "./browser";

export type { DirEntry, EntryKind } from "./browser";

export type AppError = {
  code: string;
  message: string;
  prompt?: HostKeyPrompt;
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

export type HostKeyDecision = "trusted" | "unknown" | "changed";

export type TrustedHost = {
  id: string;
  host: string;
  port: number;
  keyType: string;
  fingerprint: string;
  trustedAt: string;
};

export type HostKeyPrompt = {
  pendingId: string;
  host: string;
  port: number;
  keyType: string;
  fingerprint: string;
  storedFingerprint: string | null;
  decision: HostKeyDecision;
};

export function hostKeyPromptCopy(decision: HostKeyDecision): {
  title: string;
  confirm: string;
} {
  if (decision === "changed") {
    return {
      title: "Host key changed",
      confirm: "Replace trusted key",
    };
  }
  return {
    title: "Unknown host key",
    confirm: "Trust this key",
  };
}

export function hostkeyList(): Promise<TrustedHost[]> {
  return command("hostkey_list");
}

export function hostkeyForget(id: string): Promise<void> {
  return command("hostkey_forget", { id });
}

export function hostkeyConfirm(
  pendingId: string,
  replace: boolean,
): Promise<TrustedHost> {
  return command("hostkey_confirm", { pendingId, replace });
}

export type LiveSession = {
  sessionId: string;
  connectionId: string;
  name: string;
  username: string;
  host: string;
  port: number;
};

export function sessionForConnection(
  sessions: LiveSession[],
  connectionId: string,
): LiveSession | undefined {
  return sessions.find((session) => session.connectionId === connectionId);
}

export function isHostKeyStop(code: string): boolean {
  return code === "host_key_unknown" || code === "host_key_changed";
}

export type BoundHostPrompt = HostKeyPrompt & {
  connectionId: string;
};

/** Keep the confirm action on the connection that was connecting, not whichever row is selected later. */
export function bindHostPrompt(
  connectionId: string | null,
  prompt: HostKeyPrompt | null | undefined,
): BoundHostPrompt | null {
  if (!connectionId || !prompt?.fingerprint) {
    return null;
  }
  return { ...prompt, connectionId };
}

export function promptFromConnectError(
  connectionId: string | null,
  error: unknown,
): BoundHostPrompt | null {
  if (!isAppError(error) || !isHostKeyStop(error.code)) {
    return null;
  }
  return bindHostPrompt(connectionId, error.prompt);
}

export function sessionConnect(
  connectionId: string,
): Promise<{ sessionId: string }> {
  return command("session_connect", { connectionId });
}

export function sessionTrustHost(
  connectionId: string,
  fingerprint: string,
): Promise<void> {
  return command("session_trust_host", { connectionId, fingerprint });
}

export function sessionDisconnect(sessionId: string): Promise<void> {
  return command("session_disconnect", { sessionId });
}

export function sessionList(): Promise<LiveSession[]> {
  return command("session_list");
}

export function fsList(sessionId: string, path: string): Promise<DirEntry[]> {
  return command("fs_list", { sessionId, path });
}

export function fsStat(sessionId: string, path: string): Promise<DirEntry> {
  return command("fs_stat", { sessionId, path });
}

export function fsMkdir(sessionId: string, path: string): Promise<void> {
  return command("fs_mkdir", { sessionId, path });
}

export function fsRename(
  sessionId: string,
  from: string,
  to: string,
): Promise<void> {
  return command("fs_rename", { sessionId, from, to });
}

export function fsDelete(sessionId: string, path: string): Promise<void> {
  return command("fs_delete", { sessionId, path });
}

export type TransferStarted = {
  transferId: string;
};

export type TransferProgress = {
  id: string;
  bytes: number;
  total: number;
  state: "running" | "done" | "cancelled" | "failed";
  code?: string;
  message?: string;
};

export function transferStart(input: {
  fromSession: string;
  fromPath: string;
  toSession: string;
  toPath: string;
  overwrite: boolean;
}): Promise<TransferStarted> {
  return command("transfer_start", { ...input });
}

export function transferCancel(transferId: string): Promise<void> {
  return command("transfer_cancel", { transferId });
}

export type TextFile = {
  content: string;
  sha256: string;
  size: number;
};

export type WrittenFile = {
  sha256: string;
  size: number;
};

export type EditorOpened = {
  editorId: string;
  focused: boolean;
};

export type EditorView = {
  editorId: string;
  sessionId: string;
  connectionId: string;
  path: string;
  name: string;
  username: string;
  host: string;
  port: number;
  title: string;
  connected: boolean;
  dirty: boolean;
  draft: string | null;
  baselineSha256: string | null;
};

export type SaveNoted = {
  retargeted: boolean;
  path: string;
};

export type QuitPrompt = {
  count: number;
  paths: string[];
};

export function fsReadText(sessionId: string, path: string): Promise<TextFile> {
  return command("fs_read_text", { sessionId, path });
}

export function fsWrite(
  sessionId: string,
  path: string,
  content: string,
  options: { overwrite: boolean; expectedSha256?: string },
): Promise<WrittenFile> {
  return command("fs_write", { sessionId, path, content, options });
}

export function editorOpen(
  sessionId: string,
  path: string,
): Promise<EditorOpened> {
  return command("editor_open", { sessionId, path });
}

export function editorState(includeDraft: boolean): Promise<EditorView> {
  return command("editor_state", { includeDraft });
}

export function editorDraft(
  content: string | null,
  dirty: boolean,
): Promise<void> {
  return command("editor_draft", { content, dirty });
}

export function editorNoteSaved(
  sha256: string,
  path: string,
): Promise<SaveNoted> {
  return command("editor_note_saved", { sha256, path });
}

export function editorAllowClose(): Promise<boolean> {
  return command("editor_allow_close");
}

export function editorDiscard(): Promise<void> {
  return command("editor_discard");
}

export function editorCancelClose(): Promise<void> {
  return command("editor_cancel_close");
}

export function editorQuit(): Promise<void> {
  return command("editor_quit");
}

export function editorCancelQuit(): Promise<void> {
  return command("editor_cancel_quit");
}
