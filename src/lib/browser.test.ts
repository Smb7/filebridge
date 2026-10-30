import { describe, expect, it } from "vitest";
import {
  FILE_OPEN_LATER,
  acceptListing,
  applyPathCommit,
  attachSession,
  beginListing,
  childPath,
  createBrowser,
  entryMeta,
  escapePath,
  formatSize,
  listKeyAction,
  moveSelection,
  normalizeRemotePath,
  openAction,
  parentPath,
  pasteAction,
  pathFieldEnabled,
  pathPointer,
  rejectListing,
  scrollForIndex,
  serverLabel,
  startingPath,
  visibleRange,
  type DirEntry,
} from "./browser";

function entry(name: string, kind: DirEntry["kind"] = "file"): DirEntry {
  return {
    name,
    path: `/etc/${name}`,
    kind,
    size: 12,
    modified: null,
  };
}

describe("remote paths", () => {
  it("normalizes absolute paths and rejects a relative or unsafe path", () => {
    expect(normalizeRemotePath(" /etc/telegraf/ ")).toEqual({
      ok: true,
      path: "/etc/telegraf",
    });
    expect(normalizeRemotePath("/etc/foo/../bar")).toEqual({
      ok: true,
      path: "/etc/bar",
    });
    expect(normalizeRemotePath("/etc//./telegraf/")).toEqual({
      ok: true,
      path: "/etc/telegraf",
    });
    expect(normalizeRemotePath("/")).toEqual({ ok: true, path: "/" });
    expect(normalizeRemotePath("etc/telegraf")).toEqual({
      ok: false,
      message: "Remote path must be absolute.",
    });
    const secret = normalizeRemotePath("/etc/hunter2-secret\0conf");
    expect(secret.ok).toBe(false);
    if (!secret.ok) {
      expect(secret.message).toBe("Remote path is not valid.");
      expect(secret.message).not.toContain("hunter2-secret");
    }
  });

  it("starts at the default path only when that path is absolute", () => {
    expect(startingPath("/etc/telegraf")).toBe("/etc/telegraf");
    expect(startingPath("etc/telegraf")).toBe("/");
    expect(startingPath(null)).toBe("/");
    expect(startingPath("  ")).toBe("/");
  });

  it("joins a child without letting the name escape the directory", () => {
    expect(childPath("/etc/telegraf", "cpu.conf")).toBe(
      "/etc/telegraf/cpu.conf",
    );
    expect(childPath("/", "etc")).toBe("/etc");
    expect(childPath("/etc", "..")).toBeNull();
    expect(childPath("/etc", ".")).toBeNull();
    expect(childPath("/etc", "a/b")).toBeNull();
    expect(parentPath("/etc/telegraf")).toBe("/etc");
    expect(parentPath("/etc")).toBe("/");
    expect(parentPath("/")).toBeNull();
  });
});

describe("path field", () => {
  it("is disabled until the pane has a session and is not the server switcher", () => {
    expect(pathFieldEnabled(null)).toBe(false);
    expect(pathFieldEnabled("")).toBe(false);
    expect(pathFieldEnabled("ses-1")).toBe(true);
    expect(
      serverLabel({ username: "ubuntu", host: "10.0.0.8", port: 22 }),
    ).toBe("ubuntu@10.0.0.8:22");
  });

  it("selects the path on the first click and places a cursor on the second", () => {
    expect(pathPointer(false)).toBe("select-all");
    expect(pathPointer(true)).toBe("place-cursor");
  });

  it("does not change directory when the typed path is bad", () => {
    const state = {
      ...createBrowser("/etc/telegraf"),
      entries: [entry("cpu.conf")],
    };
    const rejected = applyPathCommit(state, "relative.conf");
    expect(rejected.list).toBeNull();
    expect(rejected.state.path).toBe("/etc/telegraf");
    expect(rejected.state.entries).toEqual(state.entries);
    expect(rejected.state.pathError).toBe("Remote path must be absolute.");

    const restored = escapePath({ ...rejected.state, draft: "nope" });
    expect(restored.draft).toBeNull();
    expect(restored.path).toBe("/etc/telegraf");
    expect(restored.pathError).toBeNull();
  });

  it("lists a typed absolute path without changing the server", () => {
    const state = createBrowser("/");
    const committed = applyPathCommit(state, "/etc/foo/../telegraf");
    expect(committed.list).toBe("/etc/telegraf");
    expect(committed.state.path).toBe("/");
    expect(committed.state.pathError).toBeNull();
    expect("connectionId" in committed.state).toBe(false);
  });

  it("pastes text into the path field instead of starting a transfer", () => {
    expect(pasteAction("path")).toBe("text");
    expect(pasteAction("list")).toBe("defer");
  });
});

describe("stale listings", () => {
  it("drops an old response and keeps the previous listing until the new one returns", () => {
    let state = {
      ...createBrowser("/etc"),
      entries: [entry("old.conf")],
    };
    const first = beginListing(state);
    expect(first.state.entries).toEqual(state.entries);
    expect(first.state.path).toBe("/etc");
    expect(first.state.listing).toBe(true);

    const second = beginListing(first.state);
    const dropped = acceptListing(second.state, first.generation, "/var", [
      entry("stale.conf"),
    ]);
    expect(dropped.path).toBe("/etc");
    expect(dropped.entries).toEqual([entry("old.conf")]);
    expect(dropped.listing).toBe(true);

    const droppedError = rejectListing(second.state, first.generation, "nope");
    expect(droppedError.error).toBeNull();
    expect(droppedError.listing).toBe(true);

    const applied = acceptListing(second.state, second.generation, "/tmp", [
      entry("fresh.conf"),
    ]);
    expect(applied.path).toBe("/tmp");
    expect(applied.entries).toEqual([entry("fresh.conf")]);
    expect(applied.listing).toBe(false);

    const failed = rejectListing(
      second.state,
      second.generation,
      "Permission denied.",
    );
    expect(failed.path).toBe("/etc");
    expect(failed.entries).toEqual([entry("old.conf")]);
    expect(failed.error).toBe("Permission denied.");
    expect(failed.listing).toBe(false);
  });

  it("clears another server's files when the session changes", () => {
    const state = {
      ...createBrowser("/etc"),
      entries: [entry("cpu.conf")],
      generation: 4,
    };
    const next = attachSession(state, "/var");
    expect(next.path).toBe("/var");
    expect(next.entries).toEqual([]);
    expect(next.generation).toBe(5);
  });
});

describe("file list keyboard and window", () => {
  const entries = [
    entry("conf.d", "dir"),
    entry("cpu.conf"),
    entry("mem.conf"),
  ];

  it("maps file-list keys and keeps copy and modified keys alone", () => {
    expect(listKeyAction("ArrowDown")).toEqual({ type: "move", delta: 1 });
    expect(listKeyAction("ArrowUp")).toEqual({ type: "move", delta: -1 });
    expect(listKeyAction("Enter")).toEqual({ type: "open" });
    expect(listKeyAction("Backspace")).toEqual({ type: "parent" });
    expect(listKeyAction("F5")).toEqual({ type: "refresh" });
    expect(listKeyAction("F2")).toEqual({ type: "rename" });
    expect(listKeyAction("Delete")).toEqual({ type: "delete" });
    expect(listKeyAction("Tab")).toEqual({ type: "focus-other" });
    expect(listKeyAction("Tab", { shift: true })).toEqual({ type: "ignore" });
    expect(listKeyAction("Delete", { shift: true })).toEqual({
      type: "ignore",
    });
    expect(listKeyAction("F2", { ctrl: true })).toEqual({ type: "ignore" });
    expect(listKeyAction("c", { ctrl: true })).toEqual({ type: "ignore" });
    expect(listKeyAction("F5", { ctrl: true })).toEqual({ type: "ignore" });
  });

  it("moves a single selection and does not open a file as a download", () => {
    expect(moveSelection(entries, null, 1)).toBe("conf.d");
    expect(moveSelection(entries, "cpu.conf", 1)).toBe("mem.conf");
    expect(moveSelection(entries, "mem.conf", 1)).toBe("mem.conf");
    expect(moveSelection(entries, "conf.d", -1)).toBe("conf.d");
    expect(openAction("dir")).toBe("list");
    expect(openAction("symlink")).toBe("stat");
    expect(openAction("file")).toBe("file");
    expect(FILE_OPEN_LATER).not.toContain("sudo");
    expect(entryMeta(entries[0])).toBe("Folder");
    expect(formatSize(1536)).toBe("1.5 KB");
  });

  it("windows a large directory instead of a node per file", () => {
    const range = visibleRange({
      scrollTop: 0,
      viewportHeight: 280,
      count: 10_000,
      itemHeight: 28,
      overscan: 4,
    });
    expect(range.end - range.start).toBeLessThan(30);
    expect(range.totalHeight).toBe(280_000);
    expect(range.end).toBeLessThan(10_000);

    const scrolled = visibleRange({
      scrollTop: 2800,
      viewportHeight: 280,
      count: 10_000,
      itemHeight: 28,
      overscan: 0,
    });
    expect(scrolled.start).toBe(100);
    expect(scrolled.offset).toBe(2800);
    expect(scrollForIndex(0, 280, 20, 28)).toBe(20 * 28 + 28 - 280);
    expect(scrollForIndex(100, 280, 4, 28)).toBe(100);
  });
});
