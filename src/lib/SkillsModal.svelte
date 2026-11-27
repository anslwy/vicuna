<script lang="ts">
  // Vicuna Skills manager modal: list, toggle, create, edit (opens SKILL.md in
  // CodeMirror), and delete skills stored under ~/vicuna/skills/<slug>/SKILL.md.
  import {
    buildSkillTemplate,
    enabledSkillCount,
    slugifySkillName,
    validateSkillSlug,
    type SkillSummary
  } from "$lib/skills";

  let {
    skills = [],
    loaded,
    onCreate,
    onToggleSkill,
    onDeleteSkill,
    onSetDefault,
    onEdit,
    onClose
  }: {
    skills?: SkillSummary[];
    loaded: boolean;
    onCreate: (slug: string, content: string) => Promise<SkillSummary>;
    onToggleSkill: (slug: string, enabled: boolean) => Promise<void>;
    onDeleteSkill: (slug: string) => Promise<void>;
    onSetDefault: () => Promise<void>;
    onEdit: (skill: SkillSummary) => void;
    onClose: () => void;
  } = $props();

  let savingDefaults = $state(false);
  let defaultFeedback = $state("");
  let defaultTimer: ReturnType<typeof setTimeout>;
  $effect(() => () => clearTimeout(defaultTimer));
  let busySlug = $state<string | null>(null);
  let confirmDeleteSlug = $state<string | null>(null);
  let err = $state<string | null>(null);

  // "+ New skill" inline form state
  let creating = $state(false);
  let newName = $state("");
  let newSlug = $state("");
  let slugTouched = $state(false);
  let newDesc = $state("");
  let creatingBusy = $state(false);

  const enabledCount = $derived(enabledSkillCount(skills));

  async function setDefault() {
    savingDefaults = true;
    defaultFeedback = "";
    err = null;
    clearTimeout(defaultTimer);
    try {
      await onSetDefault();
      defaultFeedback = "Saved as default";
      defaultTimer = setTimeout(() => { defaultFeedback = ""; }, 2500);
    } catch (e) {
      err = e instanceof Error ? e.message : String(e);
    } finally {
      savingDefaults = false;
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      if (creating) {
        creating = false;
        err = null;
      } else {
        onClose();
      }
    }
  }

  function startCreate() {
    creating = true;
    newName = "";
    newSlug = "";
    slugTouched = false;
    newDesc = "";
    err = null;
    confirmDeleteSlug = null;
  }

  function onNameInput(val: string) {
    newName = val;
    if (!slugTouched) {
      newSlug = slugifySkillName(val);
    }
  }

  async function submitCreate(e: SubmitEvent) {
    e.preventDefault();
    err = null;
    const slug = newSlug.trim();
    const slugErr = validateSkillSlug(slug);
    if (slugErr) {
      err = slugErr;
      return;
    }
    if (skills.some((s) => s.slug === slug)) {
      err = `A skill named "${slug}" already exists.`;
      return;
    }
    creatingBusy = true;
    try {
      const content = buildSkillTemplate(newName.trim() || slug, newDesc.trim());
      const created = await onCreate(slug, content);
      creating = false;
      onEdit(created);
    } catch (e: any) {
      err = typeof e === "string" ? e : (e?.message ?? String(e));
    } finally {
      creatingBusy = false;
    }
  }

  async function onToggle(skill: SkillSummary, input: HTMLInputElement) {
    busySlug = skill.slug;
    err = null;
    try {
      await onToggleSkill(skill.slug, !skill.enabled);
    } catch (e: any) {
      input.checked = skill.enabled;
      err = typeof e === "string" ? e : (e?.message ?? String(e));
    } finally {
      busySlug = null;
    }
  }

  async function onDelete(skill: SkillSummary) {
    if (confirmDeleteSlug !== skill.slug) {
      confirmDeleteSlug = skill.slug;
      return;
    }
    busySlug = skill.slug;
    err = null;
    try {
      await onDeleteSkill(skill.slug);
      confirmDeleteSlug = null;
    } catch (e: any) {
      err = typeof e === "string" ? e : (e?.message ?? String(e));
    } finally {
      busySlug = null;
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<button
  type="button"
  class="sk-backdrop"
  aria-label="Close skills"
  onclick={onClose}
></button>

<div
  class="sk-card"
  role="dialog"
  aria-modal="true"
  aria-label="Skills"
  data-testid="skills-modal"
>
  <div class="sk-head">
    <div class="sk-title">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9L12 3z"/>
        <path d="M5 19l1 2.5L8.5 22 6 23l-1 1-1-1-2.5-1L4 21.5 5 19z" opacity="0.7"/>
      </svg>
      <span>Skills</span>
      {#if skills.length > 0}
        <span class="sk-count">{enabledCount} / {skills.length} enabled</span>
      {/if}
    </div>
    <div class="sk-head-actions">
      {#if !creating}
        <button
          type="button"
          class="sk-btn primary small"
          data-testid="new-skill-btn"
          onclick={startCreate}
        >
          + New skill
        </button>
        <button
          type="button"
          class="sk-btn small"
          data-testid="set-default-skills-btn"
          onclick={setDefault}
          disabled={!loaded || savingDefaults || busySlug !== null}
        >
          {savingDefaults ? "Saving…" : "Set default"}
        </button>
      {/if}
      <button
        type="button"
        class="sk-x"
        data-testid="close-skills-btn"
        aria-label="Close skills"
        onclick={onClose}
      >
        ×
      </button>
    </div>
  </div>

  {#if defaultFeedback}
    <div class="sk-muted" role="status">{defaultFeedback}</div>
  {/if}

  {#if err}
    <div class="sk-err" role="alert">{err}</div>
  {/if}

  {#if creating}
    <form class="sk-form" onsubmit={submitCreate} data-testid="new-skill-form">
      <div class="sk-field">
        <label for="sk-name">Name</label>
        <input
          id="sk-name"
          class="sk-input"
          type="text"
          placeholder="e.g. Code Review"
          value={newName}
          oninput={(e) => onNameInput((e.currentTarget as HTMLInputElement).value)}
          required
        />
      </div>
      <div class="sk-field">
        <label for="sk-slug">Slug</label>
        <input
          id="sk-slug"
          class="sk-input mono"
          type="text"
          placeholder="code-review"
          value={newSlug}
          oninput={(e) => {
            slugTouched = true;
            newSlug = (e.currentTarget as HTMLInputElement).value;
          }}
          required
        />
        <span class="sk-path-hint">
          ~/vicuna/skills/{newSlug.trim() || "<slug>"}/SKILL.md
        </span>
      </div>
      <div class="sk-field">
        <label for="sk-desc">Description</label>
        <input
          id="sk-desc"
          class="sk-input"
          type="text"
          placeholder="When should Work agents use this skill?"
          bind:value={newDesc}
        />
      </div>
      <div class="sk-form-foot">
        <button
          type="button"
          class="sk-btn small"
          onclick={() => { creating = false; err = null; }}
          disabled={creatingBusy}
        >
          Cancel
        </button>
        <button
          type="submit"
          class="sk-btn primary small"
          data-testid="create-skill-submit"
          disabled={creatingBusy}
        >
          {creatingBusy ? "Creating…" : "Create & Edit"}
        </button>
      </div>
    </form>
  {/if}

  <div class="sk-list" data-testid="skills-list">
    {#if !loaded}
      <div class="sk-empty">Skills are unavailable until Vicuna connects.</div>
    {:else if skills.length === 0}
      <div class="sk-empty">
        <p>No skills yet.</p>
        <p class="sk-muted">
          Create a skill to store reusable instructions in <code>~/vicuna/skills/</code>. Enabled skills are injected into every Work harness automatically.
        </p>
      </div>
    {:else}
      {#each skills as skill (skill.slug)}
        <div class="sk-row" class:disabled={!skill.enabled} data-testid="skill-row-{skill.slug}">
          <label class="sk-toggle" title={skill.enabled ? "Disable skill" : "Enable skill"}>
            <input
              type="checkbox"
              checked={skill.enabled}
              disabled={busySlug !== null || savingDefaults || creatingBusy}
              onchange={(e) => void onToggle(skill, e.currentTarget)}
              aria-label={`Toggle ${skill.name}`}
            />
            <span class="sk-switch"></span>
          </label>
          <div class="sk-info">
            <div class="sk-name-line">
              <span class="sk-name">{skill.name}</span>
              <code class="sk-slug">{skill.slug}</code>
            </div>
            {#if skill.description}
              <div class="sk-desc">{skill.description}</div>
            {/if}
          </div>
          <div class="sk-actions">
            <button
              type="button"
              class="sk-btn small"
              data-testid="edit-skill-{skill.slug}"
              disabled={busySlug !== null || savingDefaults || creatingBusy}
              onclick={() => onEdit(skill)}
            >
              Edit
            </button>
            <button
              type="button"
              class="sk-btn small danger"
              data-testid="delete-skill-{skill.slug}"
              disabled={busySlug !== null || savingDefaults || creatingBusy}
              onclick={() => void onDelete(skill)}
            >
              {confirmDeleteSlug === skill.slug ? "Confirm?" : "Delete"}
            </button>
          </div>
        </div>
      {/each}
    {/if}
  </div>

  <div class="sk-foot">
    <span class="sk-hint">
      Enabled skills load in this Work session. Set default saves these switches for new sessions. Agents can also manage skills via <code>manage_skills</code>.
    </span>
  </div>
</div>

<style>
  .sk-backdrop { position: fixed; inset: 0; z-index: 60; background: rgba(0, 0, 0, 0.22); border: none; cursor: default; }
  .sk-card {
    position: fixed; left: 50%; top: 50%; transform: translate(-50%, -50%);
    z-index: 61; width: min(580px, calc(100vw - 40px)); max-height: calc(100vh - 64px);
    display: flex; flex-direction: column; gap: 12px;
    background: var(--card); border: 1px solid var(--border); border-radius: 18px;
    box-shadow: 0 24px 80px rgba(0, 0, 0, 0.2); padding: 18px; box-sizing: border-box;
    color: var(--text); font-size: 13.5px;
  }
  .sk-head { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  .sk-title { display: flex; align-items: center; gap: 8px; font-size: 15px; font-weight: 600; min-width: 0; }
  .sk-title svg { flex: none; color: var(--accent); }
  .sk-count { font-size: 12px; font-weight: 500; color: var(--muted); }
  .sk-head-actions { display: flex; align-items: center; gap: 8px; }
  .sk-x {
    width: 28px; height: 28px; border: none; border-radius: 8px; background: transparent;
    color: var(--muted); font-size: 19px; line-height: 1; cursor: pointer;
    display: inline-flex; align-items: center; justify-content: center;
  }
  .sk-x:hover { background: var(--hover-2); color: var(--text); }
  .sk-err {
    color: var(--err-text, var(--danger)); background: var(--err-bg, color-mix(in srgb, var(--danger) 10%, transparent));
    border-radius: 10px; padding: 8px 10px; font-size: 12.5px; overflow-wrap: anywhere;
  }
  .sk-form {
    border: 1px solid var(--border-2); border-radius: 12px; padding: 12px;
    display: flex; flex-direction: column; gap: 10px; background: var(--item-bg);
  }
  .sk-field { display: flex; flex-direction: column; gap: 4px; }
  .sk-field label { font-size: 12px; font-weight: 600; color: var(--text-dim); }
  .sk-input {
    border: 1px solid var(--input-border, var(--border-2)); border-radius: 9px;
    padding: 7px 10px; font: inherit; font-size: 13px;
    background: var(--bg); color: var(--text); box-sizing: border-box; width: 100%;
  }
  .sk-input.mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
  .sk-input:focus { outline: none; border-color: var(--accent); }
  .sk-path-hint { font-size: 11.5px; color: var(--muted); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  .sk-form-foot { display: flex; justify-content: flex-end; gap: 8px; }
  .sk-list { display: flex; flex-direction: column; gap: 8px; overflow-y: auto; min-height: 60px; max-height: 420px; }
  .sk-empty { padding: 18px 12px; text-align: center; color: var(--text-dim); }
  .sk-empty p { margin: 4px 0; }
  .sk-muted { color: var(--muted); font-size: 12.5px; line-height: 1.5; }
  .sk-row {
    display: flex; align-items: flex-start; gap: 10px; padding: 10px 12px;
    border: 1px solid var(--border); border-radius: 12px; background: var(--bg);
    transition: opacity 0.15s;
  }
  .sk-row.disabled { opacity: 0.62; }
  .sk-toggle { position: relative; display: inline-flex; align-items: center; margin-top: 2px; cursor: pointer; flex: none; }
  .sk-toggle input { position: absolute; opacity: 0; width: 0; height: 0; }
  .sk-switch {
    width: 32px; height: 18px; border-radius: 999px; background: var(--border-2);
    position: relative; transition: background 0.15s;
  }
  .sk-switch::after {
    content: ""; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px;
    border-radius: 50%; background: var(--card); transition: transform 0.15s;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
  }
  .sk-toggle input:checked + .sk-switch { background: var(--accent); }
  .sk-toggle input:checked + .sk-switch::after { transform: translateX(14px); }
  .sk-info { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .sk-name-line { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .sk-name { font-weight: 600; font-size: 13.5px; color: var(--text); }
  .sk-slug {
    font-size: 11px; padding: 1px 6px; border-radius: 6px;
    background: var(--chip, var(--item-bg)); color: var(--muted);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .sk-desc { font-size: 12.5px; color: var(--text-dim); line-height: 1.4; overflow-wrap: anywhere; }
  .sk-actions { display: flex; align-items: center; gap: 6px; flex: none; }
  .sk-btn {
    border: 1px solid var(--border-2); border-radius: 9px; padding: 6px 12px;
    font: inherit; font-size: 13px; background: transparent; color: var(--text); cursor: pointer;
  }
  .sk-btn.small { padding: 4px 10px; font-size: 12px; }
  .sk-btn:hover:not(:disabled) { background: var(--hover-2); }
  .sk-btn.primary { background: var(--accent); border-color: var(--accent); color: var(--accent-fg, #fff); font-weight: 600; }
  .sk-btn.primary:hover:not(:disabled) { background: var(--accent-hover, var(--accent)); border-color: var(--accent-hover, var(--accent)); }
  .sk-btn.danger { color: var(--danger); }
  .sk-btn:disabled { opacity: 0.55; cursor: default; }
  .sk-foot { border-top: 1px solid var(--border); padding-top: 10px; }
  .sk-hint { font-size: 12px; color: var(--muted); line-height: 1.45; }
  code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; }
</style>
