<script lang="ts">
  import { emit, listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount } from "svelte";
  import { EditorView, basicSetup } from "codemirror";
  import { EditorState } from "@codemirror/state";
  import {
    editorAllowClose,
    editorCancelClose,
    editorCancelQuit,
    editorDiscard,
    editorDraft,
    editorNoteSaved,
    editorQuit,
    editorState,
    fsReadText,
    fsWrite,
    isAppError,
    type QuitPrompt,
  } from "$lib/api";
  import {
    DRAFT_DEBOUNCE_MS,
    applyRemoteRead,
    canSave,
    canSaveOther,
    initialBuffer,
    otherSaveLabel,
    otherSaveTarget,
    planRemoteWrite,
    planSave,
    sameFile,
    settleSave,
    type EditorPeer,
  } from "$lib/editor";

  let host = $state<HTMLDivElement | null>(null);
  let view: EditorView | null = null;
  let editorId = $state("");
  let sessionId = $state<string | null>(null);
  let path = $state("");
  let name = $state("");
  let endpoint = $state("");
  let connected = $state(false);
  let dirty = $state(false);
  let baseline = $state<string | null>(null);
  let status = $state("Opening…");
  let busy = $state(false);
  let peer = $state<EditorPeer | null>(null);
  let askClose = $state(false);
  let askQuit = $state<QuitPrompt | null>(null);
  let askConflict = $state(false);
  let pathPrompt = $state<{
    kind: "save-as" | "other";
    path: string;
    sessionId: string;
  } | null>(null);
  let askReplace = $state<{
    sessionId: string;
    path: string;
    retarget: boolean;
  } | null>(null);
  let draftTimer: ReturnType<typeof setTimeout> | undefined;
  let lastSaved = "";
  let pinnedDirty = false;
  let closing = false;

  const saveEnabled = $derived(canSave(connected, busy));
  const other = $derived(
    otherSaveTarget({
      editorSessionId: sessionId ?? "",
      editorPath: path,
      peer,
    }),
  );
  const otherEnabled = $derived(canSaveOther(other?.sessionId, busy));
  const otherLabel = $derived(otherSaveLabel(peer?.name));

  function messageOf(error: unknown): string {
    return isAppError(error) ? error.message : "Could not save that file.";
  }

  function contentOf(): string {
    return view?.state.doc.toString() ?? "";
  }

  function scheduleDraft(content: string) {
    dirty = pinnedDirty || (baseline !== null && content !== lastSaved);
    void editorDraft(null, dirty).catch(() => {});
    clearTimeout(draftTimer);
    draftTimer = setTimeout(() => {
      void editorDraft(content, dirty).catch(() => {});
    }, DRAFT_DEBOUNCE_MS);
  }

  async function flushDraft() {
    clearTimeout(draftTimer);
    const content = contentOf();
    dirty = pinnedDirty || (baseline !== null && content !== lastSaved);
    await editorDraft(content, dirty);
  }

  function mountEditor(content: string) {
    if (!host || view) {
      return;
    }
    view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: content,
        extensions: [
          basicSetup,
          EditorView.updateListener.of((update) => {
            if (update.docChanged) {
              scheduleDraft(update.state.doc.toString());
            }
          }),
        ],
      }),
    });
  }

  function applyConnection(next: {
    editorId: string;
    sessionId: string;
    path: string;
    name: string;
    username: string;
    host: string;
    port: number;
    connected: boolean;
  }) {
    editorId = next.editorId;
    sessionId = next.sessionId;
    path = next.path;
    name = next.name;
    endpoint = `${next.username}@${next.host}:${next.port}`;
    connected = next.connected;
  }

  async function load() {
    const state = await editorState(true);
    applyConnection(state);
    const buffer = initialBuffer({
      draft: state.draft,
      dirty: state.dirty,
      baselineSha256: state.baselineSha256,
    });
    if (buffer.readRemote && state.connected) {
      const file = await fsReadText(state.sessionId, state.path);
      const read = applyRemoteRead(file.content, file.sha256);
      baseline = read.baselineSha256;
      lastSaved = read.content;
      dirty = false;
      mountEditor(read.content);
      await editorDraft(read.content, false);
    } else {
      baseline = buffer.baselineSha256;
      lastSaved = buffer.content;
      pinnedDirty = buffer.dirty;
      dirty = buffer.dirty;
      mountEditor(buffer.content);
    }
    status = state.connected
      ? dirty
        ? "Unsaved changes."
        : "Opened."
      : "Disconnected. Edits are kept here. Save is disabled until this server is connected again.";
  }

  async function refreshConnection() {
    if (!editorId) {
      return;
    }
    try {
      const state = await editorState(false);
      const was = connected;
      applyConnection(state);
      if (was && !state.connected) {
        status =
          "Disconnected. Edits are kept here. Save is disabled until this server is connected again.";
      } else if (!was && state.connected && !dirty) {
        status = "Connected.";
      }
    } catch {
      connected = false;
    }
  }

  async function requestPeer() {
    if (!editorId) {
      return;
    }
    await emit("editor://request-peer", { editorId });
  }

  async function noteSavedState(
    writtenSha256: string,
    written: string,
  ): Promise<boolean> {
    baseline = writtenSha256;
    pinnedDirty = false;
    const current = contentOf();
    const settled = settleSave(written, current);
    lastSaved = settled.lastSaved;
    dirty = settled.dirty;
    await editorDraft(current, dirty);
    return settled.dirty;
  }

  async function save(force = false): Promise<boolean> {
    if (busy) {
      return false;
    }
    const plan = planSave({
      connected,
      sessionId,
      path,
      baselineSha256: baseline,
      force,
    });
    if (plan.action === "blocked") {
      status = plan.message;
      return false;
    }
    busy = true;
    status = "Saving…";
    try {
      await flushDraft();
      const content = contentOf();
      const options = plan.expectedSha256
        ? { overwrite: true, expectedSha256: plan.expectedSha256 }
        : { overwrite: true };
      const written = await fsWrite(
        plan.sessionId,
        plan.path,
        content,
        options,
      );
      const noted = await editorNoteSaved(written.sha256, plan.path);
      if (noted.retargeted) {
        path = noted.path;
        const stillDirty = await noteSavedState(written.sha256, content);
        status = stillDirty
          ? "Saved. Edits typed during the save are not saved yet."
          : "Saved.";
        askConflict = false;
        return !stillDirty;
      }
      status = "Saved. This window stays on the original file.";
      return false;
    } catch (error) {
      if (isAppError(error) && error.code === "conflict" && !force) {
        askConflict = true;
        status = error.message;
      } else {
        status = messageOf(error);
      }
      return false;
    } finally {
      busy = false;
    }
  }

  async function writeElsewhere(
    targetSession: string,
    targetPath: string,
    overwrite: boolean,
    retarget: boolean,
  ): Promise<boolean> {
    if (
      sessionId &&
      sameFile(
        { sessionId, path },
        { sessionId: targetSession, path: targetPath },
      )
    ) {
      return save(overwrite);
    }
    busy = true;
    status = "Saving…";
    try {
      await flushDraft();
      const content = contentOf();
      const written = await fsWrite(targetSession, targetPath, content, {
        overwrite,
      });
      if (retarget) {
        const noted = await editorNoteSaved(written.sha256, targetPath);
        if (noted.retargeted) {
          path = noted.path;
          const stillDirty = await noteSavedState(written.sha256, content);
          status = stillDirty
            ? "Saved. Edits typed during the save are not saved yet."
            : "Saved.";
        } else {
          status = "Saved. This window stays on the original file.";
        }
      } else {
        status = "Saved to the other pane.";
      }
      pathPrompt = null;
      askReplace = null;
      return true;
    } catch (error) {
      if (isAppError(error) && error.code === "already_exists" && !overwrite) {
        askReplace = { sessionId: targetSession, path: targetPath, retarget };
        status = error.message;
      } else {
        status = messageOf(error);
      }
      return false;
    } finally {
      busy = false;
    }
  }

  function startSaveAs() {
    const planned = planRemoteWrite({ sessionId, path });
    if (!planned.ok) {
      status = planned.message;
      return;
    }
    pathPrompt = {
      kind: "save-as",
      path: planned.path,
      sessionId: planned.sessionId,
    };
  }

  function startSaveOther() {
    if (!other) {
      status = "The other pane is not connected.";
      return;
    }
    pathPrompt = {
      kind: "other",
      path: other.path,
      sessionId: other.sessionId,
    };
  }

  function submitPath(event: SubmitEvent) {
    event.preventDefault();
    if (!pathPrompt) {
      return;
    }
    const planned = planRemoteWrite({
      sessionId: pathPrompt.sessionId,
      path: pathPrompt.path,
    });
    if (!planned.ok) {
      status = planned.message;
      return;
    }
    void writeElsewhere(
      planned.sessionId,
      planned.path,
      false,
      pathPrompt.kind === "save-as",
    );
  }

  async function discard() {
    askClose = false;
    closing = true;
    await editorDiscard();
  }

  async function saveAndClose() {
    const clean = await save(false);
    if (clean) {
      await discard();
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      if (event.shiftKey) {
        startSaveAs();
      } else {
        void save(false);
      }
    }
  }

  onMount(() => {
    let stopped = false;
    const timers: ReturnType<typeof setInterval>[] = [];
    const stops: Array<() => void> = [];
    void load().catch((error) => {
      status = messageOf(error);
    });
    void getCurrentWindow()
      .onCloseRequested(async (event) => {
        if (closing) {
          return;
        }
        let allow = false;
        try {
          allow = await editorAllowClose();
        } catch {
          allow = !dirty;
        }
        if (allow) {
          return;
        }
        try {
          await flushDraft();
        } catch {
          // A failed mirror still keeps the prompt.
        }
        event.preventDefault();
        askClose = true;
      })
      .then((stop) => {
        if (stopped) {
          stop();
        } else {
          stops.push(stop);
        }
      });
    void listen("editor://confirm-close", () => {
      void flushDraft()
        .catch(() => {})
        .then(() => {
          if (dirty) {
            askClose = true;
          } else {
            void discard();
          }
        });
    }).then((stop) => {
      if (stopped) {
        stop();
      } else {
        stops.push(stop);
      }
    });
    void listen<QuitPrompt>("editor://confirm-quit", (event) => {
      askQuit = event.payload;
    }).then((stop) => {
      if (stopped) {
        stop();
      } else {
        stops.push(stop);
      }
    });
    void listen<EditorPeer>("editor://peer", (event) => {
      peer = event.payload;
    }).then((stop) => {
      if (stopped) {
        stop();
      } else {
        stops.push(stop);
      }
    });
    timers.push(
      setInterval(() => {
        void refreshConnection();
        void requestPeer();
      }, 2000),
    );
    return () => {
      stopped = true;
      for (const timer of timers) {
        clearInterval(timer);
      }
      for (const stop of stops) {
        stop();
      }
      clearTimeout(draftTimer);
      view?.destroy();
      view = null;
    };
  });
</script>

<svelte:window onkeydown={onKeydown} />

<section class="editor-window">
  <header>
    <div>
      <strong>{name || "Editor"}</strong>
      <span>{endpoint}</span>
    </div>
    <p class="path">{path}</p>
  </header>
  <div class="actions">
    <button
      type="button"
      disabled={!saveEnabled}
      onclick={() => void save(false)}
    >
      Save
    </button>
    <button type="button" disabled={!saveEnabled} onclick={startSaveAs}>
      Save as
    </button>
    <button type="button" disabled={!otherEnabled} onclick={startSaveOther}>
      {otherLabel}
    </button>
    <span class="mark"
      >{dirty ? "Unsaved" : connected ? "Saved" : "Offline"}</span
    >
  </div>

  {#if askConflict}
    <div class="confirm" role="group" aria-label="Confirm replace">
      <p>The file changed on the server. Replace it?</p>
      <button type="button" disabled={busy} onclick={() => void save(true)}>
        Replace
      </button>
      <button
        type="button"
        disabled={busy}
        onclick={() => (askConflict = false)}
      >
        Cancel
      </button>
    </div>
  {/if}

  {#if pathPrompt}
    <form class="prompt" onsubmit={submitPath}>
      <label>
        {pathPrompt.kind === "save-as" ? "Save as" : "Save to other pane"}
        <input
          bind:value={pathPrompt.path}
          spellcheck="false"
          autocomplete="off"
          autocapitalize="off"
          disabled={busy}
        />
      </label>
      <button type="submit" disabled={busy}>Save</button>
      <button type="button" disabled={busy} onclick={() => (pathPrompt = null)}>
        Cancel
      </button>
    </form>
  {/if}

  {#if askReplace}
    <div class="confirm" role="group" aria-label="Confirm replace">
      <p>That file already exists. Replace it?</p>
      <button
        type="button"
        disabled={busy}
        onclick={() => {
          if (askReplace) {
            void writeElsewhere(
              askReplace.sessionId,
              askReplace.path,
              true,
              askReplace.retarget,
            );
          }
        }}
      >
        Replace
      </button>
      <button type="button" disabled={busy} onclick={() => (askReplace = null)}>
        Cancel
      </button>
    </div>
  {/if}

  {#if askClose}
    <div class="confirm" role="group" aria-label="Unsaved edits">
      <p>This file has unsaved edits.</p>
      <button
        type="button"
        disabled={!saveEnabled}
        onclick={() => void saveAndClose()}
      >
        Save
      </button>
      <button type="button" disabled={busy} onclick={() => void discard()}>
        Don't save
      </button>
      <button
        type="button"
        onclick={() => {
          askClose = false;
          void editorCancelClose();
        }}
      >
        Cancel
      </button>
    </div>
  {/if}

  {#if askQuit}
    <div class="confirm" role="group" aria-label="Quit with unsaved edits">
      <p>
        Quit without saving {askQuit.count === 1
          ? "1 file"
          : `${askQuit.count} files`}?
      </p>
      <ul>
        {#each askQuit.paths as item (item)}
          <li>{item}</li>
        {/each}
      </ul>
      <button type="button" onclick={() => void editorQuit()}
        >Quit without saving</button
      >
      <button
        type="button"
        onclick={() => {
          askQuit = null;
          void editorCancelQuit();
        }}
      >
        Cancel
      </button>
    </div>
  {/if}

  <div class="host" bind:this={host}></div>
  <p class="status">{status}</p>
</section>

<style>
  .editor-window {
    height: 100vh;
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
    padding: 0.75rem 0.9rem 0.6rem;
    background: Canvas;
    color: CanvasText;
    font-family: system-ui, "Segoe UI", sans-serif;
  }

  header,
  .actions,
  .prompt,
  .confirm {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
  }

  header {
    justify-content: space-between;
  }

  header div {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }

  header span,
  .path,
  .status,
  .mark {
    opacity: 0.75;
  }

  .path,
  .status,
  input {
    font-family: ui-monospace, Consolas, monospace;
  }

  .path,
  .status {
    margin: 0;
    overflow-wrap: anywhere;
  }

  button,
  input {
    font: inherit;
    color: inherit;
  }

  button {
    border: 1px solid color-mix(in srgb, CanvasText 24%, transparent);
    background: color-mix(in srgb, Canvas 88%, CanvasText);
    border-radius: 6px;
    padding: 0.35rem 0.65rem;
  }

  button:disabled {
    opacity: 0.55;
  }

  .mark {
    margin-left: auto;
  }

  .prompt,
  .confirm {
    border: 1px solid color-mix(in srgb, CanvasText 28%, transparent);
    border-radius: 8px;
    padding: 0.7rem;
  }

  .confirm {
    align-items: flex-start;
    flex-direction: column;
  }

  .confirm p,
  .confirm ul {
    margin: 0;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    flex: 1;
    min-width: 12rem;
  }

  input {
    border: 1px solid color-mix(in srgb, CanvasText 24%, transparent);
    border-radius: 6px;
    background: Field;
    color: FieldText;
    padding: 0.4rem 0.5rem;
  }

  .host {
    flex: 1;
    min-height: 0;
    border: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
    overflow: hidden;
  }

  .host :global(.cm-editor) {
    height: 100%;
  }

  .host :global(.cm-scroller) {
    overflow: auto;
    font-family: ui-monospace, Consolas, monospace;
  }
</style>
