import { describe, expect, it } from "vitest";
import {
  confirmedDelete,
  deletePrompt,
  entryName,
  neighborName,
  planMkdir,
  planRename,
} from "./fileOps";

describe("file names", () => {
  it("keeps a single segment, including characters a shell would treat specially", () => {
    expect(entryName(" cpu.conf ")).toEqual({ ok: true, name: "cpu.conf" });
    expect(entryName("my file; rm -rf.conf")).toEqual({
      ok: true,
      name: "my file; rm -rf.conf",
    });
    expect(entryName("foo\\bar")).toEqual({ ok: true, name: "foo\\bar" });
    expect(entryName("")).toEqual({
      ok: false,
      message: "Enter a file name.",
    });
    expect(entryName(".")).toEqual({
      ok: false,
      message: "Enter a file name.",
    });
    expect(entryName("..")).toEqual({
      ok: false,
      message: "Enter a file name.",
    });
    expect(entryName("conf.d/cpu.conf")).toEqual({
      ok: false,
      message: "Enter a file name.",
    });
    expect(entryName("a\0b")).toEqual({
      ok: false,
      message: "That name is not valid.",
    });
    const invalid = entryName("a\0b");
    if (!invalid.ok) {
      expect(invalid.message).not.toContain("sudo");
    }
  });
});

describe("mkdir and rename plans", () => {
  it("joins a folder name onto the current directory and does not build a command", () => {
    expect(planMkdir("/etc/telegraf", "conf.d")).toEqual({
      ok: true,
      path: "/etc/telegraf/conf.d",
      name: "conf.d",
    });
    expect(planMkdir("/", "etc")).toEqual({
      ok: true,
      path: "/etc",
      name: "etc",
    });
    expect(planMkdir("/etc", "my file; rm -rf")).toEqual({
      ok: true,
      path: "/etc/my file; rm -rf",
      name: "my file; rm -rf",
    });
    expect(planMkdir("/etc", "../passwd").ok).toBe(false);
    expect(planMkdir("relative", "conf.d").ok).toBe(false);
  });

  it("renames inside the parent directory and skips an unchanged name", () => {
    const entry = {
      path: "/etc/telegraf/cpu.conf",
      name: "cpu.conf",
    };
    expect(planRename(entry, "cpu.conf")).toEqual({
      ok: true,
      unchanged: true,
    });
    expect(planRename(entry, "mem.conf")).toEqual({
      ok: true,
      unchanged: false,
      from: "/etc/telegraf/cpu.conf",
      to: "/etc/telegraf/mem.conf",
      name: "mem.conf",
    });
    expect(planRename(entry, "conf.d/mem.conf").ok).toBe(false);
    expect(planRename({ path: "/", name: "/" }, "etc").ok).toBe(false);
    const refused = planRename(entry, "..");
    expect(refused.ok).toBe(false);
    if (!refused.ok) {
      expect(refused.message).not.toContain("sudo");
    }
  });
});

describe("confirmed delete", () => {
  it("asks before deleting and refuses to walk a folder or follow a link", () => {
    const folder = deletePrompt({
      path: "/etc/telegraf/conf.d",
      name: "conf.d",
      kind: "dir",
    });
    expect(folder).toEqual({
      ok: true,
      path: "/etc/telegraf/conf.d",
      message: "Delete folder conf.d? Only an empty folder is removed.",
    });
    const link = deletePrompt({
      path: "/etc/telegraf/link",
      name: "link",
      kind: "symlink",
    });
    expect(link.ok).toBe(true);
    if (link.ok) {
      expect(link.message).toContain("not what it points to");
      expect(link.message.toLowerCase()).not.toContain("sudo");
    }
    const file = deletePrompt({
      path: "/etc/telegraf/cpu.conf",
      name: "cpu.conf",
      kind: "file",
    });
    expect(file.ok).toBe(true);
    if (file.ok) {
      expect(file.message).toBe("Delete cpu.conf? This cannot be undone.");
    }
    expect(deletePrompt({ path: "/", name: "/", kind: "dir" })).toEqual({
      ok: false,
      message: "That path cannot be changed.",
    });

    expect(confirmedDelete(null, "/etc/telegraf/cpu.conf")).toBeNull();
    expect(
      confirmedDelete("/etc/telegraf/cpu.conf", "/etc/telegraf/mem.conf"),
    ).toBeNull();
    expect(confirmedDelete("/", "/")).toBeNull();
    expect(
      confirmedDelete("/etc/telegraf/cpu.conf", "/etc/telegraf/cpu.conf"),
    ).toBe("/etc/telegraf/cpu.conf");
  });

  it("selects a neighbor after a delete without selecting more than one", () => {
    const entries = [
      { name: "conf.d" },
      { name: "cpu.conf" },
      { name: "mem.conf" },
    ];
    expect(neighborName(entries, "cpu.conf")).toBe("mem.conf");
    expect(neighborName(entries, "mem.conf")).toBe("cpu.conf");
    expect(neighborName(entries, "missing")).toBeNull();
    expect(neighborName([{ name: "only.conf" }], "only.conf")).toBeNull();
  });
});
