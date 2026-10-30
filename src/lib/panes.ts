import { serverLabel, startingPath } from "./browser";

/**
 * Pane bindings are UI state. The backend does not know left from right.
 * Two panes may point at one live session and still keep different directories.
 */
export const PANE_SIDES = ["left", "right"] as const;

export type PaneSide = (typeof PANE_SIDES)[number];

export type PaneBinding = {
  connectionId: string | null;
  directory: string;
};

export type DualPane = {
  focused: PaneSide;
  left: PaneBinding;
  right: PaneBinding;
};

export type PaneSnapshot = {
  connectionId: string | null;
  directory: string;
  sessionId: string | null;
};

export type PasteTarget = {
  side: PaneSide;
  connectionId: string;
  sessionId: string;
  path: string;
};

function unbound(): PaneBinding {
  return { connectionId: null, directory: "/" };
}

export function createDualPane(): DualPane {
  return {
    focused: "left",
    left: unbound(),
    right: unbound(),
  };
}

export function otherSide(side: PaneSide): PaneSide {
  return side === "left" ? "right" : "left";
}

export function paneLabel(side: PaneSide): string {
  return side === "left" ? "Left pane" : "Right pane";
}

export function focusPane(state: DualPane, side: PaneSide): DualPane {
  if (state.focused === side) {
    return state;
  }
  return { ...state, focused: side };
}

/** Re-selecting the same server keeps that pane's directory. */
export function assignConnection(
  state: DualPane,
  side: PaneSide,
  connectionId: string,
  directory = "/",
): DualPane {
  if (!connectionId || state[side].connectionId === connectionId) {
    return state;
  }
  const nextDirectory = directory.startsWith("/") ? directory : "/";
  return {
    ...state,
    [side]: { connectionId, directory: nextDirectory },
  };
}

export function setDirectory(
  state: DualPane,
  side: PaneSide,
  directory: string,
): DualPane {
  if (!directory.startsWith("/") || state[side].directory === directory) {
    return state;
  }
  return {
    ...state,
    [side]: { ...state[side], directory },
  };
}

export function clearConnection(
  state: DualPane,
  connectionId: string,
): DualPane {
  if (!connectionId) {
    return state;
  }
  let next = state;
  for (const side of PANE_SIDES) {
    if (next[side].connectionId === connectionId) {
      next = { ...next, [side]: unbound() };
    }
  }
  return next;
}

export function dropMissingConnections(
  state: DualPane,
  ids: readonly string[],
): DualPane {
  const known = new Set(ids);
  let next = state;
  for (const side of PANE_SIDES) {
    const id = next[side].connectionId;
    if (id && !known.has(id)) {
      next = clearConnection(next, id);
    }
  }
  return next;
}

/** Header text stays up on the unfocused pane. Focus only changes the accent. */
export function paneHeader(
  connection: {
    name: string;
    username: string;
    host: string;
    port: number;
  } | null,
  focused: boolean,
): { title: string; endpoint: string; focused: boolean } {
  if (!connection) {
    return { title: "No server", endpoint: "Choose a server", focused };
  }
  return {
    title: connection.name,
    endpoint: serverLabel(connection),
    focused,
  };
}

/**
 * A remembered absolute directory wins over the connection default.
 * Session loss must not substitute the other pane's path or "/".
 */
export function paneListPath(
  directory: string,
  defaultPath: string | null | undefined,
): string {
  if (directory.startsWith("/")) {
    return directory;
  }
  return startingPath(defaultPath);
}

/** List paste belongs to the focused pane's current directory, not the other one. */
export function pasteTarget(input: {
  focused: PaneSide;
  left: PaneSnapshot;
  right: PaneSnapshot;
}): PasteTarget | null {
  const pane = input[input.focused];
  if (
    !pane.connectionId ||
    !pane.sessionId ||
    !pane.directory.startsWith("/")
  ) {
    return null;
  }
  return {
    side: input.focused,
    connectionId: pane.connectionId,
    sessionId: pane.sessionId,
    path: pane.directory,
  };
}
