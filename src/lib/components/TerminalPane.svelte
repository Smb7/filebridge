<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { FitAddon } from "@xterm/addon-fit";
  import { Terminal } from "@xterm/xterm";
  import { onMount } from "svelte";
  import {
    isAppError,
    termOpen,
    termResize,
    termWrite,
    type LiveSession,
  } from "$lib/api";
  import {
    INPUT_CHUNK,
    TermBuffer,
    chunkInput,
    decodeTermData,
    encodeBinaryInput,
    encodeTextInput,
    routeTermData,
    type TermClosedEvent,
    type TermDataEvent,
    visibleSession,
  } from "$lib/terminal";
  import "@xterm/xterm/css/xterm.css";

  type Props = {
    sessions: LiveSession[];
    preferredSessionId: string | null;
  };

  let { sessions, preferredSessionId }: Props = $props();

  let host = $state<HTMLDivElement | null>(null);
  let selectedSessionId = $state<string | null>(null);
  let status = $state("");

  // Ids and the widget live outside $state. Terminal bytes never do.
  const view = {
    termId: null as string | null,
    sessionId: null as string | null,
    attaching: false,
    xterm: null as Terminal | null,
  };
  const buffer = new TermBuffer();
  const closedSessions = new Set<string>();
  let userPicked = false;
  let attachGen = 0;
  let resizeTimer: ReturnType<typeof setTimeout> | undefined;

  function showStatus(error: unknown) {
    status = isAppError(error) ? error.message : "Could not open a shell.";
  }

  async function sendInput(encoded: string) {
    const id = view.termId;
    if (!id || encoded.length === 0) {
      return;
    }
    try {
      for (const chunk of chunkInput(encoded, INPUT_CHUNK)) {
        if (view.termId !== id) {
          return;
        }
        await termWrite(id, chunk);
      }
    } catch (error) {
      showStatus(error);
    }
  }

  function onBytes(id: string, data: string) {
    let bytes: Uint8Array;
    try {
      bytes = decodeTermData(data);
    } catch {
      return;
    }
    buffer.push(id, bytes);
    if (
      routeTermData(view.termId, id) === "write" &&
      !view.attaching &&
      view.xterm
    ) {
      view.xterm.write(bytes);
    }
  }

  function onClosed(id: string) {
    buffer.drop(id);
    if (view.termId !== id) {
      return;
    }
    if (view.sessionId) {
      closedSessions.add(view.sessionId);
    }
    view.termId = null;
    view.sessionId = null;
    status = "Shell closed.";
  }

  async function attach(sessionId: string) {
    const term = view.xterm;
    if (!term) {
      return;
    }
    const gen = ++attachGen;
    closedSessions.delete(sessionId);
    try {
      const opened = await termOpen(
        sessionId,
        term.cols || 80,
        term.rows || 24,
      );
      if (gen !== attachGen) {
        return;
      }
      view.attaching = true;
      view.termId = opened.termId;
      view.sessionId = sessionId;
      term.reset();
      const replay = buffer.replay(opened.termId);
      if (replay.byteLength > 0) {
        term.write(replay);
      }
      view.attaching = false;
      status = "";
      void termResize(opened.termId, term.cols, term.rows).catch(() => {});
    } catch (error) {
      if (gen === attachGen) {
        view.attaching = false;
        showStatus(error);
      }
    }
  }

  function syncSelection() {
    const next = visibleSession({
      sessions: sessions.map((session) => session.sessionId),
      selected: selectedSessionId,
      preferred: preferredSessionId,
      userPicked,
    });
    if (next !== selectedSessionId) {
      selectedSessionId = next;
    }
    if (!next) {
      view.termId = null;
      view.sessionId = null;
      view.xterm?.reset();
      return;
    }
    if (!view.xterm || closedSessions.has(next) || next === view.sessionId) {
      return;
    }
    void attach(next);
  }

  function onPick(event: Event) {
    const select = event.currentTarget;
    if (!(select instanceof HTMLSelectElement)) {
      return;
    }
    const id = select.value;
    userPicked = true;
    selectedSessionId = id || null;
    if (id) {
      void attach(id);
    }
  }

  function reopen() {
    if (selectedSessionId) {
      void attach(selectedSessionId);
    }
  }

  $effect(() => {
    sessions;
    preferredSessionId;
    selectedSessionId;
    syncSelection();
  });

  onMount(() => {
    const term = new Terminal({
      cursorBlink: true,
      fontFamily: "ui-monospace, Consolas, monospace",
      fontSize: 13,
      scrollback: 2000,
      logLevel: "off",
      theme: {
        background: "#1c1c1c",
        foreground: "#e8e8e8",
        cursor: "#e8e8e8",
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    if (host) {
      term.open(host);
      fit.fit();
    }
    view.xterm = term;
    const dataSub = term.onData((data) => {
      void sendInput(encodeTextInput(data));
    });
    const binarySub = term.onBinary((data) => {
      void sendInput(encodeBinaryInput(data));
    });
    const unlistenData = listen<TermDataEvent>("term://data", (event) => {
      onBytes(event.payload.id, event.payload.data);
    });
    const unlistenClosed = listen<TermClosedEvent>("term://closed", (event) => {
      onClosed(event.payload.id);
    });
    const observer = new ResizeObserver(() => {
      fit.fit();
      const id = view.termId;
      if (!id) {
        return;
      }
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        void termResize(id, term.cols, term.rows).catch(() => {});
      }, 50);
    });
    if (host) {
      observer.observe(host);
    }
    syncSelection();
    return () => {
      clearTimeout(resizeTimer);
      observer.disconnect();
      dataSub.dispose();
      binarySub.dispose();
      term.dispose();
      view.xterm = null;
      void unlistenData.then((stop) => stop());
      void unlistenClosed.then((stop) => stop());
    };
  });
</script>

<section class="terminal" aria-label="Terminal">
  <header>
    <span>Terminal</span>
    <select
      aria-label="Terminal server"
      value={selectedSessionId ?? ""}
      onchange={onPick}
      disabled={sessions.length === 0}
    >
      {#if sessions.length === 0}
        <option value="">No server connected</option>
      {:else}
        {#each sessions as session (session.sessionId)}
          <option value={session.sessionId}>{session.name}</option>
        {/each}
      {/if}
    </select>
    {#if status === "Shell closed." && selectedSessionId}
      <button type="button" onclick={reopen}>Open shell</button>
    {/if}
  </header>
  {#if status}
    <p class="status">{status}</p>
  {/if}
  <div class="screen" bind:this={host}></div>
</section>

<style>
  .terminal {
    height: 100%;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: #1c1c1c;
    color: #e8e8e8;
  }

  header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.35rem 0.7rem;
    border-bottom: 1px solid color-mix(in srgb, #e8e8e8 18%, transparent);
  }

  header span {
    font-size: 0.85rem;
  }

  select,
  button {
    font: inherit;
    color: inherit;
    background: #2a2a2a;
    border: 1px solid color-mix(in srgb, #e8e8e8 28%, transparent);
    border-radius: 6px;
    padding: 0.2rem 0.45rem;
  }

  select {
    margin-left: auto;
  }

  .status {
    margin: 0;
    padding: 0.2rem 0.7rem;
    font-size: 0.8rem;
    color: color-mix(in srgb, #ffb4b4 80%, white);
  }

  .screen {
    flex: 1;
    min-height: 0;
    overflow: hidden;
    padding: 0.2rem 0.35rem 0.35rem;
  }

  .screen :global(.xterm) {
    height: 100%;
  }
</style>
