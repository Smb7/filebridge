import { childPath, normalizeRemotePath } from "./browser";

/** Short enough that a reload can restore the last edit, not every keystroke. */
export const DRAFT_DEBOUNCE_MS = 300;

export type PaneSide = "left" | "right";

export type EditorPeer = {
  sessionId: string | null;
  directory: string;
  name: string | null;
  endpoint: string | null;
};

export type SavePlan =
  | {
      action: "write";
      sessionId: string;
      path: string;
      expectedSha256?: string;
    }
  | { action: "blocked"; message: string };

const SAVE_BLOCKED = "Save is disabled until this server is connected again.";

export function fileName(path: string): string {
  if (path === "" || path === "/") {
    return "";
  }
  const slash = path.lastIndexOf("/");
  return slash < 0 ? path : path.slice(slash + 1);
}

export function canSave(connected: boolean, busy: boolean): boolean {
  return connected && !busy;
}

export function canSaveOther(
  peerSessionId: string | null | undefined,
  busy: boolean,
): boolean {
  return !busy && typeof peerSessionId === "string" && peerSessionId !== "";
}

/**
 * A normal save sends the hash we loaded. A mismatch must not write.
 * `force` is only the confirmed replace, and it omits the hash.
 */
export function planSave(input: {
  connected: boolean;
  sessionId: string | null;
  path: string;
  baselineSha256: string | null;
  force: boolean;
}): SavePlan {
  if (!input.connected || !input.sessionId) {
    return { action: "blocked", message: SAVE_BLOCKED };
  }
  const path = normalizeRemotePath(input.path);
  if (!path.ok || path.path === "/") {
    return {
      action: "blocked",
      message: path.ok ? "That path is not a file." : path.message,
    };
  }
  if (input.force) {
    return { action: "write", sessionId: input.sessionId, path: path.path };
  }
  if (!input.baselineSha256) {
    return {
      action: "blocked",
      message: "The file has not finished loading.",
    };
  }
  return {
    action: "write",
    sessionId: input.sessionId,
    path: path.path,
    expectedSha256: input.baselineSha256,
  };
}

export function planRemoteWrite(input: {
  sessionId: string | null;
  path: string;
}):
  | { ok: true; sessionId: string; path: string }
  | { ok: false; message: string } {
  if (!input.sessionId) {
    return { ok: false, message: SAVE_BLOCKED };
  }
  const path = normalizeRemotePath(input.path);
  if (!path.ok) {
    return { ok: false, message: path.message };
  }
  if (path.path === "/") {
    return { ok: false, message: "That path is not a file." };
  }
  return { ok: true, sessionId: input.sessionId, path: path.path };
}

export function sameFile(
  left: { sessionId: string; path: string },
  right: { sessionId: string; path: string },
): boolean {
  return left.sessionId === right.sessionId && left.path === right.path;
}

/**
 * Save-to-other writes the buffer to the other pane. It does not download
 * the file, and it is not a second save of the open path.
 */
export function otherSaveTarget(input: {
  editorSessionId: string;
  editorPath: string;
  peer: EditorPeer | null;
}): { sessionId: string; path: string } | null {
  if (!input.peer?.sessionId || !input.peer.directory.startsWith("/")) {
    return null;
  }
  const name = fileName(input.editorPath);
  const path = childPath(input.peer.directory, name);
  if (!path) {
    return null;
  }
  if (
    sameFile(
      { sessionId: input.editorSessionId, path: input.editorPath },
      { sessionId: input.peer.sessionId, path },
    )
  ) {
    return null;
  }
  return { sessionId: input.peer.sessionId, path };
}

export function otherSaveLabel(name: string | null | undefined): string {
  if (!name) {
    return "Save to other pane";
  }
  return `Save to ${name}`;
}

export function closeChoice(dirty: boolean): "prompt" | "close" {
  return dirty ? "prompt" : "close";
}

export function quitChoice(dirtyCount: number): "prompt" | "quit" {
  return dirtyCount > 0 ? "prompt" : "quit";
}

/** A reload restores the mirrored draft instead of discarding unsaved edits. */
export function initialBuffer(state: {
  draft: string | null;
  dirty: boolean;
  baselineSha256: string | null;
}): {
  content: string;
  dirty: boolean;
  baselineSha256: string | null;
  readRemote: boolean;
} {
  if (state.draft !== null) {
    return {
      content: state.draft,
      dirty: state.dirty,
      baselineSha256: state.baselineSha256,
      readRemote: false,
    };
  }
  return {
    content: "",
    dirty: false,
    baselineSha256: state.baselineSha256,
    readRemote: true,
  };
}

export function applyRemoteRead(
  content: string,
  sha256: string,
): {
  content: string;
  dirty: boolean;
  baselineSha256: string;
  readRemote: false;
} {
  return {
    content,
    dirty: false,
    baselineSha256: sha256,
    readRemote: false,
  };
}
