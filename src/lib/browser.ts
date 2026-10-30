export type EntryKind = "dir" | "file" | "symlink" | "other";

export type DirEntry = {
  name: string;
  path: string;
  kind: EntryKind;
  size: number;
  modified: number | null;
};

export type BrowserState = {
  path: string;
  draft: string | null;
  entries: DirEntry[];
  selectedName: string | null;
  generation: number;
  listing: boolean;
  error: string | null;
  pathError: string | null;
};

export type PathResult =
  { ok: true; path: string } | { ok: false; message: string };

export type ListAction =
  | { type: "move"; delta: -1 | 1 }
  | { type: "open" }
  | { type: "parent" }
  | { type: "refresh" }
  | { type: "focus-other" }
  | { type: "rename" }
  | { type: "delete" }
  | { type: "ignore" };

const MAX_REMOTE_PATH = 4096;

export const ROW_HEIGHT = 28;

export const FILE_OPEN_LATER =
  "Opening files in an editor is not available yet.";

export function createBrowser(path = "/"): BrowserState {
  return {
    path,
    draft: null,
    entries: [],
    selectedName: null,
    generation: 0,
    listing: false,
    error: null,
    pathError: null,
  };
}

function isControl(char: string): boolean {
  const code = char.charCodeAt(0);
  return code <= 0x1f || (code >= 0x7f && code <= 0x9f);
}

/** Lexical absolute path. `..` is legal. A relative path is not. */
export function normalizeRemotePath(input: string): PathResult {
  const trimmed = input.trim();
  if (trimmed === "" || !trimmed.startsWith("/")) {
    return { ok: false, message: "Remote path must be absolute." };
  }
  if (trimmed.length > MAX_REMOTE_PATH) {
    return { ok: false, message: "Remote path is too long." };
  }
  if ([...trimmed].some(isControl)) {
    return { ok: false, message: "Remote path is not valid." };
  }
  const parts: string[] = [];
  for (const segment of trimmed.split("/")) {
    if (segment === "" || segment === ".") {
      continue;
    }
    if (segment === "..") {
      parts.pop();
      continue;
    }
    parts.push(segment);
  }
  return { ok: true, path: parts.length === 0 ? "/" : `/${parts.join("/")}` };
}

export function startingPath(defaultPath: string | null | undefined): string {
  if (!defaultPath) {
    return "/";
  }
  const normalized = normalizeRemotePath(defaultPath);
  return normalized.ok ? normalized.path : "/";
}

export function parentPath(path: string): string | null {
  const normalized = normalizeRemotePath(path);
  if (!normalized.ok || normalized.path === "/") {
    return null;
  }
  const slash = normalized.path.lastIndexOf("/");
  return slash <= 0 ? "/" : normalized.path.slice(0, slash);
}

export function childPath(dir: string, name: string): string | null {
  if (!isListableName(name)) {
    return null;
  }
  const parent = normalizeRemotePath(dir);
  if (!parent.ok) {
    return null;
  }
  return parent.path === "/" ? `/${name}` : `${parent.path}/${name}`;
}

function isListableName(name: string): boolean {
  if (name === "" || name === "." || name === ".." || name.includes("/")) {
    return false;
  }
  return ![...name].some(isControl);
}

export function serverLabel(server: {
  username: string;
  host: string;
  port: number;
}): string {
  return `${server.username}@${server.host}:${server.port}`;
}

export function pathFieldEnabled(
  sessionId: string | null | undefined,
): boolean {
  return typeof sessionId === "string" && sessionId !== "";
}

/** Paste into the path field is text. List paste belongs to transfer, not here. */
export function pasteAction(
  focus: "path" | "list" | "other",
): "text" | "defer" {
  return focus === "path" ? "text" : "defer";
}

export function pathPointer(
  alreadyFocused: boolean,
): "select-all" | "place-cursor" {
  return alreadyFocused ? "place-cursor" : "select-all";
}

export function applyPathCommit(
  state: BrowserState,
  draft: string,
): { state: BrowserState; list: string | null } {
  const normalized = normalizeRemotePath(draft);
  if (!normalized.ok) {
    return {
      state: { ...state, pathError: normalized.message },
      list: null,
    };
  }
  return {
    state: { ...state, draft: null, pathError: null },
    list: normalized.path,
  };
}

export function escapePath(state: BrowserState): BrowserState {
  return { ...state, draft: null, pathError: null };
}

export function attachSession(state: BrowserState, path: string): BrowserState {
  return {
    path,
    draft: null,
    entries: [],
    selectedName: null,
    generation: state.generation + 1,
    listing: false,
    error: null,
    pathError: null,
  };
}

export function clearBrowser(state: BrowserState): BrowserState {
  return attachSession(state, "/");
}

/** Drop a live listing without forgetting the pane directory. */
export function releaseSession(
  state: BrowserState,
  path: string,
): BrowserState {
  const kept = path.startsWith("/")
    ? path
    : state.path.startsWith("/")
      ? state.path
      : "/";
  return {
    path: kept,
    draft: null,
    entries: [],
    selectedName: null,
    generation: state.generation + 1,
    listing: false,
    error: null,
    pathError: null,
  };
}

export function beginListing(state: BrowserState): {
  state: BrowserState;
  generation: number;
} {
  const generation = state.generation + 1;
  return {
    generation,
    state: {
      ...state,
      generation,
      listing: true,
      error: null,
      pathError: null,
    },
  };
}

export function acceptListing(
  state: BrowserState,
  generation: number,
  path: string,
  entries: DirEntry[],
): BrowserState {
  if (generation !== state.generation) {
    return state;
  }
  const selectedName = entries.some(
    (entry) => entry.name === state.selectedName,
  )
    ? state.selectedName
    : null;
  return {
    ...state,
    path,
    entries,
    selectedName,
    listing: false,
    error: null,
    draft: state.draft !== null && path === state.path ? state.draft : null,
  };
}

export function rejectListing(
  state: BrowserState,
  generation: number,
  message: string,
): BrowserState {
  if (generation !== state.generation) {
    return state;
  }
  return { ...state, listing: false, error: message };
}

export function moveSelection(
  entries: DirEntry[],
  selected: string | null,
  delta: -1 | 1,
): string | null {
  if (entries.length === 0) {
    return null;
  }
  const index = entries.findIndex((entry) => entry.name === selected);
  if (index < 0) {
    return delta < 0 ? entries[entries.length - 1].name : entries[0].name;
  }
  const next = Math.min(entries.length - 1, Math.max(0, index + delta));
  return entries[next].name;
}

export function openAction(kind: EntryKind): "list" | "stat" | "file" {
  if (kind === "dir") {
    return "list";
  }
  if (kind === "file") {
    return "file";
  }
  return "stat";
}

export function listKeyAction(
  key: string,
  modifiers: {
    alt?: boolean;
    ctrl?: boolean;
    meta?: boolean;
    shift?: boolean;
  } = {},
): ListAction {
  if (modifiers.alt || modifiers.ctrl || modifiers.meta) {
    return { type: "ignore" };
  }
  switch (key) {
    case "ArrowUp":
      return { type: "move", delta: -1 };
    case "ArrowDown":
      return { type: "move", delta: 1 };
    case "Enter":
      return { type: "open" };
    case "Backspace":
      return { type: "parent" };
    case "F5":
      return { type: "refresh" };
    case "F2":
      return modifiers.shift ? { type: "ignore" } : { type: "rename" };
    case "Delete":
      return modifiers.shift ? { type: "ignore" } : { type: "delete" };
    case "Tab":
      return modifiers.shift ? { type: "ignore" } : { type: "focus-other" };
    default:
      return { type: "ignore" };
  }
}

export function visibleRange(input: {
  scrollTop: number;
  viewportHeight: number;
  count: number;
  itemHeight: number;
  overscan?: number;
}): { start: number; end: number; offset: number; totalHeight: number } {
  const itemHeight = input.itemHeight;
  if (input.count <= 0 || itemHeight <= 0) {
    return { start: 0, end: 0, offset: 0, totalHeight: 0 };
  }
  const overscan = input.overscan ?? 4;
  const scrollTop = Math.max(0, input.scrollTop);
  const rawStart = Math.floor(scrollTop / itemHeight);
  const visibleCount = Math.max(
    1,
    Math.ceil(Math.max(0, input.viewportHeight) / itemHeight),
  );
  const start = Math.max(0, rawStart - overscan);
  const end = Math.min(input.count, rawStart + visibleCount + overscan);
  return {
    start,
    end,
    offset: start * itemHeight,
    totalHeight: input.count * itemHeight,
  };
}

export function scrollForIndex(
  scrollTop: number,
  viewportHeight: number,
  index: number,
  itemHeight: number,
): number {
  const top = Math.max(0, index) * itemHeight;
  const bottom = top + itemHeight;
  if (top < scrollTop) {
    return top;
  }
  if (bottom > scrollTop + viewportHeight) {
    return Math.max(0, bottom - viewportHeight);
  }
  return scrollTop;
}

export function formatSize(size: number): string {
  if (!Number.isFinite(size) || size < 0) {
    return "";
  }
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = size;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  if (unit === 0) {
    return `${Math.trunc(value)} B`;
  }
  const digits = value >= 10 ? 0 : 1;
  return `${value.toFixed(digits)} ${units[unit]}`;
}

export function entryMeta(entry: { kind: EntryKind; size: number }): string {
  if (entry.kind === "dir") {
    return "Folder";
  }
  if (entry.kind === "symlink") {
    return "Link";
  }
  if (entry.kind === "other") {
    return "Other";
  }
  return formatSize(entry.size);
}
