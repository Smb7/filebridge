<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import FilePane from "$lib/components/FilePane.svelte";
  import {
    bindHostPrompt,
    blankToNull,
    connectionsDelete,
    connectionsList,
    connectionsSave,
    hostKeyPromptCopy,
    hostkeyForget,
    hostkeyList,
    isAppError,
    isHostKeyStop,
    keyPathForMethod,
    planCredentialWrite,
    promptFromConnectError,
    secretFromFields,
    sessionConnect,
    sessionDisconnect,
    sessionForConnection,
    sessionList,
    sessionTrustHost,
    vaultClearCredential,
    vaultHasCredential,
    vaultInitPassword,
    vaultSetCredential,
    vaultStatus,
    vaultUnlock,
    type AuthMethod,
    type BoundHostPrompt,
    type Connection,
    type HostKeyPrompt,
    type LiveSession,
    type TrustedHost,
    type VaultMode,
  } from "$lib/api";
  import { startingPath } from "$lib/browser";
  import {
    PANE_SIDES,
    assignConnection,
    clearConnection,
    createDualPane,
    dropMissingConnections,
    focusPane,
    otherSide,
    paneLabel,
    pasteTarget,
    setDirectory,
    type PaneSide,
  } from "$lib/panes";

  const methods: { value: AuthMethod; label: string }[] = [
    { value: "password", label: "Password" },
    { value: "private_key", label: "Stored private key" },
    { value: "key_file", label: "Key file on this computer" },
    { value: "agent", label: "SSH agent" },
  ];

  let connections = $state<Connection[]>([]);
  let selectedId = $state<string | null>(null);
  let name = $state("");
  let host = $state("");
  let port = $state(22);
  let username = $state("");
  let authMethod = $state<AuthMethod>("password");
  let keyPath = $state("");
  let defaultPath = $state("");
  let notes = $state("");
  let password = $state("");
  let privateKeyPem = $state("");
  let passphrase = $state("");
  let hasCredential = $state(false);
  let savedMethod = $state<AuthMethod | null>(null);
  let vaultMode = $state<VaultMode>("uninitialized");
  let locked = $state(true);
  let masterPassword = $state("");
  let confirmPassword = $state("");
  let errorMessage = $state("");
  let busy = $state(false);
  let trustedHosts = $state<TrustedHost[]>([]);
  let hostPrompt = $state<BoundHostPrompt | null>(null);
  let sessions = $state<LiveSession[]>([]);
  let connectingId = $state<string | null>(null);
  let panes = $state(createDualPane());
  let focusToken = $state({ left: 0, right: 0 });

  const paste = $derived(
    pasteTarget({
      focused: panes.focused,
      left: {
        connectionId: panes.left.connectionId,
        directory: panes.left.directory,
        sessionId: sessionIdFor(panes.left.connectionId),
      },
      right: {
        connectionId: panes.right.connectionId,
        directory: panes.right.directory,
        sessionId: sessionIdFor(panes.right.connectionId),
      },
    }),
  );

  function sessionIdFor(id: string | null): string | null {
    if (!id) {
      return null;
    }
    return sessionForConnection(sessions, id)?.sessionId ?? null;
  }

  function connectionFor(id: string | null): Connection | null {
    if (!id) {
      return null;
    }
    return connections.find((connection) => connection.id === id) ?? null;
  }

  function sessionFor(id: string | null): LiveSession | null {
    if (!id) {
      return null;
    }
    return sessionForConnection(sessions, id) ?? null;
  }

  function activate(side: PaneSide) {
    panes = focusPane(panes, side);
  }

  function focusOther(side: PaneSide) {
    const next = otherSide(side);
    panes = focusPane(panes, next);
    focusToken = { ...focusToken, [next]: focusToken[next] + 1 };
  }

  function rememberDirectory(side: PaneSide, path: string) {
    const next = setDirectory(panes, side, path);
    if (next !== panes) {
      panes = next;
    }
  }

  function choosePane(side: PaneSide, id: string) {
    const connection = connections.find((item) => item.id === id);
    if (!connection) {
      return;
    }
    panes = assignConnection(
      focusPane(panes, side),
      side,
      connection.id,
      startingPath(connection.defaultPath),
    );
    fill(connection);
  }

  function chooseFocused(connection: Connection) {
    panes = assignConnection(
      panes,
      panes.focused,
      connection.id,
      startingPath(connection.defaultPath),
    );
    fill(connection);
  }

  function fill(connection: Connection) {
    selectedId = connection.id;
    name = connection.name;
    host = connection.host;
    port = connection.port;
    username = connection.username;
    authMethod = connection.authMethod;
    keyPath = connection.keyPath ?? "";
    defaultPath = connection.defaultPath ?? "";
    notes = connection.notes ?? "";
    savedMethod = connection.authMethod;
    password = "";
    privateKeyPem = "";
    passphrase = "";
    void loadCredential(connection.id);
  }

  async function loadCredential(id: string) {
    if (locked) {
      hasCredential = false;
      return;
    }
    try {
      hasCredential = await vaultHasCredential(id);
    } catch {
      hasCredential = false;
    }
  }

  function resetForm() {
    selectedId = null;
    name = "";
    host = "";
    port = 22;
    username = "";
    authMethod = "password";
    keyPath = "";
    defaultPath = "";
    notes = "";
    password = "";
    privateKeyPem = "";
    passphrase = "";
    hasCredential = false;
    savedMethod = null;
  }

  async function refresh() {
    connections = await connectionsList();
    trustedHosts = await hostkeyList();
    sessions = await sessionList();
    panes = dropMissingConnections(
      panes,
      connections.map((connection) => connection.id),
    );
    if (
      selectedId &&
      !connections.some((connection) => connection.id === selectedId)
    ) {
      resetForm();
    }
  }

  function showError(error: unknown) {
    errorMessage = isAppError(error) ? error.message : "The request failed.";
  }

  async function refreshVault() {
    let unlockError: unknown = null;
    try {
      await vaultUnlock(null);
    } catch (error) {
      if (!isAppError(error) || error.code !== "password_required") {
        unlockError = error;
      }
    }
    const status = await vaultStatus();
    vaultMode = status.mode;
    locked = status.locked;
    if (unlockError && locked) {
      showError(unlockError);
    }
  }

  onMount(() => {
    const unlisten = listen<HostKeyPrompt>("hostkey://prompt", (event) => {
      const bound = bindHostPrompt(connectingId, event.payload);
      if (bound) {
        hostPrompt = bound;
      }
    });
    refresh()
      .then(() => refreshVault())
      .catch(showError);
    return () => {
      void unlisten.then((stop) => stop());
    };
  });

  async function confirmHostKey() {
    if (!hostPrompt) {
      return;
    }
    const prompt = hostPrompt;
    errorMessage = "";
    busy = true;
    try {
      await sessionTrustHost(prompt.connectionId, prompt.fingerprint);
      hostPrompt = null;
      await sessionConnect(prompt.connectionId);
      if (connectingId === prompt.connectionId) {
        connectingId = null;
      }
      await refresh();
    } catch (error) {
      const next = promptFromConnectError(prompt.connectionId, error);
      if (next) {
        hostPrompt = next;
        connectingId = prompt.connectionId;
      } else if (!isAppError(error) || !isHostKeyStop(error.code)) {
        connectingId = null;
        showError(error);
      } else if (!hostPrompt) {
        showError(error);
      }
    } finally {
      busy = false;
    }
  }

  function cancelHostKey() {
    hostPrompt = null;
    connectingId = null;
  }

  async function connectConnection(connectionId: string) {
    if (!connectionId || connectingId || hostPrompt) {
      return;
    }
    errorMessage = "";
    connectingId = connectionId;
    try {
      await sessionConnect(connectionId);
      if (connectingId === connectionId) {
        connectingId = null;
      }
      hostPrompt = null;
      await refresh();
    } catch (error) {
      const prompt = promptFromConnectError(connectionId, error);
      if (prompt) {
        hostPrompt = prompt;
        return;
      }
      if (connectingId === connectionId) {
        connectingId = null;
      }
      showError(error);
    }
  }

  async function disconnectConnection(connectionId: string) {
    const live = sessionForConnection(sessions, connectionId);
    if (!live) {
      return;
    }
    errorMessage = "";
    busy = true;
    try {
      await sessionDisconnect(live.sessionId);
      await refresh();
    } catch (error) {
      showError(error);
    } finally {
      busy = false;
    }
  }

  async function forgetHost(id: string) {
    errorMessage = "";
    busy = true;
    try {
      await hostkeyForget(id);
      await refresh();
    } catch (error) {
      showError(error);
    } finally {
      busy = false;
    }
  }

  async function unlock(event: SubmitEvent) {
    event.preventDefault();
    errorMessage = "";
    busy = true;
    try {
      if (vaultMode === "keychain") {
        await vaultUnlock(null);
      } else if (vaultMode === "uninitialized") {
        if (masterPassword !== confirmPassword) {
          errorMessage = "Passwords do not match.";
          return;
        }
        await vaultInitPassword(masterPassword);
        vaultMode = "password";
      } else {
        await vaultUnlock(masterPassword);
      }
      locked = false;
      masterPassword = "";
      confirmPassword = "";
      if (selectedId) {
        await loadCredential(selectedId);
      }
    } catch (error) {
      if (isAppError(error) && error.code === "already_initialized") {
        vaultMode = "password";
        errorMessage =
          "The vault is already set up. Enter the master password to unlock.";
        return;
      }
      if (isAppError(error) && error.code === "keychain_available") {
        await refreshVault();
        return;
      }
      showError(error);
    } finally {
      busy = false;
    }
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    errorMessage = "";
    busy = true;
    try {
      const secret = secretFromFields(
        authMethod,
        password,
        privateKeyPem,
        passphrase,
      );
      const methodChanged = savedMethod !== null && savedMethod !== authMethod;
      const plan = planCredentialWrite({
        locked,
        methodChanged,
        authMethod,
        hasSecret: secret !== null,
      });
      if (plan.action === "refuse") {
        errorMessage = plan.message;
        return;
      }
      const creating = selectedId === null;
      const saved = await connectionsSave({
        id: selectedId ?? undefined,
        name,
        host,
        port,
        username,
        authMethod,
        keyPath: keyPathForMethod(authMethod, keyPath),
        defaultPath: blankToNull(defaultPath),
        notes: blankToNull(notes),
      });
      selectedId = saved.id;
      savedMethod = saved.authMethod;
      await refresh();
      if (plan.action === "clear") {
        await vaultClearCredential(saved.id).catch((error) => {
          if (!isAppError(error) || error.code !== "not_found") {
            throw error;
          }
        });
      } else if (plan.action === "set" && secret) {
        await vaultSetCredential(saved.id, secret);
      }
      password = "";
      privateKeyPem = "";
      passphrase = "";
      fill(saved);
      if (creating && panes[panes.focused].connectionId === null) {
        panes = assignConnection(
          panes,
          panes.focused,
          saved.id,
          startingPath(saved.defaultPath),
        );
      }
    } catch (error) {
      showError(error);
    } finally {
      busy = false;
    }
  }

  async function clearSecret() {
    if (!selectedId) {
      return;
    }
    errorMessage = "";
    busy = true;
    try {
      await vaultClearCredential(selectedId);
      hasCredential = false;
      password = "";
      privateKeyPem = "";
      passphrase = "";
    } catch (error) {
      showError(error);
    } finally {
      busy = false;
    }
  }

  async function remove() {
    if (!selectedId) {
      return;
    }
    const label = name.trim() || host.trim() || "this connection";
    if (!confirm(`Delete ${label}? This does not change the server.`)) {
      return;
    }
    const id = selectedId;
    errorMessage = "";
    busy = true;
    try {
      await connectionsDelete(id);
      panes = clearConnection(panes, id);
      resetForm();
      await refresh();
    } catch (error) {
      showError(error);
    } finally {
      busy = false;
    }
  }
</script>

<main>
  <section class="list" aria-label="Saved connections">
    <header>
      <h1>FileBridge</h1>
      <button type="button" onclick={resetForm}>New</button>
    </header>
    {#if connections.length === 0}
      <p class="empty">No saved connections.</p>
    {:else}
      <ul>
        {#each connections as connection (connection.id)}
          <li>
            <button
              type="button"
              class:selected={connection.id === selectedId}
              onclick={() => chooseFocused(connection)}
            >
              <span>{connection.name}</span>
              <small>
                {connection.username}@{connection.host}:{connection.port}
                {#if sessionForConnection(sessions, connection.id)}
                  · Connected
                {/if}
              </small>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <div class="workspace">
    {#if hostPrompt}
      <div
        class="prompt"
        class:changed={hostPrompt.decision === "changed"}
        role="alert"
      >
        <h2>{hostKeyPromptCopy(hostPrompt.decision).title}</h2>
        <p>
          {hostPrompt.host}:{hostPrompt.port} · {hostPrompt.keyType}
        </p>
        <p class="fingerprint">{hostPrompt.fingerprint}</p>
        {#if hostPrompt.storedFingerprint}
          <p class="hint">
            Previously trusted: {hostPrompt.storedFingerprint}
          </p>
        {/if}
        <div class="actions">
          <button type="button" disabled={busy} onclick={confirmHostKey}>
            {hostKeyPromptCopy(hostPrompt.decision).confirm}
          </button>
          <button type="button" disabled={busy} onclick={cancelHostKey}
            >Cancel</button
          >
        </div>
      </div>
    {/if}
    {#if errorMessage}
      <p class="error banner" role="alert">{errorMessage}</p>
    {/if}
    {#if locked}
      <form class="vault" onsubmit={unlock}>
        <h2>
          {vaultMode === "uninitialized"
            ? "Create a vault password"
            : vaultMode === "keychain"
              ? "Keychain unavailable"
              : "Unlock vault"}
        </h2>
        <p class="hint">
          {vaultMode === "keychain"
            ? "The system keychain could not be opened. Saved secrets stay locked until it is available."
            : "Saved passwords and keys stay encrypted on this computer."}
        </p>
        {#if vaultMode !== "keychain"}
          <label>
            Master password
            <input
              type="password"
              bind:value={masterPassword}
              autocomplete={vaultMode === "uninitialized"
                ? "new-password"
                : "current-password"}
              required
            />
          </label>
          {#if vaultMode === "uninitialized"}
            <label>
              Confirm password
              <input
                type="password"
                bind:value={confirmPassword}
                autocomplete="new-password"
                required
              />
            </label>
          {/if}
        {/if}
        <button type="submit" disabled={busy}>
          {vaultMode === "uninitialized"
            ? "Create vault"
            : vaultMode === "keychain"
              ? "Retry"
              : "Unlock"}
        </button>
      </form>
    {/if}
    <div
      class="panes"
      data-focused={panes.focused}
      data-paste-path={paste?.path ?? ""}
      data-paste-session={paste?.sessionId ?? ""}
    >
      {#each PANE_SIDES as side (side)}
        <div class="pane-slot">
          <FilePane
            label={paneLabel(side)}
            focused={panes.focused === side}
            focusToken={focusToken[side]}
            {connections}
            connection={connectionFor(panes[side].connectionId)}
            directory={panes[side].directory}
            session={sessionFor(panes[side].connectionId)}
            connecting={connectingId !== null &&
              connectingId === panes[side].connectionId}
            connectDisabled={busy ||
              connectingId !== null ||
              hostPrompt !== null}
            onActivate={() => activate(side)}
            onFocusOther={() => focusOther(side)}
            onDirectory={(path) => rememberDirectory(side, path)}
            onSelect={(id) => choosePane(side, id)}
            onConnect={() => {
              const id = panes[side].connectionId;
              if (id) {
                void connectConnection(id);
              }
            }}
            onDisconnect={() => {
              const id = panes[side].connectionId;
              if (id) {
                void disconnectConnection(id);
              }
            }}
            onSessionLost={() => {
              void refresh().catch(showError);
            }}
          />
        </div>
      {/each}
    </div>
    <details class="settings" open>
      <summary>Connection settings</summary>
      <section class="hosts">
        <h2>Trusted hosts</h2>
        {#if trustedHosts.length === 0}
          <p class="empty">No host keys trusted yet.</p>
        {:else}
          <ul>
            {#each trustedHosts as host (host.id)}
              <li>
                <span>{host.host}:{host.port}</span>
                <small>{host.fingerprint}</small>
                <button
                  type="button"
                  disabled={busy}
                  onclick={() => forgetHost(host.id)}>Forget</button
                >
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      <form onsubmit={save}>
        <h2>{selectedId ? "Edit connection" : "New connection"}</h2>
        <p class="hint">
          {#if hasCredential}
            A secret is saved. Leave the fields blank to keep it, or type a
            replacement.
          {:else if locked}
            Unlock the vault before saving a password or key.
          {:else}
            Passwords and private keys are encrypted in the local vault.
          {/if}
        </p>

        <label>
          Name
          <input bind:value={name} autocomplete="off" required />
        </label>
        <label>
          Host
          <input
            bind:value={host}
            autocomplete="off"
            spellcheck="false"
            required
          />
        </label>
        <label>
          Port
          <input bind:value={port} type="number" min="1" max="65535" required />
        </label>
        <label>
          Username
          <input
            bind:value={username}
            autocomplete="off"
            spellcheck="false"
            required
          />
        </label>
        <label>
          Authentication
          <select bind:value={authMethod}>
            {#each methods as method (method.value)}
              <option value={method.value}>{method.label}</option>
            {/each}
          </select>
        </label>
        {#if authMethod === "key_file"}
          <label>
            Key file path
            <input bind:value={keyPath} autocomplete="off" spellcheck="false" />
          </label>
        {/if}
        {#if authMethod === "password"}
          <label>
            Password
            <input
              type="password"
              bind:value={password}
              autocomplete="new-password"
              disabled={locked}
              placeholder={hasCredential ? "Saved — type to replace" : ""}
            />
          </label>
        {/if}
        {#if authMethod === "private_key"}
          <label>
            Private key
            <textarea
              bind:value={privateKeyPem}
              rows="6"
              spellcheck="false"
              disabled={locked}
              placeholder={hasCredential
                ? "Saved — paste a new key to replace"
                : ""}
            ></textarea>
          </label>
        {/if}
        {#if authMethod === "private_key" || authMethod === "key_file"}
          <label>
            Key passphrase
            <input
              type="password"
              bind:value={passphrase}
              autocomplete="new-password"
              disabled={locked}
              placeholder={hasCredential ? "Saved — type to replace" : ""}
            />
          </label>
        {/if}
        <label>
          Default remote path
          <input
            bind:value={defaultPath}
            autocomplete="off"
            spellcheck="false"
          />
        </label>
        <label>
          Notes
          <textarea bind:value={notes} rows="4"></textarea>
        </label>

        <div class="actions">
          <button type="submit" disabled={busy}>Save</button>
          {#if selectedId}
            <button
              type="button"
              disabled={busy || connectingId !== null || hostPrompt !== null}
              onclick={() => {
                if (selectedId) {
                  void connectConnection(selectedId);
                }
              }}
            >
              {connectingId === selectedId ? "Connecting…" : "Connect"}
            </button>
          {/if}
          {#if selectedId && sessionForConnection(sessions, selectedId)}
            <button
              type="button"
              disabled={busy}
              onclick={() => {
                if (selectedId) {
                  void disconnectConnection(selectedId);
                }
              }}>Disconnect</button
            >
          {/if}
          {#if selectedId && hasCredential && !locked}
            <button type="button" disabled={busy} onclick={clearSecret}
              >Remove secret</button
            >
          {/if}
          {#if selectedId}
            <button
              type="button"
              class="danger"
              disabled={busy}
              onclick={remove}>Delete</button
            >
          {/if}
        </div>
      </form>
    </details>
  </div>
</main>

<style>
  :root {
    color-scheme: light dark;
    font-family: system-ui, "Segoe UI", sans-serif;
    background: Canvas;
    color: CanvasText;
  }

  :global(body) {
    margin: 0;
  }

  main {
    height: 100vh;
    margin: 0;
    display: grid;
    grid-template-columns: 18rem minmax(0, 1fr);
    overflow: hidden;
  }

  .list {
    border-right: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
    padding: 1rem;
    overflow: auto;
  }

  .workspace {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .panes {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  }

  .pane-slot {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .pane-slot + .pane-slot {
    border-left: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
  }

  .settings {
    flex: 0 1 38%;
    overflow: auto;
    border-top: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
  }

  .settings summary {
    padding: 0.65rem 1rem;
    cursor: pointer;
  }

  header,
  .actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
  }

  h1,
  h2,
  p {
    margin: 0;
  }

  h1 {
    font-size: 1.1rem;
  }

  ul {
    list-style: none;
    margin: 1rem 0 0;
    padding: 0;
  }

  li + li {
    margin-top: 0.35rem;
  }

  button,
  input,
  select,
  textarea {
    font: inherit;
    color: inherit;
  }

  .list button,
  form button {
    border: 1px solid color-mix(in srgb, CanvasText 24%, transparent);
    background: color-mix(in srgb, Canvas 88%, CanvasText);
    border-radius: 6px;
    padding: 0.4rem 0.7rem;
  }

  .list li button {
    width: 100%;
    text-align: left;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }

  .selected {
    outline: 2px solid color-mix(in srgb, CanvasText 55%, transparent);
  }

  small,
  .hint,
  .empty {
    opacity: 0.72;
  }

  .hosts,
  form {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    padding: 1.25rem;
    max-width: 40rem;
  }

  .prompt {
    border: 1px solid color-mix(in srgb, CanvasText 28%, transparent);
    border-radius: 8px;
    padding: 0.8rem;
    margin: 0.75rem 1rem 0;
  }

  .banner {
    margin: 0.75rem 1rem 0;
  }

  .prompt.changed {
    border-color: color-mix(in srgb, red 65%, CanvasText);
  }

  .fingerprint,
  .hosts small {
    font-family: ui-monospace, Consolas, monospace;
    overflow-wrap: anywhere;
  }

  .hosts li {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    margin-bottom: 0.75rem;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  input,
  select,
  textarea {
    border: 1px solid color-mix(in srgb, CanvasText 24%, transparent);
    border-radius: 6px;
    background: Field;
    color: FieldText;
    padding: 0.45rem 0.55rem;
  }

  .error {
    color: color-mix(in srgb, red 70%, CanvasText);
  }

  .danger {
    margin-left: auto;
  }

  @media (max-width: 720px) {
    main {
      height: auto;
      overflow: visible;
      grid-template-columns: 1fr;
    }

    .workspace {
      min-height: 100vh;
    }

    .panes {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: minmax(16rem, 1fr) minmax(16rem, 1fr);
      min-height: 36rem;
    }

    .pane-slot + .pane-slot {
      border-left: 0;
      border-top: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
    }

    .list {
      border-right: 0;
      border-bottom: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
    }
  }
</style>
