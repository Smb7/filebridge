import { describe, expect, it } from "vitest";
import {
  SCROLLBACK_LIMIT,
  TermBuffer,
  chunkInput,
  decodeTermData,
  encodeBinaryInput,
  encodeTextInput,
  nextSelection,
  planSwitch,
  routeTermData,
  terminalKeepsKeys,
  visibleSession,
} from "./terminal";

describe("terminal bytes", () => {
  it("round-trips text and binary without treating ctrl+c as text loss", () => {
    const interrupt = encodeTextInput("\u0003");
    expect(Array.from(decodeTermData(interrupt))).toEqual([3]);
    const binary = encodeBinaryInput("\u0000\u00ff");
    expect(Array.from(decodeTermData(binary))).toEqual([0, 255]);
  });

  it("does not split a surrogate pair when chunking a paste", () => {
    expect(chunkInput("a😀b", 2)).toEqual(["a", "😀", "b"]);
    expect(chunkInput("", 8)).toEqual([]);
    expect(chunkInput("hi", 1)).toEqual(["hi"]);
  });

  it("writes only the active shell and keeps the other in the buffer", () => {
    const buffer = new TermBuffer();
    const active = decodeTermData(encodeTextInput("a"));
    const other = decodeTermData(encodeTextInput("b"));
    expect(routeTermData("term-a", "term-a")).toBe("write");
    expect(routeTermData("term-a", "term-b")).toBe("hold");
    expect(routeTermData(null, "term-a")).toBe("hold");
    buffer.push("term-a", active);
    buffer.push("term-b", other);
    expect(Array.from(buffer.replay("term-b"))).toEqual(
      Array.from(decodeTermData(encodeTextInput("b"))),
    );
    buffer.drop("term-b");
    expect(buffer.replay("term-b").byteLength).toBe(0);
  });

  it("drops oldest scrollback once the cap is passed", () => {
    const buffer = new TermBuffer();
    buffer.push("term-a", new Uint8Array(SCROLLBACK_LIMIT - 2).fill(1));
    buffer.push("term-a", new Uint8Array([2, 3, 4]));
    const replay = buffer.replay("term-a");
    expect(replay.byteLength).toBeLessThanOrEqual(SCROLLBACK_LIMIT);
    expect(replay[replay.byteLength - 1]).toBe(4);
  });
});

describe("terminal selection", () => {
  it("keeps the chosen server and does not close the other shell", () => {
    const plan = planSwitch("ses-b");
    expect(plan.closeTermId).toBeNull();
    expect(plan.openSessionId).toBe("ses-b");
    expect(
      nextSelection({
        sessions: ["ses-a", "ses-b"],
        selected: "ses-b",
        preferred: "ses-a",
      }),
    ).toBe("ses-b");
  });

  it("follows the focused pane until the user picks a server", () => {
    expect(
      visibleSession({
        sessions: ["ses-a", "ses-b"],
        selected: "ses-b",
        preferred: "ses-a",
        userPicked: false,
      }),
    ).toBe("ses-a");
    expect(
      visibleSession({
        sessions: ["ses-a", "ses-b"],
        selected: "ses-b",
        preferred: "ses-a",
        userPicked: true,
      }),
    ).toBe("ses-b");
    expect(planSwitch("ses-a").closeTermId).toBeNull();
  });

  it("falls back when the selected server drops", () => {
    expect(
      nextSelection({
        sessions: ["ses-a"],
        selected: "ses-b",
        preferred: "ses-a",
      }),
    ).toBe("ses-a");
    expect(
      nextSelection({
        sessions: [],
        selected: null,
        preferred: null,
      }),
    ).toBeNull();
    expect(
      nextSelection({
        sessions: ["ses-b", "ses-a"],
        selected: null,
        preferred: null,
      }),
    ).toBe("ses-b");
  });

  it("leaves terminal ctrl+c to the shell helper, not the file list", () => {
    expect(terminalKeepsKeys("textarea")).toBe(true);
    expect(terminalKeepsKeys("TEXTAREA")).toBe(true);
    expect(terminalKeepsKeys("div")).toBe(false);
  });
});
