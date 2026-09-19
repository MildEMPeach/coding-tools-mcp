<script lang="ts">
  import {
    Activity,
    AlertTriangle,
    CheckCircle2,
    Circle,
    Clock3,
    GitBranch,
    RefreshCw,
    ShieldCheck,
  } from "@lucide/svelte";
  import { onDestroy, onMount } from "svelte";
  import {
    getHarnessDashboard,
    type HarnessDashboard,
    type HarnessOperation,
    type HarnessTaskStatus,
  } from "$lib/api/harness";

  interface Props {
    workspaceId: string;
  }

  let { workspaceId }: Props = $props();

  let dashboard = $state<HarnessDashboard | null>(null);
  let loading = $state(false);
  let errorMessage = $state("");
  let expandedOperations = $state(false);
  let refreshTimer: ReturnType<typeof setInterval> | undefined;
  let requestGeneration = 0;

  const completedSteps = $derived(dashboard?.task?.completed_steps ?? []);
  const pendingSteps = $derived(dashboard?.task?.pending_steps ?? []);
  const totalSteps = $derived(completedSteps.length + pendingSteps.length);
  const progress = $derived(
    totalSteps > 0 ? Math.round((completedSteps.length / totalSteps) * 100) : 0,
  );

  function taskStatusLabel(status: HarnessTaskStatus | null | undefined): string {
    switch (status) {
      case "active":
        return "进行中";
      case "paused":
        return "已暂停";
      case "verifying":
        return "验证中";
      case "failed":
        return "失败";
      case "completed":
        return "已完成";
      case "completed_unverified":
        return "完成（未验证）";
      case "rolled_back":
        return "已回滚";
      default:
        return "Standalone";
    }
  }

  function capabilityLabel(status: string | undefined): string {
    switch (status) {
      case "available":
        return "可用";
      case "denied":
        return "受限";
      case "degraded":
        return "降级";
      case "managed_by_policy":
        return "策略管理";
      default:
        return status || "未知";
    }
  }

  function operationLabel(operation: HarnessOperation): string {
    if (operation.kind === "failed") return "失败";
    if (operation.kind === "completed") return "完成";
    return "执行中";
  }

  function operationOk(operation: HarnessOperation): boolean {
    if (operation.kind === "failed") return false;
    const ok = operation.result_summary?.ok;
    return ok !== false;
  }

  function formatTime(value: string | null | undefined): string {
    if (!value) return "—";
    const timestamp = Number(value);
    if (!Number.isFinite(timestamp)) return "—";
    return new Date(timestamp).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  }

  function visibleOperations(): HarnessOperation[] {
    if (!dashboard) return [];
    const seen = new Set<string>();
    const unique: HarnessOperation[] = [];
    for (const operation of dashboard.operations) {
      if (seen.has(operation.id)) continue;
      seen.add(operation.id);
      unique.push(operation);
    }
    return unique.slice(0, expandedOperations ? 8 : 4);
  }

  function uniqueOperationCount(): number {
    if (!dashboard) return 0;
    return new Set(dashboard.operations.map((operation) => operation.id)).size;
  }

  async function refresh(id = workspaceId) {
    if (!id) return;
    const generation = ++requestGeneration;
    loading = true;
    try {
      const next = await getHarnessDashboard(id);
      if (generation !== requestGeneration || id !== workspaceId) return;
      dashboard = next;
      errorMessage = "";
    } catch (error) {
      if (generation !== requestGeneration || id !== workspaceId) return;
      errorMessage = String(error);
    } finally {
      if (generation === requestGeneration) loading = false;
    }
  }

  $effect(() => {
    const id = workspaceId;
    dashboard = null;
    errorMessage = "";
    void refresh(id);
  });

  onMount(() => {
    refreshTimer = setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, 10_000);
  });

  onDestroy(() => {
    if (refreshTimer) clearInterval(refreshTimer);
  });
</script>

<section class="tx-card overflow-hidden" aria-labelledby="harness-panel-title">
  <div class="flex flex-wrap items-start justify-between gap-3 border-b border-[var(--color-border)] px-5 py-4">
    <div class="flex min-w-0 items-start gap-3">
      <span
        class="mt-0.5 flex size-9 shrink-0 items-center justify-center rounded-[10px] bg-[var(--primary-soft)] text-[var(--primary)]"
        aria-hidden="true"
      >
        <Activity size={17} />
      </span>
      <div class="min-w-0">
        <div class="flex flex-wrap items-center gap-2">
          <h3 id="harness-panel-title" class="text-sm font-semibold text-[var(--color-text)]">
            Harness
          </h3>
          {#if dashboard}
            <span class="tx-status-pill px-2 py-0.5 text-[11px]">
              {dashboard.status.mode === "task" ? "Task Mode" : "Standalone"}
            </span>
            <span class="text-[11px] text-[var(--color-text-muted)]">
              {taskStatusLabel(dashboard.status.task_state)}
            </span>
          {/if}
        </div>
        <p class="mt-1 text-xs leading-5 text-[var(--color-text-muted)]">
          {#if dashboard?.task}
            {dashboard.task.objective}
          {:else if dashboard}
            当前没有活动任务；简单操作会保持轻量的 standalone 模式。
          {:else}
            正在读取当前工作区 Harness 状态。
          {/if}
        </p>
      </div>
    </div>

    <button
      type="button"
      class="tx-btn-ghost min-h-9 px-2.5 py-1.5 text-xs"
      disabled={loading}
      aria-label="刷新 Harness 状态"
      onclick={() => void refresh()}
    >
      <RefreshCw size={14} class={loading ? "animate-spin" : ""} aria-hidden="true" />
      刷新
    </button>
  </div>

  {#if errorMessage}
    <div class="flex items-start gap-2 px-5 py-4 text-xs text-[var(--danger)]" role="alert">
      <AlertTriangle size={15} class="mt-0.5 shrink-0" aria-hidden="true" />
      <span class="break-all">{errorMessage}</span>
    </div>
  {:else if dashboard}
    <div class="grid gap-5 p-5 lg:grid-cols-[minmax(0,1.35fr)_minmax(240px,0.65fr)]">
      <div class="min-w-0">
        {#if dashboard.task}
          <div class="flex items-center justify-between gap-3">
            <p class="tx-section-label">任务进度</p>
            <span class="text-xs tabular-nums text-[var(--color-text-muted)]">
              {completedSteps.length}/{totalSteps || "—"}
            </span>
          </div>

          {#if totalSteps > 0}
            <div
              class="mt-2 h-1.5 overflow-hidden rounded-full bg-[var(--surface-hover)]"
              role="progressbar"
              aria-valuemin="0"
              aria-valuemax="100"
              aria-valuenow={progress}
              aria-label="任务完成进度"
            >
              <div
                class="h-full rounded-full bg-[var(--primary)] transition-[width] duration-300"
                style={`width: ${progress}%`}
              ></div>
            </div>
          {/if}

          <div class="mt-3 grid gap-2">
            {#each completedSteps as step}
              <div class="flex min-w-0 items-start gap-2 text-xs leading-5 text-[var(--color-text-secondary)]">
                <CheckCircle2 size={14} class="mt-0.5 shrink-0 text-[var(--success)]" aria-hidden="true" />
                <span class="min-w-0 break-words">{step}</span>
              </div>
            {/each}
            {#each pendingSteps as step}
              <div class="flex min-w-0 items-start gap-2 text-xs leading-5 text-[var(--color-text-muted)]">
                <Circle size={14} class="mt-0.5 shrink-0" aria-hidden="true" />
                <span class="min-w-0 break-words">{step}</span>
              </div>
            {/each}
            {#if totalSteps === 0}
              <p class="text-xs leading-5 text-[var(--color-text-muted)]">
                Agent 尚未写入阶段性步骤；任务目标和操作记录仍会持续保留。
              </p>
            {/if}
          </div>
        {:else}
          <div class="rounded-[10px] border border-[var(--color-border)] bg-[var(--surface-hover)] p-3">
            <p class="text-sm font-medium text-[var(--color-text)]">Workspace 可直接开发</p>
            <p class="mt-1 text-xs leading-5 text-[var(--color-text-muted)]">
              当 ChatGPT 开始多文件、多步骤或跨轮次任务时，Harness 会自动切换到 Task Mode 并展示进度。
            </p>
          </div>
        {/if}

        <div class="mt-5">
          <div class="flex items-center justify-between gap-3">
            <p class="tx-section-label">最近操作</p>
            {#if uniqueOperationCount() > 4}
              <button
                type="button"
                class="text-xs text-[var(--primary)] hover:underline"
                onclick={() => (expandedOperations = !expandedOperations)}
              >
                {expandedOperations ? "收起" : "展开"}
              </button>
            {/if}
          </div>

          {#if visibleOperations().length > 0}
            <div class="mt-2 divide-y divide-[var(--color-border)] rounded-[10px] border border-[var(--color-border)]">
              {#each visibleOperations() as operation}
                <div class="flex min-w-0 items-center gap-3 px-3 py-2.5 text-xs">
                  {#if operationOk(operation)}
                    <CheckCircle2 size={14} class="shrink-0 text-[var(--success)]" aria-hidden="true" />
                  {:else}
                    <AlertTriangle size={14} class="shrink-0 text-[var(--danger)]" aria-hidden="true" />
                  {/if}
                  <span class="min-w-0 flex-1 truncate font-mono text-[var(--color-text-secondary)]">
                    {operation.tool}
                  </span>
                  <span class="shrink-0 text-[var(--color-text-muted)]">{operationLabel(operation)}</span>
                  <span class="shrink-0 tabular-nums text-[var(--color-text-muted)]">
                    {formatTime(operation.created_at)}
                  </span>
                </div>
              {/each}
            </div>
          {:else}
            <p class="mt-2 text-xs text-[var(--color-text-muted)]">还没有 Harness 操作记录。</p>
          {/if}
        </div>
      </div>

      <aside class="min-w-0 border-t border-[var(--color-border)] pt-4 lg:border-l lg:border-t-0 lg:pl-5 lg:pt-0">
        <p class="tx-section-label">Workspace</p>
        <dl class="mt-2 grid gap-2.5 text-xs">
          <div class="flex items-center justify-between gap-3">
            <dt class="flex items-center gap-2 text-[var(--color-text-muted)]">
              <GitBranch size={14} aria-hidden="true" />
              Branch
            </dt>
            <dd class="max-w-[60%] truncate font-mono text-[var(--color-text-secondary)]">
              {dashboard.status.branch ?? "—"}
            </dd>
          </div>
          <div class="flex items-center justify-between gap-3">
            <dt class="flex items-center gap-2 text-[var(--color-text-muted)]">
              <ShieldCheck size={14} aria-hidden="true" />
              Baseline
            </dt>
            <dd
              class={dashboard.status.baseline_matches === true
                ? "text-[var(--success)]"
                : dashboard.status.baseline_matches === false
                  ? "text-[var(--danger)]"
                  : "text-[var(--color-text-secondary)]"}
            >
              {dashboard.status.baseline_matches === null
                ? "未跟踪"
                : dashboard.status.baseline_matches
                  ? "Matched"
                  : "Changed"}
            </dd>
          </div>
          <div class="flex items-center justify-between gap-3">
            <dt class="flex items-center gap-2 text-[var(--color-text-muted)]">
              <Activity size={14} aria-hidden="true" />
              Write / Exec
            </dt>
            <dd class="text-[var(--color-text-secondary)]">
              {capabilityLabel(dashboard.status.capabilities.write?.status)} /
              {capabilityLabel(dashboard.status.capabilities.exec?.status)}
            </dd>
          </div>
          <div class="flex items-center justify-between gap-3">
            <dt class="flex items-center gap-2 text-[var(--color-text-muted)]">
              <Clock3 size={14} aria-hidden="true" />
              Updated
            </dt>
            <dd class="tabular-nums text-[var(--color-text-secondary)]">
              {formatTime(dashboard.task?.updated_at ?? dashboard.status.task_updated_at)}
            </dd>
          </div>
        </dl>

        <div class="mt-4 rounded-[10px] bg-[var(--surface-hover)] p-3">
          <p class="text-[11px] font-medium text-[var(--color-text-secondary)]">Harness 状态</p>
          <p class="mt-1 text-[11px] leading-5 text-[var(--color-text-muted)]">
            {dashboard.status.reason}
          </p>
        </div>
      </aside>
    </div>
  {:else}
    <div class="px-5 py-5 text-xs text-[var(--color-text-muted)]">读取中…</div>
  {/if}
</section>
