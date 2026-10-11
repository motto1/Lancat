<script lang="ts">
  // Settings → 权限与诊断: one row per check (status dot, summary, details, manual help), a fix
  // button where one exists (shield = needs administrator rights, one UAC prompt), a dry-run
  // preview of the commands, and "重新检查". A fix re-checks by itself.
  import { onMount } from "svelte";
  import { api, type DiagCheck, type Diagnostics, type FixInfo, type FixResult } from "../lib/api";

  let d = $state<Diagnostics | null>(null);
  let checking = $state(false);
  let error = $state("");
  let open = $state<Record<string, boolean>>({});
  /** fix id waiting for the user's yes (network type) */
  let confirming = $state<string | null>(null);
  let busy = $state<string | null>(null);
  let result = $state<{ check: string; r: FixResult } | null>(null);

  const STATUS: Record<string, string> = { ok: "正常", warn: "注意", error: "有问题", unknown: "无法检测" };

  export async function recheck() {
    checking = true;
    error = "";
    try {
      d = await api.diagnostics();
    } catch (e) {
      error = String(e);
    } finally {
      checking = false;
    }
  }

  onMount(() => {
    recheck();
  });

  async function fix(c: DiagCheck, f: FixInfo, dryRun: boolean) {
    if (!dryRun && f.confirm && confirming !== f.id) {
      confirming = f.id;
      return;
    }
    confirming = null;
    busy = `${f.id}:${dryRun}`;
    result = null;
    try {
      const r = await api.runFix(f.id, dryRun);
      result = { check: c.id, r };
      if (r.diagnostics) d = r.diagnostics;
    } catch (e) {
      result = { check: c.id, r: { ok: false, dry_run: dryRun, cancelled: false, message: String(e), commands: [] } };
    } finally {
      busy = null;
    }
  }

  let time = $derived(d ? new Date(d.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" }) : "");
</script>

<div class="diag">
  <div class="head">
    <h2>权限与诊断</h2>
    {#if d}
      <span class="sum" class:bad={d.problems > 0}>{d.problems > 0 ? `发现 ${d.problems} 个问题` : "一切正常"}</span>
    {/if}
    <span class="grow"></span>
    {#if time}<span class="when">{time} 检查</span>{/if}
    <button class="btn" disabled={checking} onclick={recheck}>{checking ? "检查中…" : "重新检查"}</button>
  </div>
  {#if error}<p class="note err">{error}</p>{/if}
  {#if !d && !error}<p class="note">正在检查…</p>{/if}
  {#if d}
    {#if d.dry_run_forced}<p class="note">已开启演练模式（LANCAT_FIX_DRY_RUN）：修复只显示将要执行的命令，不做修改。</p>{/if}
    {#each d.checks as c (c.id)}
      <div class="check">
        <div class="line">
          <span class="st {c.status}" title={STATUS[c.status]}></span>
          <div class="text">
            <div class="title">{c.title}<small class="badge {c.status}">{STATUS[c.status]}</small></div>
            <div class="summary">{c.summary}</div>
          </div>
          {#if c.detail.length || c.help}
            <button class="more" class:open={open[c.id]} aria-label="详情" onclick={() => (open[c.id] = !open[c.id])}>
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><path d="M9 6l6 6-6 6" /></svg>
            </button>
          {/if}
        </div>
        {#if open[c.id]}
          <div class="detail">
            {#each c.detail as l}<div class="dl selectable">{l}</div>{/each}
            {#if c.help}<div class="help selectable">{c.help}</div>{/if}
          </div>
        {/if}
        {#if c.fix}
          {@const f = c.fix}
          <div class="actions">
            {#if confirming === f.id}
              <div class="confirm">
                <p>{f.confirm}</p>
                <div class="row-btns">
                  <button class="btn" onclick={() => (confirming = null)}>取消</button>
                  <button class="btn primary" onclick={() => fix(c, f, false)}>确定修改</button>
                </div>
              </div>
            {:else}
              <button class="btn" class:primary={c.status === "error" || c.status === "warn"} disabled={busy !== null} onclick={() => fix(c, f, false)}>
                {#if f.admin}<svg class="shield" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"><path d="M12 3 5 6v5c0 4.5 3 8.3 7 10 4-1.7 7-5.5 7-10V6z" /></svg>{/if}
                {busy === `${f.id}:false` ? "正在修复…" : f.label}
              </button>
              <button class="link" disabled={busy !== null} onclick={() => fix(c, f, true)}>{busy === `${f.id}:true` ? "生成中…" : "预览命令"}</button>
              {#if f.admin}<span class="hint">需要一次管理员授权</span>{/if}
            {/if}
          </div>
        {/if}
        {#if result && result.check === c.id}
          <div class="result" class:ok={result.r.ok && !result.r.dry_run} class:fail={!result.r.ok}>
            <div>{result.r.message}</div>
            {#if result.r.commands.length && result.r.dry_run}
              <pre class="selectable">{result.r.commands.join("\n")}</pre>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
  {/if}
</div>

<style>
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 8px 0 4px;
  }
  .head h2 {
    margin: 0;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-3);
    font-weight: 600;
  }
  .grow {
    flex: 1;
  }
  .sum {
    font-size: 11px;
    color: var(--online);
    font-weight: 500;
  }
  .sum.bad {
    color: var(--danger);
  }
  .when {
    font-size: 10.5px;
    color: var(--text-3);
  }
  .check {
    padding: 8px 0;
    border-bottom: 0.5px solid var(--sep);
  }
  .check:last-child {
    border-bottom: none;
  }
  .line {
    display: flex;
    align-items: flex-start;
    gap: 10px;
  }
  .st {
    flex: none;
    width: 9px;
    height: 9px;
    margin-top: 4px;
    border-radius: 50%;
    background: var(--text-3);
  }
  .st.ok {
    background: var(--online);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--online) 18%, transparent);
  }
  .st.warn {
    background: #ff9f0a;
    box-shadow: 0 0 0 3px color-mix(in srgb, #ff9f0a 20%, transparent);
  }
  .st.error {
    background: var(--danger);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--danger) 18%, transparent);
  }
  .st.unknown {
    background: transparent;
    box-shadow: inset 0 0 0 1.5px var(--text-3);
  }
  .text {
    flex: 1;
    min-width: 0;
  }
  .title {
    font-size: 12.5px;
    font-weight: 500;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .badge {
    font-size: 10px;
    font-weight: 500;
    padding: 0 6px;
    border-radius: 8px;
    line-height: 16px;
    color: var(--text-2);
    background: var(--hover);
  }
  .badge.ok {
    color: color-mix(in srgb, var(--online), var(--text) 30%);
    background: color-mix(in srgb, var(--online) 14%, transparent);
  }
  .badge.warn {
    color: color-mix(in srgb, #ff9f0a, var(--text) 25%);
    background: color-mix(in srgb, #ff9f0a 16%, transparent);
  }
  .badge.error {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 13%, transparent);
  }
  .summary {
    font-size: 11.5px;
    color: var(--text-2);
    line-height: 1.45;
    margin-top: 1px;
  }
  .more {
    flex: none;
    width: 22px;
    height: 22px;
    border-radius: 6px;
    display: grid;
    place-items: center;
    color: var(--text-3);
    transition:
      transform var(--dur) var(--ease),
      background-color var(--dur-fast) var(--ease);
  }
  .more:hover {
    background: var(--hover);
  }
  .more.open {
    transform: rotate(90deg);
  }
  .detail {
    margin: 6px 0 0 19px;
    padding: 6px 9px;
    border-radius: 7px;
    background: var(--hover);
    font-size: 11px;
    line-height: 1.55;
    color: var(--text-2);
  }
  .dl {
    word-break: break-all;
  }
  .help {
    margin-top: 4px;
    color: var(--text);
    white-space: pre-wrap;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 7px 0 0 19px;
  }
  .hint {
    font-size: 10.5px;
    color: var(--text-3);
  }
  .btn {
    padding: 4px 11px;
    border-radius: 6px;
    background-color: var(--glass-strong);
    border: 0.5px solid var(--glass-border);
    font-size: 12px;
    box-shadow: 0 0.5px 1px rgba(0, 0, 0, 0.08);
    display: inline-flex;
    align-items: center;
    gap: 5px;
    transition:
      transform var(--dur-fast) var(--ease),
      background-color var(--dur-fast) var(--ease);
  }
  .btn:hover:not(:disabled) {
    background-color: color-mix(in srgb, var(--glass-strong), var(--text) 7%);
  }
  .btn:active:not(:disabled) {
    transform: scale(0.97);
  }
  .btn:disabled {
    opacity: 0.55;
  }
  .btn.primary {
    background: var(--accent);
    color: var(--accent-contrast);
    border-color: transparent;
  }
  .btn.primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent), #000 8%);
  }
  .shield {
    flex: none;
  }
  .link {
    font-size: 11.5px;
    color: var(--accent);
    padding: 0;
  }
  .link:hover:not(:disabled) {
    text-decoration: underline;
  }
  .confirm {
    flex: 1;
    padding: 8px 10px;
    border-radius: 8px;
    background: color-mix(in srgb, #ff9f0a 12%, transparent);
    border: 0.5px solid color-mix(in srgb, #ff9f0a 40%, transparent);
  }
  .confirm p {
    margin: 0 0 7px;
    font-size: 11.5px;
    line-height: 1.5;
  }
  .row-btns {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .result {
    margin: 7px 0 0 19px;
    padding: 6px 9px;
    border-radius: 7px;
    font-size: 11.5px;
    background: var(--hover);
  }
  .result.ok {
    background: color-mix(in srgb, var(--online) 14%, transparent);
  }
  .result.fail {
    background: color-mix(in srgb, var(--danger) 12%, transparent);
  }
  pre {
    margin: 6px 0 0;
    padding: 6px 8px;
    border-radius: 6px;
    background: var(--glass-strong);
    border: 0.5px solid var(--glass-border);
    font-family: var(--mono);
    font-size: 10.5px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-all;
    user-select: text;
  }
  .note {
    font-size: 11.5px;
    color: var(--text-2);
    line-height: 1.5;
    margin: 6px 0 2px;
  }
  .note.err {
    color: var(--danger);
  }
</style>
