import { describe, expect, it } from "vitest";
import {
  TRANSFER_MIME,
  acceptsDrop,
  copyCandidate,
  copyModifier,
  dragData,
  formatBytes,
  listOwnsShortcut,
  markedMessage,
  parseDropPayload,
  planTransfer,
  reduceTransfer,
  rememberUnmatched,
  takeBuffered,
  rememberCopy,
  shortcutHint,
  suggestedCopyName,
  transferKeyAction,
  transferStatusText,
  type FileRef,
  type TrackedTransfer,
} from "./transfer";

const cpu: FileRef = {
  sessionId: "ses-prod",
  path: "/etc/telegraf/cpu.conf",
  name: "cpu.conf",
};

describe("file-list shortcuts", () => {
  it("uses Ctrl on Windows and Linux and Cmd on macOS, only without other modifiers", () => {
    expect(copyModifier("Win32")).toBe("ctrl");
    expect(copyModifier("Linux x86_64")).toBe("ctrl");
    expect(copyModifier("MacIntel")).toBe("meta");
    expect(transferKeyAction("c", { ctrl: true }, "ctrl")).toBe("copy");
    expect(transferKeyAction("v", { ctrl: true }, "ctrl")).toBe("paste");
    expect(transferKeyAction("c", { meta: true }, "ctrl")).toBe("ignore");
    expect(transferKeyAction("c", { ctrl: true }, "meta")).toBe("ignore");
    expect(transferKeyAction("c", { meta: true }, "meta")).toBe("copy");
    expect(transferKeyAction("V", { meta: true }, "meta")).toBe("paste");
    expect(transferKeyAction("c", { ctrl: true, shift: true }, "ctrl")).toBe(
      "ignore",
    );
    expect(transferKeyAction("c", { ctrl: true, alt: true }, "ctrl")).toBe(
      "ignore",
    );
    expect(transferKeyAction("x", { ctrl: true }, "ctrl")).toBe("ignore");
    expect(shortcutHint("ctrl")).toBe("Ctrl+C, Ctrl+V");
    expect(shortcutHint("meta")).toBe("Cmd+C, Cmd+V");
  });

  it("does not treat a path field paste as a transfer", () => {
    expect(listOwnsShortcut("div")).toBe(true);
    expect(listOwnsShortcut("INPUT")).toBe(false);
    expect(listOwnsShortcut("textarea")).toBe(false);
    expect(listOwnsShortcut("select")).toBe(false);
  });
});

describe("in-app copy mark", () => {
  it("stores a descriptor and never file bytes", () => {
    expect(copyCandidate(null, "ses-prod")).toEqual({
      ok: false,
      message: "Select a file to copy.",
    });
    expect(
      copyCandidate(
        { kind: "dir", path: "/etc/telegraf", name: "telegraf" },
        "ses-prod",
      ),
    ).toEqual({ ok: false, message: "Only a file can be copied." });
    expect(
      copyCandidate(
        { kind: "symlink", path: "/etc/telegraf/link", name: "link" },
        "ses-prod",
      ).ok,
    ).toBe(false);
    const marked = copyCandidate(
      { kind: "file", path: cpu.path, name: cpu.name },
      cpu.sessionId,
    );
    expect(marked).toEqual({ ok: true, ref: cpu });
    if (marked.ok) {
      const stored = rememberCopy(marked.ref);
      expect(stored).toEqual(cpu);
      expect(Object.keys(stored).sort()).toEqual(["name", "path", "sessionId"]);
      expect(JSON.stringify(stored)).not.toContain("content");
      expect(markedMessage(stored.name)).toBe("Marked cpu.conf to copy.");
    }
  });
});

describe("transfer planning", () => {
  const entries = [
    { name: "cpu.conf", kind: "file" as const },
    { name: "conf.d", kind: "dir" as const },
  ];

  it("pastes into the given directory and asks before replacing a file", () => {
    expect(
      planTransfer({
        source: cpu,
        toSession: "ses-staging",
        directory: "/etc/telegraf/conf.d",
        entries: [],
      }),
    ).toEqual({
      action: "start",
      from: cpu,
      toSession: "ses-staging",
      toPath: "/etc/telegraf/conf.d/cpu.conf",
      name: "cpu.conf",
      overwrite: false,
    });
    expect(
      planTransfer({
        source: cpu,
        toSession: "ses-staging",
        directory: "/var",
        entries,
      }),
    ).toMatchObject({
      action: "confirm-overwrite",
      toPath: "/var/cpu.conf",
      message: "Replace cpu.conf?",
    });
    expect(
      planTransfer({
        source: cpu,
        toSession: "ses-staging",
        directory: "/var",
        entries,
        overwriteConfirmed: true,
      }),
    ).toMatchObject({
      action: "start",
      overwrite: true,
      toPath: "/var/cpu.conf",
    });
  });

  it("asks for a new name when the destination is the same file", () => {
    const same = planTransfer({
      source: cpu,
      toSession: cpu.sessionId,
      directory: "/etc/telegraf",
      entries,
    });
    expect(same).toMatchObject({
      action: "rename",
      suggested: "cpu copy.conf",
      directory: "/etc/telegraf",
    });
    expect(suggestedCopyName("Makefile", ["Makefile copy"])).toBe(
      "Makefile copy 2",
    );
    expect(suggestedCopyName(".bashrc", [])).toBe(".bashrc copy");
    expect(
      planTransfer({
        source: cpu,
        toSession: cpu.sessionId,
        directory: "/etc/telegraf",
        entries,
        renamedTo: "cpu.conf",
      }),
    ).toEqual({ action: "refuse", message: "Choose a different name." });
    expect(
      planTransfer({
        source: cpu,
        toSession: cpu.sessionId,
        directory: "/etc/telegraf",
        entries,
        renamedTo: "mem.conf",
      }),
    ).toMatchObject({
      action: "start",
      toPath: "/etc/telegraf/mem.conf",
      overwrite: false,
    });
  });

  it("refuses a folder name and a missing destination session", () => {
    expect(
      planTransfer({
        source: cpu,
        toSession: "ses-staging",
        directory: "/etc",
        entries,
        renamedTo: "conf.d",
      }),
    ).toEqual({ action: "refuse", message: "That name is a folder." });
    expect(
      planTransfer({
        source: cpu,
        toSession: null,
        directory: "/etc",
        entries: [],
      }),
    ).toEqual({
      action: "refuse",
      message: "Connect the destination server.",
    });
    expect(
      planTransfer({
        source: null,
        toSession: "ses-staging",
        directory: "/etc",
        entries: [],
      }),
    ).toEqual({ action: "none" });
  });
});

describe("drag payload", () => {
  it("is a descriptor, not file bytes, and ignores an OS file drop", () => {
    const drag = dragData(cpu);
    expect(drag.mime).toBe(TRANSFER_MIME);
    expect(drag.text).toBe("cpu.conf");
    expect(drag.text).not.toContain("file-bytes");
    expect(Object.keys(JSON.parse(drag.descriptor)).sort()).toEqual([
      "name",
      "path",
      "sessionId",
    ]);
    expect(parseDropPayload(drag.descriptor)).toEqual(cpu);
    expect(acceptsDrop([TRANSFER_MIME])).toBe(true);
    expect(acceptsDrop(["Files"])).toBe(false);
    expect(acceptsDrop(["text/plain"])).toBe(false);
    expect(parseDropPayload("")).toBeNull();
    expect(parseDropPayload("not-json")).toBeNull();
    expect(
      parseDropPayload(JSON.stringify({ ...cpu, content: "secret-bytes" })),
    ).toBeNull();
    expect(
      parseDropPayload(
        JSON.stringify({ sessionId: "ses", path: "relative.conf", name: "a" }),
      ),
    ).toBeNull();
  });
});

describe("progress", () => {
  const job: TrackedTransfer = {
    id: "t-1",
    name: "cpu.conf",
    fromSession: "ses-prod",
    toSession: "ses-staging",
    toPath: "/var/cpu.conf",
    bytes: 0,
    total: 0,
    state: "running",
    message: null,
    code: null,
  };

  it("shows a status line and refreshes only when the copy finishes", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(transferStatusText(job)).toEqual({
      text: "Copying cpu.conf… 0 B / 0 B",
      canCancel: true,
    });
    const running = reduceTransfer(job, {
      id: "t-1",
      bytes: 12,
      total: 40,
      state: "running",
    });
    expect(running.refresh).toBe(false);
    expect(running.job.bytes).toBe(12);
    expect(
      reduceTransfer(job, { id: "other", bytes: 1, total: 1, state: "done" })
        .job,
    ).toBe(job);
    expect(
      reduceTransfer(job, {
        id: "t-1",
        bytes: 40,
        total: 40,
        state: "done",
      }).refresh,
    ).toBe(true);
    const failed = reduceTransfer(job, {
      id: "t-1",
      bytes: 0,
      total: 40,
      state: "failed",
      code: "already_exists",
      message: "That name already exists.",
    });
    expect(failed.conflict).toBe(true);
    expect(failed.refresh).toBe(false);
    expect(transferStatusText(failed.job)?.canCancel).toBe(false);
    expect(transferStatusText({ ...job, state: "cancelled" })?.text).toBe(
      "Copy cancelled.",
    );
    expect(transferStatusText({ ...job, state: "done" })).toBeNull();
  });

  it("keeps a progress event that arrives before the start call returns", () => {
    const early = {
      id: "t-1",
      bytes: 40,
      total: 40,
      state: "done" as const,
    };
    const pending = rememberUnmatched([], early);
    const taken = takeBuffered(pending, "t-1");
    expect(taken.matched).toEqual([early]);
    expect(taken.rest).toEqual([]);
    expect(takeBuffered(pending, "other").matched).toEqual([]);
  });
});
