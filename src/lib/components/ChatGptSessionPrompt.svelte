<script lang="ts">
  import { Check, ChevronDown, Copy, History } from "@lucide/svelte";
  import { onDestroy } from "svelte";
  import { showToast } from "$lib/stores/toast";

  const sessionPrompt = `请初始化或恢复当前项目会话，先调用 history_session_bootstrap，并把我的首次请求逐字传入 initial_user_input。
随后依次调用 harness_status 和 goal_status。若已有 active Goal，先恢复它的目标与 pending_steps；只要 goal_status 返回 should_continue=true，就继续推进，不要因为某个中间步骤完成就提前结束。若准备结束当前 ChatGPT 回合但 Goal 仍未完成，最后调用 goal_handoff，让 ChatGPT 内嵌 Goal 控件请求发送下一条 follow-up message 继续执行。
当我明确要求“长期执行 / 持续推进 / 直到某条件满足”时，使用 goal_create 建立持久 Goal；阶段性进展用 goal_update，同步 completed_steps 和 pending_steps。缺少用户决策或外部资源时用 goal_block；完成项目验证后才使用 goal_complete。
随后调用 harness_status：若已有活动任务，先用 task_context 恢复任务；若没有活动任务，而本次需求包含多文件修改、命令/测试迭代或需要跨轮次追踪，则调用 start_task 创建任务。简单查询或一次性操作可以保持 standalone，不要为了形式强制建任务。
任务推进过程中，在完成阶段性工作或待办变化时调用 update_task；遇到基线不一致时，先用 project_state、operation_log、git_status 和 git_diff 判断外部变化，不要直接覆盖。
任务完成前运行与项目匹配的验证；验证通过后调用 finish_task 并传 verified=true。若环境原因无法验证，只能使用 allow_unverified=true，并明确说明未验证项。
如果没有历史记录，则创建首个 history-session；如果已有历史记录，先阅读返回的有界 state。
需要早期精确细节时，先调用 history_session_search，再用 history_session_read 分页读取相关原始 Markdown，并根据 next_cursor 继续直到完成；不要要求 bootstrap 返回全部历史。
本会话每轮任务完成后调用 history_session_checkpoint，并原样传入 bootstrap 返回的 session_key 和 current_path，以及我本轮请求的逐字 raw_user_input。
只有 checkpoint 返回 ok=true 且会话目标一致后才能确认进度已保存；服务端不能自动读取未通过工具参数传入的对话内容。`;

  let copying = $state(false);
  let copied = $state(false);
  let expanded = $state(false);
  let errorMessage = $state("");
  let resetTimer: ReturnType<typeof setTimeout> | undefined;

  async function copyPrompt() {
    if (copying) return;
    copying = true;
    copied = false;
    errorMessage = "";
    if (resetTimer) clearTimeout(resetTimer);
    try {
      await navigator.clipboard.writeText(sessionPrompt);
      copied = true;
      showToast("新会话启动提示词已复制，可以直接粘贴到 ChatGPT。", {
        title: "复制成功",
        kind: "success",
        duration: 2500,
      });
      resetTimer = setTimeout(() => {
        copied = false;
      }, 2000);
    } catch (error) {
      errorMessage = "复制失败，请选中提示词后手动复制。";
      showToast(String(error), {
        title: "无法复制提示词",
        kind: "error",
        duration: 6000,
      });
    } finally {
      copying = false;
    }
  }

  onDestroy(() => {
    if (resetTimer) clearTimeout(resetTimer);
  });
</script>

<section
  class="rounded-[12px] border border-[var(--color-border)] bg-[var(--card-bg)] px-3 py-2.5 sm:px-4"
  aria-labelledby="chatgpt-session-prompt-title"
>
  <div class="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
    <div class="flex min-w-0 items-center gap-3">
      <span
        class="flex size-9 shrink-0 items-center justify-center rounded-[10px] bg-[var(--primary-soft)] text-[var(--primary)]"
        aria-hidden="true"
      >
        <History size={16} />
      </span>
      <div class="min-w-0">
        <h3 id="chatgpt-session-prompt-title" class="text-sm font-semibold text-[var(--color-text)]">
          ChatGPT 新会话启动提示词
        </h3>
        <p class="mt-0.5 text-xs leading-5 text-[var(--color-text-muted)]">
          同时恢复历史、Harness 与 Goal；长期目标可持续监控并保留推进状态。
        </p>
      </div>
    </div>

    <div class="flex shrink-0 flex-wrap items-center gap-2 sm:flex-nowrap">
      <button
        type="button"
        class="tx-btn-primary min-h-11 shrink-0 px-3 py-2 text-xs active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-50"
        disabled={copying}
        aria-label="复制 ChatGPT 新会话启动提示词"
        onclick={() => void copyPrompt()}
      >
        {#if copied}
          <Check size={14} aria-hidden="true" />
          <span>已复制</span>
        {:else}
          <Copy size={14} aria-hidden="true" />
          <span>{copying ? "复制中…" : "复制完整提示词"}</span>
        {/if}
      </button>

      <button
        type="button"
        class="tx-btn-ghost min-h-11 shrink-0 gap-1.5 px-3 py-2 text-xs active:scale-[0.98]"
        aria-expanded={expanded}
        aria-controls="chatgpt-session-prompt-content"
        onclick={() => (expanded = !expanded)}
      >
        <span>{expanded ? "收起提示词" : "查看完整提示词"}</span>
        <ChevronDown
          size={14}
          class={`transition-transform duration-200 motion-reduce:transition-none ${expanded ? "rotate-180" : ""}`}
          aria-hidden="true"
        />
      </button>
    </div>
  </div>

  {#if expanded}
    <div id="chatgpt-session-prompt-content" class="mt-3 border-t border-[var(--color-border)] pt-3">
      <pre
        class="tx-mono whitespace-pre-wrap break-words rounded-[10px] bg-[var(--surface-hover)] p-3 leading-5 text-[var(--color-text-secondary)]"
      >{sessionPrompt}</pre>
      <p class="mt-2 text-[11px] leading-5 text-[var(--color-text-muted)]">
        复制后粘贴到使用当前工作区 MCP 连接器的 ChatGPT 新会话。
      </p>
    </div>
  {/if}

  {#if errorMessage}
    <p class="mt-2 text-xs text-[var(--danger)]" role="alert">{errorMessage}</p>
  {/if}
  <span class="sr-only" aria-live="polite">{copied ? "提示词已复制" : ""}</span>
</section>
