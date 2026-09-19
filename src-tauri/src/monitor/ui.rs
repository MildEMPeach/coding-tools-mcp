pub const GOAL_WIDGET_URI: &str = "ui://coding-tools-mcp/goal-status-v1.html";

pub fn goal_widget_html() -> &'static str {
    r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width,initial-scale=1" />
  <style>
    :root { color-scheme: light dark; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
    body { margin: 0; padding: 12px; background: transparent; }
    .card { border: 1px solid color-mix(in srgb, currentColor 18%, transparent); border-radius: 12px; padding: 12px; }
    .row { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
    .title { font-size: 13px; font-weight: 650; line-height: 1.4; }
    .meta, .msg { margin-top: 6px; font-size: 11px; opacity: .72; line-height: 1.45; }
    .badge { padding: 2px 7px; border-radius: 999px; font-size: 10px; border: 1px solid color-mix(in srgb, currentColor 18%, transparent); white-space: nowrap; }
    .progress { height: 5px; margin-top: 9px; border-radius: 999px; overflow: hidden; background: color-mix(in srgb, currentColor 10%, transparent); }
    .bar { height: 100%; background: currentColor; opacity: .55; transition: width .2s ease; }
    button { margin-top: 9px; border: 1px solid color-mix(in srgb, currentColor 22%, transparent); background: transparent; color: inherit; border-radius: 8px; padding: 6px 9px; font: inherit; font-size: 11px; cursor: pointer; }
    button:disabled { opacity: .5; cursor: default; }
    .hidden { display: none; }
  </style>
</head>
<body>
  <div class="card">
    <div class="row">
      <div id="objective" class="title">Goal</div>
      <span id="status" class="badge">—</span>
    </div>
    <div class="progress"><div id="bar" class="bar" style="width:0%"></div></div>
    <div id="meta" class="meta"></div>
    <div id="message" class="msg"></div>
    <button id="continue" class="hidden" type="button">继续 Goal</button>
  </div>
  <script>
    (() => {
      let sent = false;
      const objectiveEl = document.getElementById('objective');
      const statusEl = document.getElementById('status');
      const metaEl = document.getElementById('meta');
      const messageEl = document.getElementById('message');
      const barEl = document.getElementById('bar');
      const continueEl = document.getElementById('continue');

      function output() {
        return window.openai?.toolOutput ?? null;
      }

      function followUpPrompt(data) {
        const goal = data?.goal ?? {};
        const pending = Array.isArray(goal.pending_steps) ? goal.pending_steps.slice(0, 6) : [];
        return [
          `继续推进当前 Goal：${goal.objective ?? ''}`,
          `Goal ID: ${goal.id ?? ''}`,
          pending.length ? `Pending steps: ${pending.join('；')}` : '',
          '先调用 goal_status 获取最新状态，并继续实际执行，不要只做总结。',
          '阶段进展用 goal_update；需要用户决策或外部资源时用 goal_block。',
          '验证通过后调用 goal_complete(verified=true)。如果本轮结束时仍未完成，再调用 goal_handoff。'
        ].filter(Boolean).join('\n');
      }

      async function sendFollowUp(data) {
        if (sent || !data?.handoff_requested || !data?.should_continue) return;
        const send = window.openai?.sendFollowUpMessage;
        continueEl.classList.remove('hidden');
        if (typeof send !== 'function') {
          messageEl.textContent = '当前 ChatGPT 宿主不支持自动 follow-up，可点击按钮或手动发送“继续 Goal”。';
          return;
        }
        const goal = data.goal ?? {};
        const key = `${goal.id ?? 'goal'}:${goal.continuation_count ?? 0}`;
        if (window.openai?.widgetState?.lastHandoffKey === key) return;
        sent = true;
        continueEl.disabled = true;
        messageEl.textContent = '正在请求 ChatGPT 继续下一轮…';
        window.openai?.setWidgetState?.({ lastHandoffKey: key });
        try {
          await send({ prompt: followUpPrompt(data), scrollToBottom: true });
          messageEl.textContent = '已请求 ChatGPT 继续当前 Goal。';
        } catch (error) {
          sent = false;
          continueEl.disabled = false;
          messageEl.textContent = `自动续跑请求失败：${String(error)}`;
        }
      }

      function render() {
        const data = output();
        const goal = data?.goal;
        if (!goal) {
          objectiveEl.textContent = '当前没有活动 Goal';
          statusEl.textContent = 'standalone';
          metaEl.textContent = '';
          messageEl.textContent = data?.message ?? '';
          return;
        }
        objectiveEl.textContent = goal.objective ?? 'Goal';
        statusEl.textContent = goal.health ?? goal.status ?? 'active';
        const completed = Array.isArray(goal.completed_steps) ? goal.completed_steps.length : 0;
        const pending = Array.isArray(goal.pending_steps) ? goal.pending_steps.length : 0;
        const total = completed + pending;
        const pct = total > 0 ? Math.round((completed / total) * 100) : 0;
        barEl.style.width = `${pct}%`;
        metaEl.textContent = `进度 ${completed}/${total || '—'} · 自动续跑 ${data.should_continue ? 'Ready' : 'Off'} · 第 ${goal.continuation_count ?? 0} 次 handoff`;
        messageEl.textContent = goal.monitor_message ?? '';
        if (data.handoff_requested && data.should_continue) {
          setTimeout(() => void sendFollowUp(data), 1800);
        }
      }

      continueEl.addEventListener('click', () => {
        sent = false;
        void sendFollowUp(output());
      });
      render();
    })();
  </script>
</body>
</html>"#
}

