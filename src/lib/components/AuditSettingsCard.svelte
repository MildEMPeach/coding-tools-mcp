<script lang="ts">
  import { onMount } from "svelte";
  import { ask, message } from "@tauri-apps/plugin-dialog";
  import { Database, Globe2, Save, Trash2 } from "@lucide/svelte";
  import {
    clearAllLogs,
    getAuditConfig,
    setAuditConfig,
    type AuditConfig,
  } from "$lib/api/audit";
  import { showToast } from "$lib/stores/toast";

  // 仅用于异步加载前的可渲染草稿；后端配置返回后会整体替换，不能据此覆盖持久化值。
  const DEFAULT_AUDIT_CONFIG: AuditConfig = {
    httpLogEnabled: true,
    httpLogRetentionDays: 30,
    toolAuditEnabled: true,
    toolAuditRetentionDays: 30,
    detailLimitBytes: 256 * 1024,
  };

  const detailOptions = [
    { value: -1, label: "仅元数据", hint: "不保存入参与结果正文" },
    { value: 256 * 1024, label: "标准 (256 KB)", hint: "适合日常使用" },
    { value: 1024 * 1024, label: "扩展 (1 MB)", hint: "保留较长结果" },
    { value: 0, label: "完整保存", hint: "可能增加磁盘占用" },
  ];
  const retentionPresets = [1, 3, 7, 30, 90];
  const retentionOptions = [
    { value: "off", label: "不记录" },
    { value: "forever", label: "永久保留" },
    { value: "1", label: "保留 1 天" },
    { value: "3", label: "保留 3 天" },
    { value: "7", label: "保留 7 天" },
    { value: "30", label: "保留 1 个月" },
    { value: "90", label: "保留 3 个月" },
  ];

  // saved 是持久化基线，auditConfig 是可编辑副本；两者分离才能可靠判断未保存改动。
  let savedAuditConfig = $state<AuditConfig | null>(null);
  let auditConfig = $state<AuditConfig>({ ...DEFAULT_AUDIT_CONFIG });
  let auditConfigSaving = $state(false);
  let clearingLogs = $state(false);

  // AuditConfig 是固定字段的扁平对象，序列化比较可稳定判断草稿是否偏离持久化基线。
  const auditConfigChanged = $derived(
    savedAuditConfig !== null && JSON.stringify(savedAuditConfig) !== JSON.stringify(auditConfig),
  );

  /**
   * 加载持久化配置，同时建立保存基线和独立编辑副本。
   * 请求失败时保留默认草稿，但 savedAuditConfig 仍为 null，因此不会误启用“保存更改”。
   */
  async function loadAuditConfig() {
    try {
      const config = await getAuditConfig();
      savedAuditConfig = config;
      auditConfig = { ...config };
    } catch (e) {
      await message(String(e), { title: "加载日志设置失败", kind: "error" });
    }
  }

  /**
   * 保存当前草稿。重复点击由 auditConfigSaving 合并；提交前将保留期和详情上限映射到 UI
   * 支持的规范档位，成功后用后端返回值同时刷新基线与草稿。
   */
  async function saveAuditConfig() {
    if (auditConfigSaving) return;
    auditConfigSaving = true;
    try {
      const next = {
        ...auditConfig,
        httpLogRetentionDays: auditConfig.httpLogEnabled
          ? retentionPreset(auditConfig.httpLogRetentionDays)
          : normalizeDays(auditConfig.httpLogRetentionDays),
        toolAuditRetentionDays: auditConfig.toolAuditEnabled
          ? retentionPreset(auditConfig.toolAuditRetentionDays)
          : normalizeDays(auditConfig.toolAuditRetentionDays),
        detailLimitBytes: Number(detailChoice()),
      };
      const saved = await setAuditConfig(next);
      savedAuditConfig = saved;
      auditConfig = { ...saved };
      showToast("日志与审计设置已保存", { title: "已保存", kind: "success" });
    } catch (e) {
      await message(String(e), { title: "保存失败", kind: "error" });
    } finally {
      auditConfigSaving = false;
    }
  }

  /**
   * 经原生确认框清空全部文本日志与工具审计。
   * clearingLogs 只阻止同一破坏性操作重复提交；实际清理数量以两种后端存储的返回值为准。
   */
  async function handleClearLogs() {
    if (clearingLogs) return;
    // 清理同时影响文本日志和 SQLite 审计，必须在调用不可逆命令前确认。
    const confirmed = await ask("确定清除全部日志和工具审计记录", {
      title: "清除缓存",
      kind: "warning",
      okLabel: "清除",
      cancelLabel: "取消",
    });
    if (!confirmed) return;
    clearingLogs = true;
    try {
      const result = await clearAllLogs();
      showToast(`已清空 ${result.logFiles} 个日志文件和 ${result.auditRecords} 条审计记录`, {
        title: "缓存已清除",
        kind: "success",
      });
    } catch (e) {
      await message(String(e), { title: "清除失败", kind: "error" });
    } finally {
      clearingLogs = false;
    }
  }

  /** 将任意保留天数收敛为后端允许的 0..36500 整数；非有限值按 0 处理。 */
  function normalizeDays(value: number): number {
    return Number.isFinite(value) ? Math.min(36_500, Math.max(0, Math.trunc(value))) : 0;
  }

  /** 保留详情上限的 -1 哨兵，并剔除小数及非法数值。 */
  function normalizeDetailLimit(value: number): number {
    if (!Number.isFinite(value)) return 256 * 1024;
    return Math.max(-1, Math.trunc(value));
  }

  /**
   * 将任意历史天数映射到当前下拉档位：向上取最近档位，0 或超过最大档位均显示为永久。
   * 保存时复用该函数，保证展示值和最终持久化值一致。
   */
  function retentionPreset(days: number): number {
    const normalized = normalizeDays(days);
    if (normalized === 0) return 0;
    return retentionPresets.find((preset) => normalized <= preset) ?? 0;
  }

  /** 将后端的 enabled + retentionDays 两字段还原为单个下拉选项。 */
  function retentionChoice(kind: "http" | "tool"): string {
    // enabled 与 days 是两个维度：关闭记录不抹掉原保留天数，0 天仅在启用时表示永久。
    const enabled = kind === "http" ? auditConfig.httpLogEnabled : auditConfig.toolAuditEnabled;
    const days = kind === "http" ? auditConfig.httpLogRetentionDays : auditConfig.toolAuditRetentionDays;
    if (!enabled) return "off";
    const preset = retentionPreset(days);
    return preset === 0 ? "forever" : String(preset);
  }

  /**
   * 将下拉选择写回对应草稿。
   * “不记录”只关闭 enabled，不清除原天数；重新选择具体档位时再覆盖天数。“永久”使用 0。
   */
  function updateRetention(kind: "http" | "tool", event: Event) {
    const choice = (event.currentTarget as HTMLSelectElement).value;
    const enabled = choice !== "off";
    let days = kind === "http" ? auditConfig.httpLogRetentionDays : auditConfig.toolAuditRetentionDays;
    if (choice === "forever") {
      days = 0;
    } else if (enabled) {
      days = Number(choice);
    }
    if (kind === "http") {
      auditConfig.httpLogEnabled = enabled;
      auditConfig.httpLogRetentionDays = days;
    } else {
      auditConfig.toolAuditEnabled = enabled;
      auditConfig.toolAuditRetentionDays = days;
    }
  }

  /**
   * 将任意后端详情上限投影到四个 UI 档位；负数、标准、扩展和不限制分别保持其哨兵语义。
   */
  function detailChoice(): string {
    const limit = normalizeDetailLimit(auditConfig.detailLimitBytes);
    if (detailOptions.some((option) => option.value === limit)) return String(limit);
    if (limit < 0) return "-1";
    if (limit <= 256 * 1024) return String(256 * 1024);
    if (limit <= 1024 * 1024) return String(1024 * 1024);
    return "0";
  }

  onMount(() => {
    void loadAuditConfig();
  });
</script>

<section class="tx-card overflow-hidden">
  <div class="flex flex-wrap items-center justify-between gap-3 border-b border-[var(--color-border)] px-5 py-4">
    <div>
      <h3 class="text-sm font-semibold">日志与审计</h3>
      <p class="mt-0.5 text-xs text-[var(--color-text-muted)]">
        管理 HTTP 访问日志与工具调用审计记录的保存策略
      </p>
    </div>
    <div class="flex flex-wrap items-center gap-2">
      <button
        type="button"
        class="inline-flex items-center gap-1.5 rounded-md border border-red-500/25 bg-red-500/5 px-3 py-1.5 text-xs font-medium text-red-500 transition-colors hover:bg-red-500/10 disabled:opacity-50"
        disabled={clearingLogs}
        onclick={() => void handleClearLogs()}
      >
        <Trash2 size={13} />
        {clearingLogs ? "清除中…" : "清除缓存"}
      </button>
      <button
        type="button"
        class="tx-btn-primary gap-1.5 px-3.5 py-1.5 text-xs disabled:cursor-not-allowed disabled:opacity-45"
        disabled={!auditConfigChanged || auditConfigSaving}
        onclick={() => void saveAuditConfig()}
      >
        <Save size={13} />
        {auditConfigSaving ? "保存中…" : "保存更改"}
      </button>
    </div>
  </div>

  <div class="grid divide-y divide-[var(--color-border)] xl:grid-cols-2 xl:divide-x xl:divide-y-0">
    <div class="flex items-center justify-between gap-4 p-5">
      <div class="flex min-w-0 gap-3">
        <span class="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-sky-500/10 text-sky-600">
          <Globe2 size={20} />
        </span>
        <div>
          <h4 class="text-sm font-semibold">HTTP 请求日志</h4>
          <p class="mt-0.5 text-xs text-[var(--color-text-muted)]">记录 HTTP 请求与扫描访问</p>
        </div>
      </div>
      <div class="shrink-0">
        <select
          class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-xs font-medium text-[var(--color-text)] transition-colors focus:border-[var(--color-accent)] focus:outline-none"
          aria-label="HTTP 请求日志记录方式"
          value={retentionChoice("http")}
          onchange={(event) => updateRetention("http", event)}
        >
          {#each retentionOptions as option}
            <option value={option.value}>{option.label}</option>
          {/each}
        </select>
      </div>
    </div>

    <div class="flex items-center justify-between gap-4 p-5">
      <div class="flex min-w-0 gap-3">
        <span class="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-violet-500/10 text-violet-600">
          <Database size={20} />
        </span>
        <div>
          <h4 class="text-sm font-semibold">工具调用审计</h4>
          <p class="mt-0.5 text-xs text-[var(--color-text-muted)]">记录工具参数与执行结果</p>
        </div>
      </div>
      <div class="shrink-0">
        <select
          class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-xs font-medium text-[var(--color-text)] transition-colors focus:border-[var(--color-accent)] focus:outline-none"
          aria-label="工具调用审计记录方式"
          value={retentionChoice("tool")}
          onchange={(event) => updateRetention("tool", event)}
        >
          {#each retentionOptions as option}
            <option value={option.value}>{option.label}</option>
          {/each}
        </select>
      </div>
    </div>
  </div>

  <div class="border-t border-[var(--color-border)] px-5 py-4">
    <div class="flex flex-col gap-3">
      <div>
        <h4 class="text-sm font-semibold">工具详情保留级别</h4>
        <p class="mt-0.5 text-xs text-[var(--color-text-muted)]">
          设置输入与返回内容的保存上限
        </p>
      </div>
      <div class="grid grid-cols-2 gap-2 sm:grid-cols-4">
        {#each detailOptions as option}
          {@const selected = detailChoice() === String(option.value)}
          <button
            type="button"
            disabled={!auditConfig.toolAuditEnabled}
            class="flex flex-col justify-center rounded-xl border px-3.5 py-2.5 text-left transition-all duration-150 disabled:cursor-not-allowed disabled:opacity-40 {selected ? 'border-[var(--color-accent)] bg-[var(--color-accent)]/8 text-[var(--color-accent)]' : 'border-[var(--color-border)] bg-[var(--color-bg)]/50 hover:bg-[var(--color-bg)] text-[var(--color-text)]'}"
            onclick={() => {
              auditConfig.detailLimitBytes = option.value;
            }}
          >
            <span class="text-xs font-semibold">
              {option.label}
            </span>
            <span class="mt-1 text-[11px] text-[var(--color-text-muted)]">
              {option.hint}
            </span>
          </button>
        {/each}
      </div>
    </div>
  </div>
</section>
