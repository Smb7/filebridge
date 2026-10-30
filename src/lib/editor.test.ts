import { describe, expect, it } from "vitest";
import {
  DRAFT_DEBOUNCE_MS,
  applyRemoteRead,
  canSave,
  canSaveOther,
  closeChoice,
  initialBuffer,
  otherSaveLabel,
  otherSaveTarget,
  planRemoteWrite,
  planSave,
  quitChoice,
  sameFile,
} from "./editor";

const baseline = "ab".repeat(32);

describe("editor windows", () => {
  it("sends the loaded hash and does not replace on a silent overwrite", () => {
    expect(
      planSave({
        connected: true,
        sessionId: "ses-a",
        path: "/etc/telegraf/cpu.conf",
        baselineSha256: baseline,
        force: false,
      }),
    ).toEqual({
      action: "write",
      sessionId: "ses-a",
      path: "/etc/telegraf/cpu.conf",
      expectedSha256: baseline,
    });

    const confirmed = planSave({
      connected: true,
      sessionId: "ses-a",
      path: "/etc/telegraf/cpu.conf",
      baselineSha256: baseline,
      force: true,
    });
    expect(confirmed).toEqual({
      action: "write",
      sessionId: "ses-a",
      path: "/etc/telegraf/cpu.conf",
    });
    expect(confirmed).not.toHaveProperty("expectedSha256");

    const blocked = planSave({
      connected: false,
      sessionId: null,
      path: "/etc/telegraf/cpu.conf",
      baselineSha256: baseline,
      force: false,
    });
    expect(blocked.action).toBe("blocked");
    if (blocked.action === "blocked") {
      expect(blocked.message).toContain("connected again");
      expect(blocked.message.toLowerCase()).not.toContain("sudo");
      expect(blocked.message.toLowerCase()).not.toContain("password");
    }
    expect(canSave(false, false)).toBe(false);
    expect(canSave(true, true)).toBe(false);
    expect(canSave(true, false)).toBe(true);
  });

  it("writes save-as on the same session and save-to-other without a download", () => {
    expect(
      planRemoteWrite({
        sessionId: "ses-a",
        path: "/etc/telegraf/../telegraf/disk.conf",
      }),
    ).toEqual({
      ok: true,
      sessionId: "ses-a",
      path: "/etc/telegraf/disk.conf",
    });
    expect(planRemoteWrite({ sessionId: "ses-a", path: "/" }).ok).toBe(false);
    expect(planRemoteWrite({ sessionId: null, path: "/cpu.conf" }).ok).toBe(
      false,
    );

    const other = otherSaveTarget({
      editorSessionId: "ses-a",
      editorPath: "/etc/telegraf/cpu.conf",
      peer: {
        sessionId: "ses-b",
        directory: "/etc/telegraf/conf.d",
        name: "staging-telegraf",
        endpoint: "ubuntu@10.0.0.9:22",
      },
    });
    expect(other).toEqual({
      sessionId: "ses-b",
      path: "/etc/telegraf/conf.d/cpu.conf",
    });
    expect(JSON.stringify(other)).not.toContain("interval");

    expect(
      otherSaveTarget({
        editorSessionId: "ses-a",
        editorPath: "/etc/telegraf/cpu.conf",
        peer: {
          sessionId: "ses-a",
          directory: "/etc/telegraf",
          name: "prod",
          endpoint: null,
        },
      }),
    ).toBeNull();
    expect(
      otherSaveTarget({
        editorSessionId: "ses-a",
        editorPath: "/etc/telegraf/cpu.conf",
        peer: {
          sessionId: null,
          directory: "/etc",
          name: null,
          endpoint: null,
        },
      }),
    ).toBeNull();
    expect(canSaveOther(null, false)).toBe(false);
    expect(canSaveOther("ses-b", false)).toBe(true);
    expect(otherSaveLabel("staging-telegraf")).toBe("Save to staging-telegraf");
    expect(otherSaveLabel(null)).toBe("Save to other pane");
    expect(
      sameFile(
        { sessionId: "ses-a", path: "/etc/cpu.conf" },
        { sessionId: "ses-a", path: "/etc/cpu.conf" },
      ),
    ).toBe(true);
  });

  it("keeps a dirty buffer across reload and prompts before quit", () => {
    expect(DRAFT_DEBOUNCE_MS).toBeGreaterThan(0);
    expect(DRAFT_DEBOUNCE_MS).toBeLessThan(1000);
    expect(
      initialBuffer({
        draft: 'interval = "10s"\n',
        dirty: true,
        baselineSha256: baseline,
      }),
    ).toEqual({
      content: 'interval = "10s"\n',
      dirty: true,
      baselineSha256: baseline,
      readRemote: false,
    });
    expect(
      initialBuffer({
        draft: null,
        dirty: false,
        baselineSha256: null,
      }).readRemote,
    ).toBe(true);
    expect(applyRemoteRead("ok\n", baseline)).toEqual({
      content: "ok\n",
      dirty: false,
      baselineSha256: baseline,
      readRemote: false,
    });
    expect(closeChoice(true)).toBe("prompt");
    expect(closeChoice(false)).toBe("close");
    expect(quitChoice(1)).toBe("prompt");
    expect(quitChoice(0)).toBe("quit");
  });
});
