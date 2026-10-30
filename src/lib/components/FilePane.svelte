<script lang="ts">
  import { onMount, tick } from "svelte";
  import {
    fsDelete,
    fsList,
    fsMkdir,
    fsRename,
    fsStat,
    isAppError,
    type Connection,
    type LiveSession,
  } from "$lib/api";
  import {
    confirmedDelete,
    deletePrompt,
    neighborName,
    planMkdir,
    planRename,
  } from "$lib/fileOps";
  import {
    FILE_OPEN_LATER,
    ROW_HEIGHT,
    acceptListing,
    applyPathCommit,
    attachSession,
    beginListing,
    childPath,
    clearBrowser,
    createBrowser,
    entryMeta,
    escapePath,
    listKeyAction,
    moveSelection,
    openAction,
    parentPath,
    pathFieldEnabled,
    pathPointer,
    rejectListing,
    scrollForIndex,
    startingPath,
    visibleRange,
    type DirEntry,
  } from "$lib/browser";
  import { paneHeader } from "$lib/panes";

  type Props = {
    label: string;
    focused: boolean;
    focusToken: number;
    connections: Connection[];
    connection: Connection | null;
    session: LiveSession | null;
    connecting: boolean;
    connectDisabled: boolean;
    onActivate: () => void;
    onFocusOther: () => void;
    onDirectory: (path: string) => void;
    onSelect: (id: string) => void;
    onConnect: () => void;
    onDisconnect: () => void;
    onSessionLost: () => void;
  };

  let {
    label,
    focused,
    focusToken,
    connections,
    connection,
    session,
    connecting,
    connectDisabled,
    onActivate,
    onFocusOther,
    onDirectory,
    onSelect,
    onConnect,
    onDisconnect,
    onSessionLost,
  }: Props = $props();

  let browser = $state(createBrowser());
  let note = $state("");
  let seenSession = $state<string | null>(null);
  let scrollTop = $state(0);
  let viewportHeight = $state(320);
  let pathInput = $state<HTMLInputElement | null>(null);
  let viewport = $state<HTMLDivElement | null>(null);
  let paneRoot = $state<HTMLElement | null>(null);
  let appliedFocus = 0;
  let namingMode = $state<"mkdir" | "rename" | null>(null);
  let nameDraft = $state("");
  let namedIn = $state<string | null>(null);
  let pendingDelete = $state<{
    path: string;
    name: string;
    message: string;
  } | null>(null);
  let acting = $state(false);
  let nameInput = $state<HTMLInputElement | null>(null);

  const header = $derived(paneHeader(connection, focused));
  const pathValue = $derived(browser.draft ?? browser.path);
  const range = $derived(
    visibleRange({
      scrollTop,
      viewportHeight,
      count: browser.entries.length,
      itemHeight: ROW_HEIGHT,
    }),
  );
  const visible = $derived(browser.entries.slice(range.start, range.end));
  const status = $derived.by(() => {
    if (acting) {
      return "Working…";
    }
    if (browser.listing) {
      return "Listing…";
    }
    if (browser.error) {
      return browser.error;
    }
    if (note) {
      return note;
    }
    if (!connection) {
      return "Choose a server.";
    }
    if (!session) {
      return "Connect to browse this server.";
    }
    if (browser.entries.length === 0) {
      return "Empty directory.";
    }
    const count = browser.entries.length;
    return count === 1 ? "1 item" : `${count} items`;
  });

  function messageOf(error: unknown): string {
    return isAppError(error) ? error.message : "Could not read that path.";
  }

  function changeMessage(error: unknown): string {
    return isAppError(error) ? error.message : "Could not change that path.";
  }

  function selectedEntry(): DirEntry | null {
    return (
      browser.entries.find((item) => item.name === browser.selectedName) ?? null
    );
  }

  function lostSession(error: unknown) {
    if (
      isAppError(error) &&
      (error.code === "not_connected" || error.code === "sftp_unavailable")
    ) {
      onSessionLost();
    }
  }

  function cancelPrompt() {
    namingMode = null;
    nameDraft = "";
    namedIn = null;
    pendingDelete = null;
  }

  async function focusName() {
    await tick();
    nameInput?.focus();
    nameInput?.select();
  }

  function startMkdir() {
    if (!session || acting) {
      return;
    }
    pendingDelete = null;
    namingMode = "mkdir";
    nameDraft = "";
    namedIn = browser.path;
    void focusName();
  }

  function startRename() {
    const selected = selectedEntry();
    if (!session || !selected || acting) {
      return;
    }
    pendingDelete = null;
    namingMode = "rename";
    nameDraft = selected.name;
    namedIn = browser.path;
    void focusName();
  }

  function startDelete() {
    const selected = selectedEntry();
    if (!session || !selected || acting) {
      return;
    }
    const prompt = deletePrompt(selected);
    namingMode = null;
    if (!prompt.ok) {
      browser = { ...browser, error: prompt.message };
      pendingDelete = null;
      return;
    }
    pendingDelete = {
      path: prompt.path,
      name: selected.name,
      message: prompt.message,
    };
  }

  async function submitName(event?: Event) {
    event?.preventDefault();
    if (!session || !namingMode || acting) {
      return;
    }
    const selected = selectedEntry();
    const planned =
      namingMode === "mkdir"
        ? planMkdir(browser.path, nameDraft)
        : selected
          ? planRename(selected, nameDraft)
          : { ok: false as const, message: "Select a file to rename." };
    if (!planned.ok) {
      browser = { ...browser, error: planned.message };
      return;
    }
    if ("unchanged" in planned && planned.unchanged) {
      cancelPrompt();
      return;
    }
    const generation = browser.generation;
    const sessionId = session.sessionId;
    const directory = browser.path;
    const selectName = planned.name;
    acting = true;
    browser = { ...browser, error: null };
    try {
      if ("path" in planned) {
        await fsMkdir(sessionId, planned.path);
      } else {
        await fsRename(sessionId, planned.from, planned.to);
      }
      cancelPrompt();
      if (
        browser.generation !== generation ||
        session?.sessionId !== sessionId
      ) {
        return;
      }
      browser = { ...browser, selectedName: selectName, error: null };
      await load(sessionId, directory);
    } catch (error) {
      if (browser.generation !== generation) {
        return;
      }
      browser = { ...browser, error: changeMessage(error) };
      lostSession(error);
    } finally {
      acting = false;
    }
  }

  async function confirmDelete() {
    const selected = selectedEntry();
    const path = confirmedDelete(
      pendingDelete?.path ?? null,
      selected?.path ?? null,
    );
    if (!path || !session || acting) {
      pendingDelete = null;
      return;
    }
    const generation = browser.generation;
    const sessionId = session.sessionId;
    const directory = browser.path;
    const next = neighborName(browser.entries, selected?.name ?? "");
    acting = true;
    pendingDelete = null;
    browser = { ...browser, error: null };
    try {
      await fsDelete(sessionId, path);
      if (
        browser.generation !== generation ||
        session?.sessionId !== sessionId
      ) {
        return;
      }
      browser = { ...browser, selectedName: next, error: null };
      await load(sessionId, directory);
    } catch (error) {
      if (browser.generation !== generation) {
        return;
      }
      browser = { ...browser, error: changeMessage(error) };
      lostSession(error);
    } finally {
      acting = false;
    }
  }

  function measure() {
    if (!viewport) {
      return;
    }
    viewportHeight = viewport.clientHeight || viewportHeight;
    scrollTop = viewport.scrollTop;
  }

  async function load(sessionId: string, nextPath: string) {
    const started = beginListing(browser);
    browser = started.state;
    note = "";
    const token = started.generation;
    try {
      const entries = await fsList(sessionId, nextPath);
      browser = acceptListing(browser, token, nextPath, entries);
    } catch (error) {
      browser = rejectListing(browser, token, messageOf(error));
      if (
        isAppError(error) &&
        (error.code === "not_connected" || error.code === "sftp_unavailable")
      ) {
        onSessionLost();
      }
    }
  }

  async function openEntry(entry: DirEntry) {
    if (!session) {
      return;
    }
    const next = childPath(browser.path, entry.name);
    if (!next) {
      return;
    }
    const action = openAction(entry.kind);
    if (action === "list") {
      await load(session.sessionId, next);
      return;
    }
    if (action === "file") {
      note = FILE_OPEN_LATER;
      return;
    }
    const sessionId = session.sessionId;
    const token = browser.generation;
    try {
      const stat = await fsStat(sessionId, next);
      if (token !== browser.generation || session?.sessionId !== sessionId) {
        return;
      }
      if (stat.kind === "dir") {
        await load(sessionId, next);
      } else {
        note = FILE_OPEN_LATER;
      }
    } catch (error) {
      if (token !== browser.generation) {
        return;
      }
      browser = { ...browser, error: messageOf(error) };
    }
  }

  function submitPath() {
    if (!session) {
      return;
    }
    const committed = applyPathCommit(browser, browser.draft ?? browser.path);
    browser = committed.state;
    if (committed.list) {
      void load(session.sessionId, committed.list);
    }
  }

  function onPathMouseDown(event: MouseEvent) {
    if (!pathInput) {
      return;
    }
    if (pathPointer(document.activeElement === pathInput) === "select-all") {
      event.preventDefault();
      pathInput.focus();
      pathInput.select();
    }
  }

  function onPathKeyDown(event: KeyboardEvent) {
    if (event.key === "Enter") {
      event.preventDefault();
      submitPath();
    } else if (event.key === "Escape") {
      event.preventDefault();
      browser = escapePath(browser);
    }
  }

  function scrollSelection(name: string | null) {
    if (!viewport || !name) {
      return;
    }
    const index = browser.entries.findIndex((item) => item.name === name);
    if (index < 0) {
      return;
    }
    const next = scrollForIndex(
      viewport.scrollTop,
      viewport.clientHeight,
      index,
      ROW_HEIGHT,
    );
    if (next !== viewport.scrollTop) {
      viewport.scrollTop = next;
      scrollTop = next;
    }
  }

  function onNameKey(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      cancelPrompt();
    }
  }

  function onListKey(event: KeyboardEvent) {
    if (namingMode || pendingDelete) {
      if (event.key === "Escape") {
        event.preventDefault();
        cancelPrompt();
      }
      return;
    }
    const action = listKeyAction(event.key, {
      alt: event.altKey,
      ctrl: event.ctrlKey,
      meta: event.metaKey,
      shift: event.shiftKey,
    });
    if (action.type === "focus-other") {
      event.preventDefault();
      onFocusOther();
      return;
    }
    if (action.type === "ignore") {
      return;
    }
    event.preventDefault();
    if (!session) {
      return;
    }
    if (action.type === "move") {
      const name = moveSelection(
        browser.entries,
        browser.selectedName,
        action.delta,
      );
      browser = { ...browser, selectedName: name };
      scrollSelection(name);
      return;
    }
    if (action.type === "parent") {
      const parent = parentPath(browser.path);
      if (parent) {
        void load(session.sessionId, parent);
      }
      return;
    }
    if (action.type === "refresh") {
      void load(session.sessionId, browser.path);
      return;
    }
    if (action.type === "rename") {
      startRename();
      return;
    }
    if (action.type === "delete") {
      startDelete();
      return;
    }
    const selected = browser.entries.find(
      (item) => item.name === browser.selectedName,
    );
    if (selected) {
      void openEntry(selected);
    }
  }

  function selectRow(name: string) {
    browser = { ...browser, selectedName: name };
    viewport?.focus();
  }

  $effect(() => {
    const id = session?.sessionId ?? null;
    const start = startingPath(connection?.defaultPath);
    if (id === seenSession) {
      return;
    }
    const previous = seenSession;
    seenSession = id;
    namingMode = null;
    pendingDelete = null;
    if (!id) {
      if (previous !== null) {
        browser = clearBrowser(browser);
      }
      return;
    }
    browser = attachSession(browser, start);
    void load(id, start);
  });

  $effect(() => {
    onDirectory(browser.path);
  });

  $effect(() => {
    const selected = browser.selectedName;
    const directory = browser.path;
    if (pendingDelete && pendingDelete.name !== selected) {
      pendingDelete = null;
    }
    if (namingMode && namedIn !== directory) {
      namingMode = null;
    }
  });

  $effect(() => {
    const token = focusToken;
    if (token === 0 || token === appliedFocus) {
      return;
    }
    appliedFocus = token;
    void tick().then(() => {
      if (viewport && pathFieldEnabled(session?.sessionId)) {
        viewport.focus();
        return;
      }
      paneRoot?.focus();
    });
  });

  onMount(() => {
    measure();
    const observer = new ResizeObserver(() => measure());
    if (viewport) {
      observer.observe(viewport);
    }
    return () => observer.disconnect();
  });
</script>

<section
  class="pane"
  class:focused
  aria-label={focused ? `${label}, focused` : label}
  data-focused={focused ? "true" : "false"}
  tabindex="-1"
  bind:this={paneRoot}
  onpointerdown={() => onActivate()}
  onfocusin={() => onActivate()}
>
  <header>
    <div class="identity">
      <h2>{header.title}</h2>
      <p>{header.endpoint}</p>
    </div>
    <div class="controls">
      <label>
        Server
        <select
          value={connection?.id ?? ""}
          onchange={(event) => {
            const value = (event.currentTarget as HTMLSelectElement).value;
            if (value) {
              onSelect(value);
            }
          }}
        >
          <option value="" disabled>Choose a server</option>
          {#each connections as item (item.id)}
            <option value={item.id}>{item.name}</option>
          {/each}
        </select>
      </label>
      {#if session}
        <button type="button" disabled={connectDisabled} onclick={onDisconnect}>
          Disconnect
        </button>
      {:else}
        <button
          type="button"
          disabled={!connection || connectDisabled || connecting}
          onclick={onConnect}
        >
          {connecting ? "Connecting…" : "Connect"}
        </button>
      {/if}
    </div>
  </header>

  <label class="path">
    Remote path
    <input
      bind:this={pathInput}
      value={pathValue}
      disabled={!pathFieldEnabled(session?.sessionId)}
      spellcheck="false"
      autocomplete="off"
      autocapitalize="off"
      aria-invalid={browser.pathError !== null}
      onmousedown={onPathMouseDown}
      onkeydown={onPathKeyDown}
      onfocus={() => pathInput?.select()}
      oninput={(event) => {
        browser = {
          ...browser,
          draft: (event.currentTarget as HTMLInputElement).value,
          pathError: null,
        };
      }}
    />
  </label>
  {#if browser.pathError}
    <p class="error" role="alert">{browser.pathError}</p>
  {/if}

  <div class="actions">
    <button type="button" disabled={!session || acting} onclick={startMkdir}>
      New folder
    </button>
    <button
      type="button"
      disabled={!session || !browser.selectedName || acting}
      onclick={startRename}
    >
      Rename
    </button>
    <button
      type="button"
      disabled={!session || !browser.selectedName || acting}
      onclick={startDelete}
    >
      Delete
    </button>
  </div>

  {#if namingMode}
    <form class="prompt" onsubmit={submitName}>
      <label>
        {namingMode === "mkdir" ? "Folder name" : "New name"}
        <input
          bind:this={nameInput}
          bind:value={nameDraft}
          spellcheck="false"
          autocomplete="off"
          autocapitalize="off"
          disabled={acting}
          onkeydown={onNameKey}
        />
      </label>
      <button type="submit" disabled={acting}>
        {namingMode === "mkdir" ? "Create" : "Rename"}
      </button>
      <button type="button" disabled={acting} onclick={cancelPrompt}>
        Cancel
      </button>
    </form>
  {/if}

  {#if pendingDelete}
    <div class="confirm" role="group" aria-label="Confirm delete">
      <p>{pendingDelete.message}</p>
      <button type="button" disabled={acting} onclick={confirmDelete}>
        Delete
      </button>
      <button type="button" disabled={acting} onclick={cancelPrompt}>
        Cancel
      </button>
    </div>
  {/if}

  <div
    class="viewport"
    role="listbox"
    aria-label="Remote files"
    aria-describedby="list-keys"
    tabindex={session ? 0 : -1}
    bind:this={viewport}
    onscroll={measure}
    onkeydown={onListKey}
  >
    <div class="spacer" style:height="{range.totalHeight}px">
      <div class="window" style:transform="translateY({range.offset}px)">
        {#each visible as item, index (item.path)}
          <button
            type="button"
            class="row"
            class:selected={item.name === browser.selectedName}
            role="option"
            aria-selected={item.name === browser.selectedName}
            aria-posinset={range.start + index + 1}
            aria-setsize={browser.entries.length}
            style:height="{ROW_HEIGHT}px"
            tabindex={-1}
            onclick={() => selectRow(item.name)}
            ondblclick={() => {
              selectRow(item.name);
              void openEntry(item);
            }}
          >
            <span class="name">
              {item.name}{item.kind === "dir" ? "/" : ""}
            </span>
            <span class="meta">{entryMeta(item)}</span>
          </button>
        {/each}
      </div>
    </div>
  </div>
  <p class="status" id="list-keys">
    {status}
    <span class="keys">Up/Down, Enter, Backspace, F5, Tab, F2, Delete</span>
  </p>
</section>

<style>
  .pane {
    display: flex;
    flex: 1;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    padding: 0.85rem 1rem 0.6rem;
    gap: 0.55rem;
    outline: 2px solid transparent;
    outline-offset: -2px;
  }

  .pane.focused {
    outline-color: AccentColor;
    box-shadow: inset 3px 0 0 AccentColor;
    background: color-mix(in srgb, AccentColor 7%, Canvas);
  }

  header,
  .controls,
  .path,
  .actions,
  .prompt,
  .confirm,
  .row {
    display: flex;
    gap: 0.75rem;
  }

  header {
    flex-direction: column;
    align-items: stretch;
    gap: 0.45rem;
  }

  .identity {
    min-width: 0;
  }

  h2,
  p {
    margin: 0;
  }

  h2 {
    font-size: 1rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .identity p,
  .status,
  .meta,
  .keys {
    opacity: 0.72;
  }

  .identity p {
    font-family: ui-monospace, Consolas, monospace;
  }

  .controls,
  .path {
    align-items: flex-end;
  }

  .controls label {
    flex: 1;
  }

  .path,
  .controls label {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 0.25rem;
    min-width: 0;
  }

  .path {
    flex: 0 0 auto;
  }

  .actions,
  .prompt,
  .confirm {
    align-items: center;
    flex: 0 0 auto;
    gap: 0.45rem;
  }

  .prompt label {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 0.25rem;
    min-width: 0;
  }

  .confirm p {
    flex: 1;
    min-width: 0;
  }

  .confirm button:first-of-type {
    border-color: color-mix(in srgb, red 55%, CanvasText);
  }

  button,
  input,
  select {
    font: inherit;
    color: inherit;
  }

  button,
  input,
  select {
    border: 1px solid color-mix(in srgb, CanvasText 24%, transparent);
    background: color-mix(in srgb, Canvas 88%, CanvasText);
    border-radius: 6px;
    padding: 0.4rem 0.55rem;
  }

  input,
  select {
    background: Field;
    color: FieldText;
  }

  .path input {
    font-family: ui-monospace, Consolas, monospace;
  }

  .viewport {
    flex: 1;
    min-height: 0;
    overflow: auto;
    border: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
    border-radius: 6px;
  }

  .viewport:focus-visible {
    outline: 2px solid color-mix(in srgb, CanvasText 55%, transparent);
    outline-offset: -2px;
  }

  .spacer {
    position: relative;
    width: 100%;
  }

  .window {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
  }

  .row {
    width: 100%;
    align-items: center;
    justify-content: space-between;
    border: 0;
    border-radius: 0;
    background: transparent;
    text-align: left;
    padding: 0 0.55rem;
  }

  .row.selected {
    background: color-mix(in srgb, CanvasText 12%, Canvas);
  }

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .meta {
    flex: 0 0 auto;
    font-variant-numeric: tabular-nums;
  }

  .status {
    display: flex;
    justify-content: space-between;
    gap: 1rem;
    min-height: 1.2rem;
  }

  .error {
    margin: 0;
    color: color-mix(in srgb, red 70%, CanvasText);
  }
</style>
