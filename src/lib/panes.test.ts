import { describe, expect, it } from "vitest";
import {
  acceptListing,
  applyPathCommit,
  beginListing,
  createBrowser,
  listKeyAction,
} from "./browser";
import {
  PANE_SIDES,
  assignConnection,
  clearConnection,
  createDualPane,
  dropMissingConnections,
  focusPane,
  otherSide,
  paneHeader,
  paneLabel,
  pasteTarget,
  setDirectory,
} from "./panes";

const prod = {
  name: "prod-telegraf",
  username: "ubuntu",
  host: "10.0.0.8",
  port: 22,
};

describe("dual pane", () => {
  it("starts with two unbound panes and the left one focused", () => {
    expect(PANE_SIDES).toEqual(["left", "right"]);
    expect(createDualPane()).toEqual({
      focused: "left",
      left: { connectionId: null, directory: "/" },
      right: { connectionId: null, directory: "/" },
    });
    expect(paneLabel("left")).toBe("Left pane");
    expect(otherSide("left")).toBe("right");
    expect(otherSide("right")).toBe("left");
    expect(listKeyAction("Tab")).toEqual({ type: "focus-other" });
  });

  it("keeps the server name visible when the pane is not focused", () => {
    const focused = paneHeader(prod, true);
    const idle = paneHeader(prod, false);
    expect(idle).toEqual({
      title: "prod-telegraf",
      endpoint: "ubuntu@10.0.0.8:22",
      focused: false,
    });
    expect(focused.title).toBe(idle.title);
    expect(focused.endpoint).toBe(idle.endpoint);
    expect(paneHeader(null, false)).toEqual({
      title: "No server",
      endpoint: "Choose a server",
      focused: false,
    });
  });

  it("lets both panes share a session without sharing a directory", () => {
    let state = assignConnection(createDualPane(), "left", "conn-1", "/etc");
    state = assignConnection(state, "right", "conn-1", "/var");
    state = setDirectory(state, "right", "/var/log");
    expect(state.left).toEqual({ connectionId: "conn-1", directory: "/etc" });
    expect(state.right).toEqual({
      connectionId: "conn-1",
      directory: "/var/log",
    });

    const listed = beginListing(createBrowser(state.left.directory));
    const leftBrowser = acceptListing(
      listed.state,
      listed.generation,
      "/etc/telegraf",
      [],
    );
    state = setDirectory(state, "left", leftBrowser.path);
    expect(state.left.directory).toBe("/etc/telegraf");
    expect(state.right.directory).toBe("/var/log");
    expect(state.right.connectionId).toBe("conn-1");
  });

  it("changes one server without resetting the other pane", () => {
    let state = assignConnection(createDualPane(), "left", "conn-1", "/etc");
    state = assignConnection(state, "right", "conn-1", "/var");
    state = setDirectory(state, "right", "/var/log");
    state = assignConnection(state, "left", "conn-2", "/opt");
    expect(state.left).toEqual({ connectionId: "conn-2", directory: "/opt" });
    expect(state.right).toEqual({
      connectionId: "conn-1",
      directory: "/var/log",
    });

    const kept = assignConnection(state, "right", "conn-1", "/");
    expect(kept).toBe(state);
    expect(kept.right.directory).toBe("/var/log");
  });

  it("moves the accent without moving either directory", () => {
    let state = assignConnection(createDualPane(), "left", "conn-1", "/etc");
    state = assignConnection(state, "right", "conn-2", "/srv");
    const focused = focusPane(state, "right");
    expect(focused.focused).toBe("right");
    expect(focused.left).toEqual(state.left);
    expect(focused.right).toEqual(state.right);
    expect(focusPane(focused, "right")).toBe(focused);
    expect(setDirectory(focused, "left", focused.left.directory)).toBe(focused);
  });

  it("does not change either server when a typed path is rejected", () => {
    let state = assignConnection(createDualPane(), "left", "conn-a", "/etc");
    state = assignConnection(state, "right", "conn-b", "/var");
    const committed = applyPathCommit(
      createBrowser(state.left.directory),
      "relative.conf",
    );
    expect(committed.list).toBeNull();
    expect(committed.state.path).toBe("/etc");
    const next = setDirectory(state, "left", "relative.conf");
    expect(next).toBe(state);
    expect(state.left.connectionId).toBe("conn-a");
    expect(state.right.directory).toBe("/var");
  });

  it("drops a missing connection from both panes and leaves the other server", () => {
    let state = assignConnection(createDualPane(), "left", "conn-1", "/etc");
    state = assignConnection(state, "right", "conn-2", "/var");
    state = clearConnection(state, "conn-2");
    expect(state.right).toEqual({ connectionId: null, directory: "/" });
    expect(state.left.directory).toBe("/etc");

    state = assignConnection(state, "right", "conn-2", "/var");
    state = dropMissingConnections(state, ["conn-1"]);
    expect(state.left.connectionId).toBe("conn-1");
    expect(state.right.connectionId).toBeNull();
    expect(dropMissingConnections(state, ["conn-1"])).toBe(state);
  });

  it("targets paste at the focused pane's current directory", () => {
    const shared = {
      focused: "right" as const,
      left: {
        connectionId: "conn-1",
        directory: "/etc/telegraf",
        sessionId: "ses-1",
      },
      right: {
        connectionId: "conn-1",
        directory: "/var/log",
        sessionId: "ses-1",
      },
    };
    expect(pasteTarget(shared)).toEqual({
      side: "right",
      connectionId: "conn-1",
      sessionId: "ses-1",
      path: "/var/log",
    });
    const leftTarget = pasteTarget({ ...shared, focused: "left" });
    expect(leftTarget?.path).toBe("/etc/telegraf");
    expect(
      pasteTarget({
        ...shared,
        right: { ...shared.right, sessionId: null },
      }),
    ).toBeNull();
    expect(
      pasteTarget({
        focused: "left",
        left: {
          connectionId: null,
          directory: "/etc",
          sessionId: "ses-1",
        },
        right: shared.right,
      }),
    ).toBeNull();
  });
});
