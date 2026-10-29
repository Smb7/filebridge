<script lang="ts">
  import { onMount } from "svelte";
  import {
    blankToNull,
    connectionsDelete,
    connectionsList,
    connectionsSave,
    isAppError,
    keyPathForMethod,
    type AuthMethod,
    type Connection,
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
  }

  async function refresh() {
    connections = await connectionsList();
  }

  function showError(error: unknown) {
    errorMessage = isAppError(error) ? error.message : "The request failed.";
  }

  onMount(() => {
    refresh().catch(showError);
  });

  async function save(event: SubmitEvent) {
    event.preventDefault();
    errorMessage = "";
    busy = true;
    try {
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
      await refresh();
      fill(saved);
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

  <form onsubmit={save}>
    <h2>{selectedId ? "Edit connection" : "New connection"}</h2>
    <p class="hint">Passwords and private keys are not saved in this step.</p>

    <label>
      Name
      <input bind:value={name} autocomplete="off" required />
    </label>
    <label>
      Host
      <input bind:value={host} autocomplete="off" spellcheck="false" required />
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
      {#if selectedId}
        <button type="button" class="danger" disabled={busy} onclick={remove}
          >Delete</button
        >
      {/if}
    </div>
  </form>
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
