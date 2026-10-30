import { childPath, type EntryKind } from "./browser";
import { entryName } from "./fileOps";

/** In-app drag type. The payload is a descriptor, never file bytes. */
export const TRANSFER_MIME = "application/x-filebridge";

export type FileRef = {
  sessionId: string;
  path: string;
  name: string;
};

export type CopyModifier = "ctrl" | "meta";

export type TransferKey = "copy" | "paste" | "ignore";

export type StartRequest = {
  fromSession: string;
  fromPath: string;
  toSession: string;
  toPath: string;
  overwrite: boolean;
  name: string;
};

export type TransferStateName = "running" | "done" | "cancelled" | "failed";

export type TransferEvent = {
  id: string;
  bytes: number;
  total: number;
  state: TransferStateName;
  code?: string;
  message?: string;
};

export type TrackedTransfer = {
  id: string;
  name: string;
  fromSession: string;
  toSession: string;
  toPath: string;
  bytes: number;
  total: number;
  state: TransferStateName;
  message: string | null;
  code: string | null;
};

export type TransferIntent =
  | { action: "none" }
  | { action: "refuse"; message: string }
  | {
      action: "rename";
      message: string;
      suggested: string;
      from: FileRef;
      toSession: string;
      directory: string;
    }
  | {
      action: "confirm-overwrite";
      message: string;
      from: FileRef;
      toSession: string;
      toPath: string;
      name: string;
    }
  | {
      action: "start";
      from: FileRef;
      toSession: string;
      toPath: string;
      name: string;
      overwrite: boolean;
    };

export function copyModifier(platform: string): CopyModifier {
  return /mac/i.test(platform) ? "meta" : "ctrl";
}

/**
 * File-list copy and paste. Ctrl+C on Windows and Linux, Cmd+C on macOS.
 * These are not a global accelerator. A path field does not use this.
 */
export function transferKeyAction(
  key: string,
  modifiers: {
    alt?: boolean;
    ctrl?: boolean;
    meta?: boolean;
    shift?: boolean;
  },
  modifier: CopyModifier,
): TransferKey {
  if (modifiers.alt || modifiers.shift) {
    return "ignore";
  }
  const pressed =
    modifier === "meta" ? Boolean(modifiers.meta) : Boolean(modifiers.ctrl);
  const other =
    modifier === "meta" ? Boolean(modifiers.ctrl) : Boolean(modifiers.meta);
  if (!pressed || other) {
    return "ignore";
  }
  const lower = key.toLowerCase();
  if (lower === "c") {
    return "copy";
  }
  if (lower === "v") {
    return "paste";
  }
  return "ignore";
}

/** Paste into a focused path field is text, not a file transfer. */
export function listOwnsShortcut(tagName: string): boolean {
  const tag = tagName.toLowerCase();
  return tag !== "input" && tag !== "textarea" && tag !== "select";
}

export function shortcutHint(modifier: CopyModifier): string {
  const key = modifier === "meta" ? "Cmd" : "Ctrl";
  return `${key}+C, ${key}+V`;
}

export function copyCandidate(
  entry: { kind: EntryKind; path: string; name: string } | null,
  sessionId: string | null,
): { ok: true; ref: FileRef } | { ok: false; message: string } {
  if (!sessionId) {
    return { ok: false, message: "Connect to copy a file." };
  }
  if (!entry) {
    return { ok: false, message: "Select a file to copy." };
  }
  if (entry.kind !== "file") {
    return { ok: false, message: "Only a file can be copied." };
  }
  if (
    !entry.path.startsWith("/") ||
    entry.name === "" ||
    entry.name.includes("/") ||
    entry.name.includes("\0")
  ) {
    return { ok: false, message: "That file cannot be copied." };
  }
  return {
    ok: true,
    ref: { sessionId, path: entry.path, name: entry.name },
  };
}

/** The in-app mark is a descriptor. It has no file bytes. */
export function rememberCopy(ref: FileRef): FileRef {
  return { sessionId: ref.sessionId, path: ref.path, name: ref.name };
}

export function markedMessage(name: string): string {
  return `Marked ${name} to copy.`;
}

export function suggestedCopyName(
  name: string,
  taken: readonly string[],
): string {
  const names = new Set(taken);
  const dot = name.lastIndexOf(".");
  const hasExt = dot > 0;
  const stem = hasExt ? name.slice(0, dot) : name;
  const ext = hasExt ? name.slice(dot) : "";
  const first = `${stem} copy${ext}`;
  if (!names.has(first)) {
    return first;
  }
  for (let index = 2; index < 1000; index += 1) {
    const candidate = `${stem} copy ${index}${ext}`;
    if (!names.has(candidate)) {
      return candidate;
    }
  }
  return first;
}

export function planTransfer(input: {
  source: FileRef | null;
  toSession: string | null;
  directory: string;
  entries: ReadonlyArray<{ name: string; kind: EntryKind }>;
  renamedTo?: string | null;
  overwriteConfirmed?: boolean;
}): TransferIntent {
  if (!input.source) {
    return { action: "none" };
  }
  if (!input.toSession) {
    return { action: "refuse", message: "Connect the destination server." };
  }
  if (!input.directory.startsWith("/")) {
    return { action: "refuse", message: "Remote path must be absolute." };
  }
  const named = entryName(input.renamedTo ?? input.source.name);
  if (!named.ok) {
    return { action: "refuse", message: named.message };
  }
  const toPath = childPath(input.directory, named.name);
  if (!toPath) {
    return { action: "refuse", message: "That name is not valid." };
  }
  const sameFile =
    input.source.sessionId === input.toSession && input.source.path === toPath;
  if (sameFile) {
    if (input.renamedTo) {
      return { action: "refuse", message: "Choose a different name." };
    }
    return {
      action: "rename",
      message: "That file is already here. Choose a new name.",
      suggested: suggestedCopyName(
        input.source.name,
        input.entries.map((entry) => entry.name),
      ),
      from: input.source,
      toSession: input.toSession,
      directory: input.directory,
    };
  }
  const existing = input.entries.find((entry) => entry.name === named.name);
  if (existing?.kind === "dir") {
    return { action: "refuse", message: "That name is a folder." };
  }
  if (existing && !input.overwriteConfirmed) {
    return {
      action: "confirm-overwrite",
      message: `Replace ${named.name}?`,
      from: input.source,
      toSession: input.toSession,
      toPath,
      name: named.name,
    };
  }
  return {
    action: "start",
    from: input.source,
    toSession: input.toSession,
    toPath,
    name: named.name,
    overwrite: Boolean(existing) || Boolean(input.overwriteConfirmed),
  };
}

export function dragData(ref: FileRef): {
  mime: string;
  descriptor: string;
  text: string;
} {
  return {
    mime: TRANSFER_MIME,
    descriptor: JSON.stringify({
      sessionId: ref.sessionId,
      path: ref.path,
      name: ref.name,
    }),
    text: ref.name,
  };
}

export function acceptsDrop(types: readonly string[]): boolean {
  return types.includes(TRANSFER_MIME);
}

export function parseDropPayload(
  raw: string | null | undefined,
): FileRef | null {
  if (!raw) {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) {
    return null;
  }
  const record = parsed as Record<string, unknown>;
  if (
    typeof record.content === "string" ||
    typeof record.bytes === "string" ||
    Array.isArray(record.data)
  ) {
    return null;
  }
  if (
    typeof record.sessionId !== "string" ||
    typeof record.path !== "string" ||
    typeof record.name !== "string" ||
    record.sessionId === "" ||
    !record.path.startsWith("/") ||
    record.name === "" ||
    record.name.includes("/") ||
    record.name.includes("\0")
  ) {
    return null;
  }
  return {
    sessionId: record.sessionId,
    path: record.path,
    name: record.name,
  };
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) {
    return "0 B";
  }
  if (bytes < 1024) {
    return `${Math.floor(bytes)} B`;
  }
  const kilobytes = bytes / 1024;
  if (kilobytes < 1024) {
    return `${kilobytes.toFixed(kilobytes < 10 ? 1 : 0)} KB`;
  }
  const megabytes = kilobytes / 1024;
  return `${megabytes.toFixed(megabytes < 10 ? 1 : 0)} MB`;
}

export function transferStatusText(
  job: Pick<
    TrackedTransfer,
    "name" | "bytes" | "total" | "state" | "message"
  > | null,
): { text: string; canCancel: boolean } | null {
  if (!job || job.state === "done") {
    return null;
  }
  if (job.state === "running") {
    return {
      text: `Copying ${job.name}… ${formatBytes(job.bytes)} / ${formatBytes(job.total)}`,
      canCancel: true,
    };
  }
  if (job.state === "cancelled") {
    return { text: "Copy cancelled.", canCancel: false };
  }
  return {
    text: job.message ?? "Could not copy that file.",
    canCancel: false,
  };
}

/** A copy can finish before the start call returns, so unmatched events wait. */
export function rememberUnmatched(
  pending: readonly TransferEvent[],
  event: TransferEvent,
): TransferEvent[] {
  return [...pending, event].slice(-20);
}

export function takeBuffered(
  pending: readonly TransferEvent[],
  id: string,
): { rest: TransferEvent[]; matched: TransferEvent[] } {
  return {
    rest: pending.filter((event) => event.id !== id),
    matched: pending.filter((event) => event.id === id),
  };
}

export function reduceTransfer(
  job: TrackedTransfer,
  event: TransferEvent,
): { job: TrackedTransfer; refresh: boolean; conflict: boolean } {
  if (event.id !== job.id) {
    return { job, refresh: false, conflict: false };
  }
  return {
    job: {
      ...job,
      bytes: event.bytes,
      total: event.total,
      state: event.state,
      message: event.message ?? job.message,
      code: event.code ?? job.code,
    },
    refresh: event.state === "done",
    conflict: event.state === "failed" && event.code === "already_exists",
  };
}
