import { listOwnsShortcut } from "./transfer";

/** Recent output kept off the Svelte graph so a switch can refill the one widget. */
export const SCROLLBACK_LIMIT = 256 * 1024;
export const INPUT_CHUNK = 32 * 1024;

export type TermDataEvent = {
  id: string;
  data: string;
};

export type TermClosedEvent = {
  id: string;
};

export type SwitchPlan = {
  closeTermId: null;
  openSessionId: string;
};

export function decodeTermData(data: string): Uint8Array {
  const binary = atob(data);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

export function encodeTermInput(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

export function encodeTextInput(data: string): string {
  return encodeTermInput(new TextEncoder().encode(data));
}

/** xterm onBinary is latin1, not UTF-8. */
export function encodeBinaryInput(data: string): string {
  const bytes = new Uint8Array(data.length);
  for (let i = 0; i < data.length; i += 1) {
    bytes[i] = data.charCodeAt(i) & 0xff;
  }
  return encodeTermInput(bytes);
}

export function chunkInput(data: string, max: number): string[] {
  if (data.length === 0) {
    return [];
  }
  if (max < 2) {
    return [data];
  }
  const chunks: string[] = [];
  let start = 0;
  while (start < data.length) {
    let end = Math.min(start + max, data.length);
    if (end < data.length) {
      const tail = data.charCodeAt(end - 1);
      if (tail >= 0xd800 && tail <= 0xdbff) {
        end -= 1;
      }
    }
    if (end <= start) {
      end = Math.min(start + 2, data.length);
    }
    chunks.push(data.slice(start, end));
    start = end;
  }
  return chunks;
}

export class TermBuffer {
  private chunks = new Map<string, Uint8Array[]>();
  private sizes = new Map<string, number>();

  push(id: string, bytes: Uint8Array): void {
    if (bytes.byteLength === 0) {
      return;
    }
    const list = this.chunks.get(id) ?? [];
    list.push(bytes);
    const size = (this.sizes.get(id) ?? 0) + bytes.byteLength;
    this.chunks.set(id, list);
    this.sizes.set(id, size);
    this.trim(id);
  }

  replay(id: string): Uint8Array {
    const list = this.chunks.get(id) ?? [];
    const size = this.sizes.get(id) ?? 0;
    const out = new Uint8Array(size);
    let offset = 0;
    for (const chunk of list) {
      out.set(chunk, offset);
      offset += chunk.byteLength;
    }
    return out;
  }

  drop(id: string): void {
    this.chunks.delete(id);
    this.sizes.delete(id);
  }

  private trim(id: string): void {
    let size = this.sizes.get(id) ?? 0;
    const list = this.chunks.get(id);
    if (!list) {
      return;
    }
    while (size > SCROLLBACK_LIMIT && list.length > 1) {
      const removed = list.shift();
      if (!removed) {
        break;
      }
      size -= removed.byteLength;
    }
    if (size > SCROLLBACK_LIMIT && list[0]) {
      const only = list[0];
      list[0] = only.subarray(only.byteLength - SCROLLBACK_LIMIT);
      size = list[0].byteLength;
    }
    this.sizes.set(id, size);
  }
}

/** Active bytes go to xterm. Inactive bytes stay in the buffer, not a store. */
export function routeTermData(
  activeId: string | null,
  eventId: string,
): "write" | "hold" {
  return activeId === eventId ? "write" : "hold";
}

/** Switching the visible server never closes the other shell. */
export function planSwitch(nextSessionId: string): SwitchPlan {
  return { closeTermId: null, openSessionId: nextSessionId };
}

export function nextSelection(input: {
  sessions: string[];
  selected: string | null;
  preferred: string | null;
}): string | null {
  const live = new Set(input.sessions);
  if (input.selected && live.has(input.selected)) {
    return input.selected;
  }
  if (input.preferred && live.has(input.preferred)) {
    return input.preferred;
  }
  return input.sessions[0] ?? null;
}

/** Follow the focused pane until the user picks a server in the selector. */
export function visibleSession(input: {
  sessions: string[];
  selected: string | null;
  preferred: string | null;
  userPicked: boolean;
}): string | null {
  return nextSelection({
    sessions: input.sessions,
    selected: input.userPicked ? input.selected : null,
    preferred: input.preferred,
  });
}

/** File-list Ctrl/Cmd+C must not own the terminal helper. */
export function terminalKeepsKeys(tagName: string): boolean {
  return !listOwnsShortcut(tagName);
}
