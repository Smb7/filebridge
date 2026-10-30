import {
  childPath,
  parentPath,
  type DirEntry,
  type EntryKind,
} from "./browser";

export type NameResult =
  { ok: true; name: string } | { ok: false; message: string };

export type MkdirPlan =
  { ok: true; path: string; name: string } | { ok: false; message: string };

export type RenamePlan =
  | { ok: true; unchanged: true }
  | { ok: true; unchanged: false; from: string; to: string; name: string }
  | { ok: false; message: string };

export type DeletePrompt =
  { ok: true; path: string; message: string } | { ok: false; message: string };

const MAX_NAME = 255;

function isControl(char: string): boolean {
  const code = char.charCodeAt(0);
  return code <= 0x1f || (code >= 0x7f && code <= 0x9f);
}

/** A single remote path segment. `/` is a separator, not part of a name. */
export function entryName(input: string): NameResult {
  const name = input.trim();
  if (name === "" || name === "." || name === ".." || name.includes("/")) {
    return { ok: false, message: "Enter a file name." };
  }
  if (name.length > MAX_NAME || [...name].some(isControl)) {
    return { ok: false, message: "That name is not valid." };
  }
  return { ok: true, name };
}

export function planMkdir(directory: string, rawName: string): MkdirPlan {
  const named = entryName(rawName);
  if (!named.ok) {
    return named;
  }
  const path = childPath(directory, named.name);
  if (!path || path === "/") {
    return { ok: false, message: "That name is not valid." };
  }
  return { ok: true, path, name: named.name };
}

export function planRename(
  entry: { path: string; name: string },
  rawName: string,
): RenamePlan {
  if (entry.path === "/" || entry.name === "." || entry.name === "..") {
    return { ok: false, message: "That path cannot be changed." };
  }
  const named = entryName(rawName);
  if (!named.ok) {
    return named;
  }
  if (named.name === entry.name) {
    return { ok: true, unchanged: true };
  }
  const parent = parentPath(entry.path);
  if (!parent) {
    return { ok: false, message: "That path cannot be changed." };
  }
  const to = childPath(parent, named.name);
  if (!to || to === entry.path || to.startsWith(`${entry.path}/`)) {
    return { ok: false, message: "That path cannot be changed." };
  }
  return { ok: true, unchanged: false, from: entry.path, to, name: named.name };
}

export function deletePrompt(entry: {
  path: string;
  name: string;
  kind: EntryKind;
}): DeletePrompt {
  if (
    entry.path === "/" ||
    entry.name === "" ||
    entry.name === "." ||
    entry.name === ".."
  ) {
    return { ok: false, message: "That path cannot be changed." };
  }
  if (entry.kind === "dir") {
    return {
      ok: true,
      path: entry.path,
      message: `Delete folder ${entry.name}? Only an empty folder is removed.`,
    };
  }
  if (entry.kind === "symlink") {
    return {
      ok: true,
      path: entry.path,
      message: `Delete link ${entry.name}? This removes the link, not what it points to.`,
    };
  }
  return {
    ok: true,
    path: entry.path,
    message: `Delete ${entry.name}? This cannot be undone.`,
  };
}

/** The delete command runs only after the confirm still matches the selection. */
export function confirmedDelete(
  pendingPath: string | null,
  selectedPath: string | null,
): string | null {
  if (!pendingPath || pendingPath !== selectedPath || pendingPath === "/") {
    return null;
  }
  return pendingPath;
}

export function neighborName(
  entries: Array<Pick<DirEntry, "name">>,
  name: string,
): string | null {
  const index = entries.findIndex((entry) => entry.name === name);
  if (index < 0) {
    return null;
  }
  return entries[index + 1]?.name ?? entries[index - 1]?.name ?? null;
}
