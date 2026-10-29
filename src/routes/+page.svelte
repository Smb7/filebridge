<script lang="ts">
  import { onMount } from "svelte";
  import {
    blankToNull,
    connectionsDelete,
    connectionsList,
    connectionsSave,
    isAppError,
    keyPathForMethod,
    planCredentialWrite,
    secretFromFields,
    vaultClearCredential,
    vaultHasCredential,
    vaultInitPassword,
    vaultSetCredential,
    vaultStatus,
    vaultUnlock,
    type AuthMethod,
    type Connection,
    type VaultMode,
  } from "$lib/api";

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

  function clearForm() {
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
    refresh()
      .then(() => refreshVault())
      .catch(showError);
  });

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
    errorMessage = "";
    busy = true;
    try {
      await connectionsDelete(selectedId);
      clearForm();
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
      <button type="button" onclick={clearForm}>New</button>
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
              onclick={() => fill(connection)}
            >
              <span>{connection.name}</span>
              <small
                >{connection.username}@{connection.host}:{connection.port}</small
              >
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <div class="editor">
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
        <input bind:value={defaultPath} autocomplete="off" spellcheck="false" />
      </label>
      <label>
        Notes
        <textarea bind:value={notes} rows="4"></textarea>
      </label>

      {#if errorMessage}
        <p class="error" role="alert">{errorMessage}</p>
      {/if}

      <div class="actions">
        <button type="submit" disabled={busy}>Save</button>
        {#if selectedId && hasCredential && !locked}
          <button type="button" disabled={busy} onclick={clearSecret}
            >Remove secret</button
          >
        {/if}
        {#if selectedId}
          <button type="button" class="danger" disabled={busy} onclick={remove}
            >Delete</button
          >
        {/if}
      </div>
    </form>
  </div>
</main>

<style>
  :root {
    color-scheme: light dark;
    font-family: system-ui, "Segoe UI", sans-serif;
    background: Canvas;
    color: CanvasText;
  }

  main {
    min-height: 100vh;
    margin: 0;
    display: grid;
    grid-template-columns: 18rem minmax(0, 1fr);
  }

  .list {
    border-right: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
    padding: 1rem;
  }

  .editor {
    min-width: 0;
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

  form {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    padding: 1.25rem;
    max-width: 36rem;
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
      grid-template-columns: 1fr;
    }

    .list {
      border-right: 0;
      border-bottom: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
    }
  }
</style>
