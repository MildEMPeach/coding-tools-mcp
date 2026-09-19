<script lang="ts">
  import { goto } from "$app/navigation";
  import { workspaces } from "$lib/stores/app";
  import { showToast } from "$lib/stores/toast";
  import {
    clearWorkspaceGoal,
    createWorkspaceGoal,
    listGoalDashboard,
    pauseWorkspaceGoal,
    resumeWorkspaceGoal,
    type GoalDashboardItem,
    type GoalHealth,
    type GoalStatus,
  } from "$lib/api/monitor";
  import {
    Activity,
    AlertTriangle,
    CheckCircle2,
    CirclePause,
    CirclePlay,
    Plus,
    RefreshCw,
    Target,
    Trash2,
  } from "@lucide/svelte";
  import { confirm } from "@tauri-apps/plugin-dialog";
  import { onDestroy, onMount } from "svelte";

  type ViewFilter = "current" | "history" | "all";

  let items = $state<GoalDashboardItem[]>([]);
  let loading = $state(false);
  let creating = $state(false);
  let filter = $state<ViewFilter>("current");
  let selectedWorkspace = $state("");
  let objective = $state("");
  let autoContinue = $state(true);
  let staleAfterSecs = $state(180);
  let refreshTimer: ReturnType<typeof setInterval> | undefined;

  const currentItems = $derived(
    items.filter((item) => !["completed", "failed", "cleared"].includes(item.goal.status)),
  );
  const historyItems = $derived(
    items.filter((item) => ["completed", "failed", "cleared"].includes(item.goal.status)),
  );
  const visibleItems = $derived(
    filter === "current" ? currentItems : filter === "history" ? historyItems : items,
  );
  const attentionCount = $derived(currentItems.filter((item) => item.goal.needs_attention).length);
  const runningCount = $derived(currentItems.filter((item) => item.goal.health === "running").length);

  $effect(() => {
    if (!selectedWorkspace && $workspaces.length > 0) {
      selectedWorkspace = $workspaces[0].id;
    }
  });

  function statusLabel(status: GoalStatus): string {
    switch (status) {
      case "active": return "进行中";
      case "paused": return "已暂停";
      case "blocked": return "已阻塞";
      case "completed": return "已完成";
      case "failed": return "失败";
      case "cleared": return "已清除";
    }
  }

  function healthLabel(health: GoalHealth): string {
    switch (health) {
      case "running": return "正在推进";
      case "idle": return "空闲等待";
      case "stalled": return "疑似停滞";
      case "paused": return "已暂停";
      case "blocked": return "需要处理";
      case "completed": return "完成";
      case "failed": return "失败";
      case "cleared": return "已清除";
    }
  }

  function healthClass(health: GoalHealth): string {
    if (health === "running" || health === "completed") return "text-[var(--success)]";
    if (health === "stalled" || health === "blocked" || health === "failed") return "text-[var(--danger)]";
    return "text-[var(--color-text-muted)]";
  }

  function progress(item: GoalDashboardItem): number {
    const completed = item.goal.completed_steps.length;
    const total = completed + item.goal.pending_steps.length;
    return total > 0 ? Math.round((completed / total) * 100) : 0;
  }

  function formatTime(value: string): string {
    const timestamp = Number(value);
    if (!Number.isFinite(timestamp) || timestamp <= 0) return "—";
    return new Date(timestamp).toLocaleString([], {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  async function refresh() {
    loading = true;
    try {
      items = await listGoalDashboard();
      if (!selectedWorkspace && $workspaces.length > 0) {
        selectedWorkspace = $workspaces[0].id;
      }
    } catch (error) {
      showToast(String(error), { title: "Goal 状态读取失败", kind: "error", duration: 7000 });
    } finally {
      loading = false;
    }
  }

  async function createGoal() {
    const trimmed = objective.trim();
    if (!selectedWorkspace || !trimmed || creating) return;
    creating = true;
    try {
      await createWorkspaceGoal(selectedWorkspace, trimmed, {
        autoContinue,
        staleAfterSecs,
      });
      objective = "";
      showToast("Goal 已创建并绑定 Harness Task", { kind: "success" });
      await refresh();
    } catch (error) {
      showToast(String(error), { title: "创建 Goal 失败", kind: "error", duration: 7000 });
    } finally {
      creating = false;
    }
  }

  async function pause(item: GoalDashboardItem) {
    try {
      await pauseWorkspaceGoal(item.profile_id, item.goal.id);
      await refresh();
    } catch (error) {
      showToast(String(error), { title: "暂停失败", kind: "error" });
    }
  }

  async function resume(item: GoalDashboardItem) {
    try {
      await resumeWorkspaceGoal(item.profile_id, item.goal.id);
      await refresh();
    } catch (error) {
      showToast(String(error), { title: "恢复失败", kind: "error" });
    }
  }

  async function clearGoal(item: GoalDashboardItem) {
    const accepted = await confirm(`清除 Goal「${item.goal.objective}」？历史记录仍会保留。`, {
      title: "清除 Goal",
      kind: "warning",
      okLabel: "清除",
      cancelLabel: "取消",
    });
    if (!accepted) return;
    try {
      await clearWorkspaceGoal(item.profile_id, item.goal.id);
      await refresh();
    } catch (error) {
      showToast(String(error), { title: "清除失败", kind: "error" });
    }
  }

  onMount(() => {
    void refresh();
    refreshTimer = setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, 15_000);
  });

  onDestroy(() => {
    if (refreshTimer) clearInterval(refreshTimer);
  });
</script>

<section class="page-scroll">
  <header class="page-header">
    <div class="flex flex-wrap items-start justify-between gap-4">
      <div>
        <p class="page-kicker">自动化监控</p>
        <h2 class="page-title">目标监控</h2>
        <p class="mt-2 max-w-2xl text-sm leading-6 text-[var(--color-text-secondary)]">
          Goal 是跨轮次的长期目标。Monitor 持续检查 Harness 活动、进度和阻塞状态；ChatGPT 网页端可以继续推进当前 Goal。
        </p>
      </div>
      <button type="button" class="tx-btn-ghost" disabled={loading} onclick={() => void refresh()}>
        <RefreshCw size={15} class={loading ? "animate-spin" : ""} />
        刷新
      </button>
    </div>

    <div class="mt-4 grid gap-3 sm:grid-cols-3">
      <div class="rounded-[10px] border border-[var(--color-border)] p-3">
        <p class="text-[11px] text-[var(--color-text-muted)]">当前 Goal</p>
        <p class="mt-1 text-xl font-semibold tabular-nums">{currentItems.length}</p>
      </div>
      <div class="rounded-[10px] border border-[var(--color-border)] p-3">
        <p class="text-[11px] text-[var(--color-text-muted)]">正在推进</p>
        <p class="mt-1 text-xl font-semibold tabular-nums">{runningCount}</p>
      </div>
      <div class="rounded-[10px] border border-[var(--color-border)] p-3">
        <p class="text-[11px] text-[var(--color-text-muted)]">需要关注</p>
        <p class="mt-1 text-xl font-semibold tabular-nums">{attentionCount}</p>
      </div>
    </div>
  </header>

  <div class="page-body grid gap-5">
    <form
      class="tx-card grid gap-4 p-5"
      onsubmit={(event) => {
        event.preventDefault();
        void createGoal();
      }}
    >
      <div class="flex items-center gap-2">
        <Target size={17} class="text-[var(--primary)]" />
        <p class="text-sm font-semibold">新建长期 Goal</p>
      </div>
      <div class="grid gap-3 lg:grid-cols-[220px_minmax(0,1fr)]">
        <label class="grid gap-1">
          <span class="text-xs text-[var(--color-text-muted)]">工作区</span>
          <select
            class="min-h-11 rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-3 text-sm"
            bind:value={selectedWorkspace}
          >
            {#each $workspaces as workspace}
              <option value={workspace.id}>{workspace.name}</option>
            {/each}
          </select>
        </label>
        <label class="grid gap-1">
          <span class="text-xs text-[var(--color-text-muted)]">目标</span>
          <input
            class="min-h-11 rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-3 text-sm"
            placeholder="例如：修复所有 flaky tests，直到 CI 全部通过"
            bind:value={objective}
          />
        </label>
      </div>
      <div class="flex flex-wrap items-end gap-4">
        <label class="flex min-h-11 items-center gap-2 text-sm">
          <input type="checkbox" bind:checked={autoContinue} />
          <span>持续推进</span>
        </label>
        <label class="grid gap-1">
          <span class="text-xs text-[var(--color-text-muted)]">停滞阈值（秒）</span>
          <input
            type="number"
            min="30"
            max="86400"
            class="min-h-11 w-32 rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-3 text-sm"
            bind:value={staleAfterSecs}
          />
        </label>
        <button
          type="submit"
          class="tx-btn-primary min-h-11"
          disabled={creating || !selectedWorkspace || !objective.trim()}
        >
          <Plus size={15} />
          {creating ? "创建中…" : "创建 Goal"}
        </button>
      </div>
      <p class="text-[11px] leading-5 text-[var(--color-text-muted)]">
        勾选“持续推进”后，Goal 会在 MCP 中持久化；ChatGPT 网页端会在每轮结束前检查 Goal，并可由内嵌 Goal 控件发起下一轮继续执行。
      </p>
    </form>

    <div class="flex flex-wrap items-center gap-2">
      {#each [
        { value: "current", label: `当前 ${currentItems.length}` },
        { value: "history", label: `历史 ${historyItems.length}` },
        { value: "all", label: `全部 ${items.length}` },
      ] as option}
        <button
          type="button"
          class="tx-status-pill"
          class:active={filter === option.value}
          onclick={() => (filter = option.value as ViewFilter)}
        >
          {option.label}
        </button>
      {/each}
    </div>

    {#if visibleItems.length === 0}
      <div class="tx-card p-8 text-center">
        <Target size={24} class="mx-auto text-[var(--color-text-muted)]" />
        <p class="mt-3 text-sm font-medium">暂无 Goal</p>
        <p class="mt-1 text-xs text-[var(--color-text-muted)]">
          可以从上方创建，也可以让 Agent 调用 goal_create。
        </p>
      </div>
    {:else}
      <div class="grid gap-3">
        {#each visibleItems as item (item.goal.id)}
          {@const pct = progress(item)}
          {@const total = item.goal.completed_steps.length + item.goal.pending_steps.length}
          <article class="tx-card p-5">
            <div class="flex flex-wrap items-start justify-between gap-4">
              <div class="min-w-0 flex-1">
                <div class="flex flex-wrap items-center gap-2">
                  {#if item.goal.needs_attention}
                    <AlertTriangle size={15} class="text-[var(--danger)]" />
                  {:else if item.goal.health === "completed"}
                    <CheckCircle2 size={15} class="text-[var(--success)]" />
                  {:else}
                    <Activity size={15} class="text-[var(--primary)]" />
                  {/if}
                  <button
                    type="button"
                    class="truncate text-xs font-medium text-[var(--primary)] hover:underline"
                    onclick={() => goto(`/workspace/${item.profile_id}`)}
                  >
                    {item.workspace_name}
                  </button>
                  <span class="tx-status-pill px-2 py-0.5 text-[11px]">{statusLabel(item.goal.status)}</span>
                  <span class={`text-[11px] ${healthClass(item.goal.health)}`}>
                    {healthLabel(item.goal.health)}
                  </span>
                </div>
                <h3 class="mt-2 break-words text-sm font-semibold text-[var(--color-text)]">
                  {item.goal.objective}
                </h3>
                <p class="mt-1 text-xs leading-5 text-[var(--color-text-muted)]">
                  {item.goal.monitor_message}
                </p>
              </div>

              <div class="flex shrink-0 flex-wrap gap-2">
                {#if item.goal.status === "active"}
                  <button type="button" class="tx-btn-ghost text-xs" onclick={() => void pause(item)}>
                    <CirclePause size={14} />暂停
                  </button>
                {:else if item.goal.status === "paused" || item.goal.status === "blocked"}
                  <button type="button" class="tx-btn-ghost text-xs" onclick={() => void resume(item)}>
                    <CirclePlay size={14} />恢复
                  </button>
                {/if}
                {#if !["completed", "failed", "cleared"].includes(item.goal.status)}
                  <button
                    type="button"
                    class="tx-btn-ghost text-xs text-[var(--danger)]"
                    onclick={() => void clearGoal(item)}
                  >
                    <Trash2 size={14} />清除
                  </button>
                {/if}
              </div>
            </div>

            <div class="mt-4 grid gap-4 lg:grid-cols-[minmax(0,1fr)_260px]">
              <div>
                <div class="flex items-center justify-between text-[11px] text-[var(--color-text-muted)]">
                  <span>进度</span>
                  <span class="tabular-nums">{item.goal.completed_steps.length}/{total || "—"}</span>
                </div>
                <div class="mt-1.5 h-1.5 overflow-hidden rounded-full bg-[var(--surface-hover)]">
                  <div class="h-full rounded-full bg-[var(--primary)]" style={`width:${pct}%`}></div>
                </div>
                {#if item.goal.pending_steps.length > 0}
                  <p class="mt-2 truncate text-xs text-[var(--color-text-secondary)]">
                    下一步：{item.goal.pending_steps[0]}
                  </p>
                {:else if item.goal.note}
                  <p class="mt-2 truncate text-xs text-[var(--color-text-secondary)]">{item.goal.note}</p>
                {/if}
              </div>
              <dl class="grid gap-2 text-[11px]">
                <div class="flex justify-between gap-3">
                  <dt class="text-[var(--color-text-muted)]">最近活动</dt>
                  <dd class="tabular-nums">{formatTime(item.goal.last_activity_at)}</dd>
                </div>
                <div class="flex justify-between gap-3">
                  <dt class="text-[var(--color-text-muted)]">停滞阈值</dt>
                  <dd>{item.goal.stale_after_secs}s</dd>
                </div>
                <div class="flex justify-between gap-3">
                  <dt class="text-[var(--color-text-muted)]">Continuation</dt>
                  <dd>
                    {item.goal.should_continue ? "Ready" : "Off"}
                    {#if item.goal.continuation_count > 0}
                      · {item.goal.continuation_count}
                    {/if}
                  </dd>
                </div>
                <div class="flex justify-between gap-3">
                  <dt class="text-[var(--color-text-muted)]">Task</dt>
                  <dd class="max-w-36 truncate font-mono">{item.goal.task_id ?? "—"}</dd>
                </div>
              </dl>
            </div>

          </article>
        {/each}
      </div>
    {/if}
  </div>
</section>

