<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import {
    Activity,
    AlertCircle,
    AlertTriangle,
    Ban,
    Check,
    CheckCircle2,
    ChevronDown,
    ChevronRight,
    Clock3,
    Copy,
    Database,
    FolderOpen,
    Globe2,
    RefreshCw,
    XCircle,
  } from "@lucide/svelte";
  import {
    getAuditConfig,
    getAuditRecord,
    getAuditStats,
    queryAuditRecords,
    type AuditConfig,
    type AuditQuery,
    type AuditRecord,
    type AuditStats,
  } from "$lib/api/audit";
  import {
    openHttpAccessLogDirectory,
    queryHttpAccessLogs,
    type AccessLogResult,
    type AccessLogService,
  } from "$lib/api/access-logs";
  import { listWorkspaces } from "$lib/api/workspaces";
  import { showToast } from "$lib/stores/toast";
  import type { WorkspaceProfile } from "$lib/types";

  /**
   * 页面并行展示两套存储：工具调用来自 SQLite 审计，HTTP 请求来自按日文本日志。
   * 工作区和时间作用于两套数据，成功状态由两套列表共享，服务来源只作用于 HTTP。列表查询
   * 只取摘要，工具详情在展开时按 UUID 延迟加载。
   */
  type RangePreset = "today" | "7d" | "30d" | "all" | "custom";

  const rangeOptions: { value: RangePreset; label: string }[] = [
    { value: "today", label: "今日" },
    { value: "7d", label: "7 天" },
    { value: "30d", label: "30 天" },
    { value: "all", label: "全部" },
    { value: "custom", label: "自定义" },
  ];

  const accessServiceOptions: { value: AccessLogService; label: string }[] = [
    { value: "all", label: "全部服务" },
    { value: "mcp", label: "MCP" },
    { value: "actions", label: "Actions" },
  ];

  const autoRefreshOptions = [
    { value: 0, label: "关闭" },
    { value: 5, label: "5 秒" },
    { value: 10, label: "10 秒" },
    { value: 30, label: "30 秒" },
    { value: 60, label: "60 秒" },
  ];

  const RANGE_PRESET_STORAGE_KEY = "coding-tools-mcp.audit.range-preset";
  const AUTO_REFRESH_STORAGE_KEY = "coding-tools-mcp.audit.auto-refresh-sec";
  // 自定义区间依赖临时时间输入，不持久化；离开页面后恢复最近一次固定预设。
  const rememberedRangePresets = new Set<RangePreset>(["today", "7d", "30d", "all"]);

  const EMPTY_ACCESS_LOG_RESULT: AccessLogResult = {
    stats: {
      totalRequests: 0,
      status2xx: 0,
      status3xx: 0,
      status4xx: 0,
      status5xx: 0,
      responseBytes: 0,
    },
    records: [],
  };

  type LogTab = "tool" | "http";
  type StatusFilter = "all" | "success" | "failure";
  type RecordLimit = 100 | 200 | 500 | 1000;

  const recordLimitOptions: RecordLimit[] = [100, 200, 500, 1000];

  // 页面基础数据与当前筛选条件。
  let savedConfig = $state<AuditConfig | null>(null);
  let workspaces = $state<WorkspaceProfile[]>([]);
  let workspaceFilter = $state("");
  let rangePreset = $state<RangePreset>("today");
  let from = $state("");
  let to = $state("");
  let accessService = $state<AccessLogService>("all");
  // 两套列表结果；detailsById 仅缓存已展开工具记录的大字段。
  let accessLogs = $state<AccessLogResult>(EMPTY_ACCESS_LOG_RESULT);
  let stats = $state<AuditStats | null>(null);
  let records = $state<AuditRecord[]>([]);
  let detailsById = $state<Record<string, AuditRecord>>({});
  let loadingDetails = $state<Record<string, boolean>>({});
  let activeTab = $state<LogTab>("tool");
  let statusFilter = $state<StatusFilter>("all");
  // 仅在筛选条件变化时递增：重建工具列表并统一收起原生 <details>；普通刷新不要修改它。
  let toolListRevision = $state(0);
  let toolRecordLimit = $state<RecordLimit>(100);
  let httpRecordLimit = $state<RecordLimit>(100);
  // 刷新采用“单任务 + 一个待处理标记”，避免定时器和用户操作并发覆盖新筛选结果。
  let busy = $state(false);
  let refreshPending = false;
  let error = $state("");
  let autoRefreshSec = $state(0);
  let autoRefreshDropdownOpen = $state(false);
  let autoRefreshTimer: ReturnType<typeof setInterval> | undefined;
  // WKWebView 的原生纵向滚动条会占布局宽度：主容器隐藏原生条，右侧代理条只负责显示/拖动。
  let auditPageScroll: HTMLElement | undefined;
  let auditPageHeader: HTMLElement | undefined;
  let auditPageBody: HTMLElement | undefined;
  let auditPageScrollbar: HTMLElement | undefined;
  let auditPageScrollHeight = $state(0);
  let auditPageHasScrollbar = $state(false);
  let auditPageResizeObserver: ResizeObserver | undefined;

  /**
   * 用真实滚动容器的 scrollHeight 驱动代理滚动条。
   * Header、自定义时间区间、列表和展开详情都会改变页面高度，因此由 ResizeObserver 统一重算。
   */
  function updateAuditPageScrollbar() {
    if (!auditPageScroll) return;
    auditPageScrollHeight = auditPageScroll.scrollHeight;
    auditPageHasScrollbar = auditPageScroll.scrollHeight > auditPageScroll.clientHeight;
  }

  /** 用户滚动正文时同步代理滑块；相等判断用于阻止两个 scroll 事件互相递归。 */
  function syncAuditPageScrollbar() {
    if (auditPageScroll && auditPageScrollbar && auditPageScrollbar.scrollTop !== auditPageScroll.scrollTop) {
      auditPageScrollbar.scrollTop = auditPageScroll.scrollTop;
    }
  }

  /** 用户拖动代理滑块时反向更新正文滚动位置。 */
  function syncAuditPageScroll() {
    if (auditPageScroll && auditPageScrollbar && auditPageScroll.scrollTop !== auditPageScrollbar.scrollTop) {
      auditPageScroll.scrollTop = auditPageScrollbar.scrollTop;
    }
  }

  /**
   * 切换自动刷新周期。每次先清理旧定时器，确保全页最多存在一个刷新源。
   * 页面不可见、已有刷新在执行或已登记补偿刷新时跳过当前 tick；restore 时 persist=false，
   * 避免仅恢复偏好又重复写 localStorage。
   */
  function setAutoRefresh(sec: number, persist = true) {
    autoRefreshSec = sec;
    if (persist) {
      localStorage.setItem(AUTO_REFRESH_STORAGE_KEY, String(sec));
    }
    if (autoRefreshTimer) {
      clearInterval(autoRefreshTimer);
      autoRefreshTimer = undefined;
    }
    if (sec > 0) {
      autoRefreshTimer = setInterval(() => {
        if (!busy && !refreshPending && !document.hidden) {
          void refreshData();
        }
      }, sec * 1000);
    }
  }

  /**
   * 切换时间预设并收起工具详情。固定预设立即查询并持久化；自定义只展开输入区，待用户点击
   * “查询”后才发送请求，同时不覆盖最近固定预设。
   */
  function selectRangePreset(preset: RangePreset) {
    rangePreset = preset;
    toolListRevision += 1;
    if (preset !== "custom") {
      localStorage.setItem(RANGE_PRESET_STORAGE_KEY, preset);
      void refreshData();
    }
  }

  /**
   * 恢复时间与自动刷新偏好。存储值必须仍存在于当前选项集合，旧版本或损坏值直接忽略。
   */
  function restorePreferences() {
    const storedRange = localStorage.getItem(RANGE_PRESET_STORAGE_KEY) as RangePreset | null;
    if (storedRange && rememberedRangePresets.has(storedRange)) {
      rangePreset = storedRange;
    }

    const storedAutoRefresh = localStorage.getItem(AUTO_REFRESH_STORAGE_KEY);
    if (storedAutoRefresh !== null) {
      const seconds = Number(storedAutoRefresh);
      if (autoRefreshOptions.some((option) => option.value === seconds)) {
        setAutoRefresh(seconds, false);
      }
    }
  }

  function handleWindowClick() {
    if (autoRefreshDropdownOpen) {
      autoRefreshDropdownOpen = false;
    }
  }

  // 两个标签分别记忆获取上限；HTTP 成功=2xx+3xx，工具失败=执行错误+策略拒绝。
  let activeRecordLimit = $derived(activeTab === "tool" ? toolRecordLimit : httpRecordLimit);
  let httpSuccessCount = $derived(accessLogs.stats.status2xx + accessLogs.stats.status3xx);
  let httpFailureCount = $derived(accessLogs.stats.status4xx + accessLogs.stats.status5xx);
  let toolErrorCount = $derived(stats ? stats.failureCalls + stats.rejectedCalls : 0);
  let toolErrorRate = $derived(
    stats && stats.totalCalls > 0
      ? ((toolErrorCount / stats.totalCalls) * 100).toFixed(1)
      : "0.0"
  );
  let toolErrorRateNum = $derived(Number(toolErrorRate));
  /**
   * 错误率多级平滑预警梯度：
   * 1. rate === 0: 完全安全（正翡翠绿），系统无任何异常
   * 2. rate <= 10%: 相对安全（温和青绿色），偶发抖动但总体健康，不惊扰用户
   * 3. rate <= 20%: 轻度关注（暖黄浅琥珀色），错误率轻微抬头，提示留意
   * 4. rate <= 35%: 中度异常（醒目橙色），异常占比显著，需尽快排查
   * 5. rate > 35%: 严重故障（警报猩红色），大面积不可用或核心报错，触发强告警
   */
  let toolErrorSeverity = $derived.by(() => {
    const rate = toolErrorRateNum;
    if (rate === 0) {
      // 档位 1：完全安全（0% 错误率，100% 成功）
      return {
        level: "safe",
        iconColor: "text-emerald-500",
        textColor: "text-emerald-600 dark:text-emerald-400",
      };
    }
    if (rate <= 10) {
      // 档位 2：相对安全（10% 以内，温和青绿，偶发抖动不告警）
      return {
        level: "stable",
        iconColor: "text-teal-500",
        textColor: "text-teal-600 dark:text-teal-400",
      };
    }
    if (rate <= 20) {
      // 档位 3：轻度关注（20% 以内，暖黄琥珀色提示）
      return {
        level: "low",
        iconColor: "text-amber-500",
        textColor: "text-amber-600 dark:text-amber-400",
      };
    }
    if (rate <= 35) {
      // 档位 4：中度异常（20% ~ 35%，饱和橙色，需要排查）
      return {
        level: "medium",
        iconColor: "text-orange-500",
        textColor: "text-orange-600 dark:text-orange-400",
      };
    }
    // 档位 5：严重故障（大于 35%，警报猩红）
    return {
      level: "critical",
      iconColor: "text-rose-500",
      textColor: "text-rose-600 dark:text-rose-400",
    };
  });
  function timestamp(value: string): number | undefined {
    if (!value) return undefined;
    const parsed = Date.parse(value);
    return Number.isNaN(parsed) ? undefined : parsed;
  }

  /**
   * 将时间预设转换为后端毫秒区间。
   * “今日”从本地零点开始；7/30 天是相对当前时刻的滚动窗口；“全部”不发送边界；自定义
   * 保持左闭右开语义，非法或空输入不生成对应边界。
   */
  function currentTimeRange(): { fromMs?: number; toMs?: number } {
    const now = Date.now();
    let fromMs: number | undefined;
    let toMs: number | undefined;
    if (rangePreset === "today") {
      const start = new Date();
      start.setHours(0, 0, 0, 0);
      fromMs = start.getTime();
    } else if (rangePreset === "7d") {
      fromMs = now - 7 * 86_400_000;
    } else if (rangePreset === "30d") {
      fromMs = now - 30 * 86_400_000;
    } else if (rangePreset === "custom") {
      fromMs = timestamp(from);
      toMs = timestamp(to);
    }
    return { fromMs, toMs };
  }

  /** 构造工具审计的公共查询；列表固定使用摘要模式，详情由 getAuditRecord 单独获取。 */
  function currentQuery(): AuditQuery {
    return {
      ...currentTimeRange(),
      workspaceId: workspaceFilter || undefined,
      recordType: "tool",
      includeDetails: false,
    };
  }

  /** 将共享 UI 状态映射到两套后端口径：all=不筛选，failure=所有非成功记录。 */
  function successfulFilter(): boolean | undefined {
    if (statusFilter === "all") return undefined;
    return statusFilter === "success";
  }

  /**
   * 更新当前标签自己的获取上限。工具列表重建后统一收起；切换标签时另一标签的上限不变。
   */
  function changeRecordLimit(event: Event) {
    const value = Number((event.currentTarget as HTMLSelectElement).value) as RecordLimit;
    if (!recordLimitOptions.includes(value)) return;
    if (activeTab === "tool") {
      toolRecordLimit = value;
      toolListRevision += 1;
    } else {
      httpRecordLimit = value;
    }
    void refreshData();
  }

  /** 切换共享状态筛选，并重建工具列表以清除所有原生 details 展开状态。 */
  function changeStatusFilter(value: StatusFilter) {
    if (statusFilter === value) return;
    statusFilter = value;
    toolListRevision += 1;
    void refreshData();
  }

  /** 首次加载先并行读取配置和工作区，随后用完整筛选上下文执行第一次数据刷新。 */
  async function loadPage() {
    try {
      const [nextConfig, nextWorkspaces] = await Promise.all([
        getAuditConfig(),
        listWorkspaces(),
      ]);
      savedConfig = nextConfig;
      workspaces = nextWorkspaces;
      await refreshData();
    } catch (value) {
      error = String(value);
    }
  }

  /**
   * 原子刷新概览、工具摘要和 HTTP 日志。
   *
   * 同一时刻只允许一个请求批次。忙碌期间的新刷新只设置 refreshPending；旧批次完成后丢弃
   * 其结果，并使用最新筛选条件补跑一次，避免慢响应覆盖用户刚切换的条件。三个查询并行执行，
   * 只有全部成功且批次未过期时才一起替换页面数据。
   */
  async function refreshData() {
    if (busy) {
      refreshPending = true;
      return;
    }
    busy = true;
    error = "";
    try {
      const query = currentQuery();
      const timeRange = currentTimeRange();
      const successful = successfulFilter();
      const [nextStats, nextRecords, nextAccessLogs] = await Promise.all([
        getAuditStats(query),
        queryAuditRecords({
          ...query,
          successful,
          limit: toolRecordLimit,
        }),
        queryHttpAccessLogs({
          ...timeRange,
          workspaceId: workspaceFilter || undefined,
          service: accessService,
          successful,
          limit: httpRecordLimit,
        }),
      ]);
      if (refreshPending) return;
      stats = nextStats;
      records = nextRecords;
      accessLogs = nextAccessLogs;
      // 列表查询只含摘要。刷新时必须保留仍可见记录的完整详情，否则 <details>
      // 会保持展开却不再触发 toggle 重新取数，导致命令、输入和返回结果显示为空。
      const visibleRecordIds = new Set(nextRecords.map((record) => record.id));
      detailsById = Object.fromEntries(
        Object.entries(detailsById).filter(([id]) => visibleRecordIds.has(id))
      );
      loadingDetails = Object.fromEntries(
        Object.entries(loadingDetails).filter(([id]) => visibleRecordIds.has(id))
      );
    } catch (value) {
      if (!refreshPending) error = String(value);
    } finally {
      busy = false;
      if (refreshPending) {
        refreshPending = false;
        void refreshData();
      }
    }
  }

  /** 打开当前工作区日志目录；未选择工作区时打开日志总目录。 */
  async function openAccessLogFolder() {
    try {
      await openHttpAccessLogDirectory(workspaceFilter || undefined);
    } catch (value) {
      showToast(String(value), { title: "无法打开日志目录", kind: "error" });
    }
  }

  /**
   * 首次展开工具条目时按审计 UUID 获取完整记录。
   * 收起事件、已有缓存和正在加载三种情况直接返回，避免重复 IPC；详情加载失败不影响摘要列表。
   */
  async function loadRecordDetail(event: Event, id: string) {
    const target = event.currentTarget as HTMLDetailsElement;
    if (!target.open || detailsById[id] || loadingDetails[id]) return;
    loadingDetails[id] = true;
    try {
      const detail = await getAuditRecord(id);
      if (detail) detailsById[id] = detail;
    } catch (value) {
      showToast(String(value), { title: "读取调用详情失败", kind: "error" });
    } finally {
      loadingDetails[id] = false;
    }
  }

  function formatTime(value: number): string {
    return new Date(value).toLocaleString();
  }

  /** 使用十进制 B/KB/MB/GB，与概览和详情保持同一显示口径。 */
  function formatBytes(value: number): string {
    if (value < 1000) return value + " B";
    if (value < 1_000_000) return (value / 1000).toFixed(1) + " KB";
    if (value < 1_000_000_000) return (value / 1_000_000).toFixed(1) + " MB";
    return (value / 1_000_000_000).toFixed(1) + " GB";
  }

  /** 格式化已保存 JSON；摘要或禁用正文时显示“未保留内容”，非 JSON 文本保持原样。 */
  function formatJson(value: string | null): string {
    if (!value) return "未保留内容";
    try {
      return JSON.stringify(JSON.parse(value), null, 2);
    } catch {
      return value;
    }
  }

  function statusLabel(value: string): string {
    if (value === "success") return "成功";
    if (value === "rejected") return "拒绝";
    if (value === "failure") return "错误";
    return value;
  }

  function statusClass(value: string): string {
    if (value === "success") return "border-emerald-500/25 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400";
    if (value === "rejected") return "border-amber-500/25 bg-amber-500/10 text-amber-600 dark:text-amber-400";
    return "border-rose-500/25 bg-rose-500/10 text-rose-600 dark:text-rose-400";
  }

  function workspaceName(id: string | null | undefined): string {
    if (!id) return "无";
    return workspaces.find((workspace) => workspace.id === id)?.name ?? id;
  }

  function httpStatusClass(status: number): string {
    if (status >= 500) return "border-rose-500/25 bg-rose-500/10 text-rose-500 dark:text-rose-400";
    if (status >= 400) return "border-amber-500/25 bg-amber-500/10 text-amber-500 dark:text-amber-400";
    if (status >= 300) return "border-sky-500/25 bg-sky-500/10 text-sky-500 dark:text-sky-400";
    return "border-emerald-500/25 bg-emerald-500/10 text-emerald-500 dark:text-emerald-400";
  }

  let copiedKey = $state<string | null>(null);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  /**
   * 复制详情文本并短暂标记当前按钮。key 包含记录 UUID 和字段名，防止多个复制按钮共享状态。
   */
  async function handleCopy(key: string, text: string) {
    if (!text || text === "未保留内容") return;
    try {
      await navigator.clipboard.writeText(text);
      copiedKey = key;
      if (copyTimer) clearTimeout(copyTimer);
      copyTimer = setTimeout(() => {
        copiedKey = null;
      }, 1500);
      showToast("已复制到剪贴板", { kind: "success", duration: 1500 });
    } catch (err) {
      showToast(String(err), { title: "复制失败", kind: "error" });
    }
  }

  /** 为失败摘要选择最有信息量的单行提示；完整详情缓存存在时优先使用详情。 */
  function recordAlertText(record: AuditRecord): string | null {
    if (record.status === "success") return null;
    const target = detailsById[record.id] ?? record;
    if (target.errorCode && target.errorMessage) {
      return `${target.errorCode} ${target.errorMessage}`;
    }
    if (target.errorMessage) {
      return target.errorMessage;
    }
    if (target.errorCode) {
      return target.errorCode;
    }
    return target.reason || null;
  }

  // 定时器、观察器和全局事件必须随页面销毁释放，避免返回页面后出现重复刷新/重复回调。
  onDestroy(() => {
    if (copyTimer) clearTimeout(copyTimer);
    auditPageResizeObserver?.disconnect();
    if (autoRefreshTimer) {
      clearInterval(autoRefreshTimer);
      autoRefreshTimer = undefined;
    }
    if (typeof window !== "undefined") {
      window.removeEventListener("click", handleWindowClick);
    }
  });

  // 先恢复偏好并建立尺寸观察，再加载数据；首批内容到达后观察器会自动校准代理滚动条。
  onMount(() => {
    restorePreferences();
    auditPageResizeObserver = new ResizeObserver(updateAuditPageScrollbar);
    if (auditPageScroll) auditPageResizeObserver.observe(auditPageScroll);
    if (auditPageHeader) auditPageResizeObserver.observe(auditPageHeader);
    if (auditPageBody) auditPageResizeObserver.observe(auditPageBody);
    requestAnimationFrame(updateAuditPageScrollbar);
    void loadPage();
    window.addEventListener("click", handleWindowClick);
  });
</script>

<div class="audit-page-shell">
<section bind:this={auditPageScroll} class="page-scroll audit-page-scroll" onscroll={syncAuditPageScrollbar}>
  <header bind:this={auditPageHeader} class="page-header relative z-30">
    <p class="page-kicker">日志记录</p>
    <div class="mt-1 flex flex-wrap items-center justify-between gap-4">
      <h2 class="page-title !mt-0">请求与调用日志</h2>

      <!-- 页级筛选：工作区、时间范围与刷新周期 -->
      <div class="flex flex-wrap items-center gap-2">
        <select
          class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-xs font-medium text-[var(--color-text)] focus:border-[var(--color-accent)] focus:outline-none"
          aria-label="工作区筛选"
          bind:value={workspaceFilter}
          onchange={() => {
            toolListRevision += 1;
            void refreshData();
          }}
        >
          <option value="">全部工作区</option>
          {#each workspaces as workspace}
            <option value={workspace.id}>{workspace.name}</option>
          {/each}
        </select>

        <div class="flex rounded-lg border border-[var(--color-border)] bg-[var(--color-bg)] p-0.5">
          {#each rangeOptions as item (item.value)}
            <button
              type="button"
              aria-pressed={rangePreset === item.value}
              class="whitespace-nowrap rounded-md px-2.5 py-1 text-xs font-medium {rangePreset === item.value
                ? 'bg-[var(--color-surface)] text-[var(--color-accent)] shadow-sm'
                : 'text-[var(--color-text-muted)] hover:text-[var(--color-text)]'}"
              onclick={() => selectRangePreset(item.value as RangePreset)}
            >
              {item.label}
            </button>
          {/each}
        </div>

        <!-- 分体式组合按钮：与 tx-btn-ghost 样式完全同源统一 -->
        <div
          class="relative inline-flex h-[32px] items-stretch rounded-[10px] border border-[var(--border)] bg-[var(--card-bg)] shadow-[var(--shadow-sm)] transition-all {autoRefreshDropdownOpen ? 'border-[var(--primary)]/40 ring-1 ring-[var(--primary)]/20' : ''}"
        >
          <button
            type="button"
            class="inline-flex items-center gap-2 rounded-l-[9px] pl-3 pr-2 text-[13px] font-medium text-[var(--text-secondary)] hover:text-[var(--text-main)] hover:bg-[var(--surface-hover)] transition-colors disabled:opacity-50"
            disabled={busy}
            onclick={() => void refreshData()}
            title={autoRefreshSec > 0 ? `当前每 ${autoRefreshSec} 秒自动巡检中，点击立即刷新` : "立即刷新"}
          >
            <RefreshCw size={14} class={busy ? "animate-spin" : ""} />
            <span>
              {autoRefreshSec > 0 ? `${autoRefreshSec}s` : "刷新"}
            </span>
          </button>

          <span class="my-auto h-3.5 w-px bg-[var(--border)] opacity-60"></span>

          <button
            type="button"
            class="inline-flex items-center justify-center rounded-r-[9px] px-2 text-[var(--text-secondary)] hover:text-[var(--text-main)] hover:bg-[var(--surface-hover)] transition-colors {autoRefreshDropdownOpen ? 'bg-[var(--surface-hover)] text-[var(--text-main)]' : ''}"
            aria-label="设置自动刷新频率"
            aria-haspopup="menu"
            aria-expanded={autoRefreshDropdownOpen}
            onclick={(e) => {
              e.stopPropagation();
              autoRefreshDropdownOpen = !autoRefreshDropdownOpen;
            }}
          >
            <ChevronDown
              size={12}
              class="transition-transform duration-200 {autoRefreshDropdownOpen ? 'rotate-180 text-[var(--primary)]' : ''}"
            />
          </button>

          {#if autoRefreshDropdownOpen}
            <div
              class="absolute right-0 top-full z-50 mt-1.5 min-w-[124px] rounded-xl border border-[var(--border)] bg-[var(--card-bg)] p-1.5 shadow-xl backdrop-blur-md"
              role="menu"
            >
              <div class="px-2.5 py-1 text-[11px] font-medium text-[var(--text-muted)]">
                自动刷新
              </div>
              {#each autoRefreshOptions as opt}
                <button
                  type="button"
                  role="menuitem"
                  class="flex w-full items-center justify-between gap-2 rounded-lg px-2.5 py-1.5 text-xs transition-colors {autoRefreshSec === opt.value
                    ? 'bg-[var(--primary-soft)] font-medium text-[var(--primary)]'
                    : 'text-[var(--text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-main)]'}"
                  onclick={(e) => {
                    e.stopPropagation();
                    setAutoRefresh(opt.value);
                    autoRefreshDropdownOpen = false;
                  }}
                >
                  <span>{opt.label}</span>
                  {#if autoRefreshSec === opt.value}
                    <Check size={13} class="shrink-0 text-[var(--primary)]" />
                  {/if}
                </button>
              {/each}
            </div>
          {/if}
        </div>
      </div>
    </div>
    <p class="mt-2 max-w-3xl text-sm text-[var(--color-text-muted)]">
      查看各工作区的工具执行与 HTTP 请求记录
    </p>

    {#if rangePreset === "custom"}
      <div class="mt-3 flex flex-wrap items-center gap-2 border-t border-[var(--color-border)]/60 pt-3 text-xs">
        <span class="font-medium text-[var(--color-text-muted)]">起止时间：</span>
        <input
          class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1 text-xs text-[var(--color-text)] focus:border-[var(--color-accent)] focus:outline-none"
          type="datetime-local"
          bind:value={from}
        />
        <span class="text-[var(--color-text-muted)]">至</span>
        <input
          class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1 text-xs text-[var(--color-text)] focus:border-[var(--color-accent)] focus:outline-none"
          type="datetime-local"
          bind:value={to}
        />
        <button
          type="button"
          class="rounded-md bg-[var(--color-accent)] px-3 py-1 text-xs font-medium text-white transition-opacity hover:opacity-90"
          onclick={() => {
            toolListRevision += 1;
            void refreshData();
          }}
        >
          查询
        </button>
      </div>
    {/if}
  </header>

  <div bind:this={auditPageBody} class="page-body flex flex-col gap-5">
    {#if error}
      <p class="rounded-xl border border-rose-500/25 bg-rose-500/10 px-4 py-3 text-sm text-rose-600 dark:text-rose-400">{error}</p>
    {/if}

    {#if savedConfig && !savedConfig.toolAuditEnabled}
      <div class="flex items-center justify-between gap-3 rounded-xl border border-amber-500/25 bg-amber-500/10 px-4 py-3 text-sm text-amber-700">
        <span>工具调用审计已关闭</span>
        <a class="shrink-0 font-medium underline" href="/settings/general">前往通用设置</a>
      </div>
    {/if}

    {#if savedConfig && !savedConfig.httpLogEnabled}
      <div class="flex items-center justify-between gap-3 rounded-xl border border-amber-500/25 bg-amber-500/10 px-4 py-3 text-sm text-amber-700">
        <span>HTTP 请求日志已关闭</span>
        <a class="shrink-0 font-medium underline" href="/settings/general">前往通用设置</a>
      </div>
    {/if}

    <section class="tx-card overflow-hidden">
      <!-- 概览顶栏：HTTP 服务来源仅影响 HTTP 数据 -->
      <div class="flex flex-wrap items-center justify-between gap-3 border-b border-[var(--color-border)] px-5 py-3">
        <div class="flex items-center gap-2">
          <Activity size={15} class="text-[var(--color-accent)]" />
          <h3 class="font-semibold text-sm text-[var(--color-text)]">概览</h3>
        </div>
        <div class="flex items-center gap-2">
          <div class="flex rounded-lg border border-[var(--color-border)] bg-[var(--color-bg)] p-0.5">
            {#each accessServiceOptions as item (item.value)}
              <button
                type="button"
                aria-pressed={accessService === item.value}
                class="rounded-md px-2.5 py-1 text-xs font-medium {accessService === item.value
                  ? 'bg-[var(--color-surface)] text-[var(--color-accent)] shadow-sm'
                  : 'text-[var(--color-text-muted)] hover:text-[var(--color-text)]'}"
                onclick={() => {
                  accessService = item.value as AccessLogService;
                  void refreshData();
                }}
              >
                {item.label}
              </button>
            {/each}
          </div>
        </div>
      </div>

      <!-- 四个独立指标：HTTP 请求、HTTP 响应流量、工具调用与工具错误率 -->
      <div class="grid grid-cols-2 divide-x divide-y divide-[var(--color-border)] sm:divide-y-0 lg:grid-cols-4">
        <!-- 1. HTTP 请求 -->
        <div class="p-4 sm:p-5 flex flex-col justify-between">
          <div>
            <div class="flex items-center gap-1.5 text-xs text-[var(--color-text-muted)]">
              <Globe2 size={13} class="text-sky-500" />
              <span>HTTP 请求</span>
            </div>
            <p
              class="mt-2 font-mono text-2xl font-semibold text-[var(--color-text)]"
              title={`成功 ${httpSuccessCount} 次 · 失败 ${httpFailureCount} 次`}
            >
              {accessLogs.stats.totalRequests}
              <span class="text-xs font-normal text-[var(--color-text-muted)]">次</span>
            </p>
          </div>
        </div>

        <!-- 2. HTTP 响应流量 -->
        <div class="p-4 sm:p-5 flex flex-col justify-between">
          <div>
            <div class="flex items-center gap-1.5 text-xs text-[var(--color-text-muted)]">
              <Activity size={13} class="text-indigo-500" />
              <span>HTTP 流量</span>
            </div>
            <p class="mt-2 font-mono text-2xl font-semibold text-[var(--color-text)]">
              {formatBytes(accessLogs.stats.responseBytes)}
            </p>
          </div>
        </div>

        <!-- 3. 工具调用 -->
        <div class="p-4 sm:p-5 flex flex-col justify-between !border-t-0">
          <div>
            <div class="flex items-center gap-1.5 text-xs text-[var(--color-text-muted)]">
              <Database size={13} class="text-violet-500" />
              <span>工具调用</span>
            </div>
            <p
              class="mt-2 font-mono text-2xl font-semibold text-[var(--color-text)]"
              title={(stats?.totalCalls ?? 0) > 0
                ? `平均耗时 ${stats ? stats.averageDurationMs.toFixed(0) : 0}ms · 成功 ${stats?.successCalls ?? 0} 次 · 失败 ${toolErrorCount} 次`
                : `成功 ${stats?.successCalls ?? 0} 次 · 失败 ${toolErrorCount} 次`}
            >
              {stats ? stats.totalCalls : 0}
              <span class="text-xs font-normal text-[var(--color-text-muted)]">次</span>
            </p>
          </div>
        </div>

        <!-- 4. 错误率 -->
        <div class="p-4 sm:p-5 flex flex-col justify-between">
          <div>
            <div class="flex items-center gap-1.5 text-xs text-[var(--color-text-muted)]">
              {#if toolErrorSeverity.level === "safe" || toolErrorSeverity.level === "stable"}
                <CheckCircle2 size={13} class={toolErrorSeverity.iconColor} />
              {:else if toolErrorSeverity.level === "low"}
                <AlertTriangle size={13} class={toolErrorSeverity.iconColor} />
              {:else if toolErrorSeverity.level === "medium"}
                <AlertCircle size={13} class={toolErrorSeverity.iconColor} />
              {:else}
                <XCircle size={13} class={toolErrorSeverity.iconColor} />
              {/if}
              <span>错误率</span>
            </div>
            <p
              class="mt-2 font-mono text-2xl font-semibold {toolErrorSeverity.textColor}"
              title={`错误 ${stats?.failureCalls ?? 0} 次 · 拒绝 ${stats?.rejectedCalls ?? 0} 次`}
            >
              {toolErrorRate}%
            </p>
          </div>
        </div>

      </div>
    </section>

    <section class="tx-card min-w-0 overflow-hidden">
      <!-- 日志顶栏：左侧切换存储类型，右侧共享成功/失败筛选 -->
      <div class="flex flex-wrap items-center justify-between gap-3 border-b border-[var(--color-border)] px-5 py-3">
        <!-- 日志类型 -->
        <div class="flex items-center gap-1.5">
          <button
            type="button"
            class="inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-sm font-semibold transition-colors {activeTab === 'tool'
              ? 'bg-[var(--primary-soft)] text-[var(--color-accent)]'
              : 'text-[var(--color-text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--color-text)]'}"
            onclick={() => (activeTab = "tool")}
          >
            <Database size={15} />
            <span>工具日志</span>
          </button>

          <button
            type="button"
            class="inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-sm font-semibold transition-colors {activeTab === 'http'
              ? 'bg-[var(--primary-soft)] text-[var(--color-accent)]'
              : 'text-[var(--color-text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--color-text)]'}"
            onclick={() => (activeTab = "http")}
          >
            <Globe2 size={15} />
            <span>HTTP 日志</span>
          </button>
        </div>

        <!-- 两套日志共用状态筛选和目录操作 -->
        <div class="flex items-center gap-2">
          <!-- 状态筛选胶囊 -->
          <div class="flex rounded-lg border border-[var(--color-border)] bg-[var(--color-bg)] p-0.5">
            <button
              type="button"
              aria-pressed={statusFilter === "all"}
              class="rounded-md px-2.5 py-1 text-xs font-medium {statusFilter === 'all'
                ? 'bg-[var(--color-surface)] text-[var(--color-accent)] shadow-sm'
                : 'text-[var(--color-text-muted)] hover:text-[var(--color-text)]'}"
              onclick={() => changeStatusFilter("all")}
            >
              全部
            </button>
            <button
              type="button"
              aria-pressed={statusFilter === "success"}
              class="rounded-md px-2.5 py-1 text-xs font-medium {statusFilter === "success"
                ? 'bg-[var(--color-surface)] text-[var(--color-accent)] shadow-sm'
                : 'text-[var(--color-text-muted)] hover:text-[var(--color-text)]'}"
              onclick={() => changeStatusFilter("success")}
            >
              成功
            </button>
            <button
              type="button"
              aria-pressed={statusFilter === "failure"}
              class="rounded-md px-2.5 py-1 text-xs font-medium {statusFilter === "failure"
                ? 'bg-[var(--color-surface)] text-[var(--color-accent)] shadow-sm'
                : 'text-[var(--color-text-muted)] hover:text-[var(--color-text)]'}"
              onclick={() => changeStatusFilter("failure")}
            >
              失败
            </button>
          </div>

          <!-- 动作：打开日志目录（纯图标，悬停显示原生 tooltip） -->
          <button
            type="button"
            class="tx-btn-ghost !h-[30px] px-2.5 shrink-0 !rounded-md text-[var(--color-text-secondary)] hover:text-[var(--color-text)]"
            title="打开日志目录"
            aria-label="打开日志目录"
            onclick={() => void openAccessLogFolder()}
          >
            <FolderOpen size={14} />
          </button>
        </div>
      </div>

      <!-- 工具调用摘要；展开时延迟读取完整详情 -->
      {#if activeTab === "tool"}
        {#if records.length === 0}
          <p class="p-8 text-center text-sm text-[var(--color-text-muted)]">暂无记录</p>
        {:else}
          <!-- revision 负责筛选后整体收起；record.id 防止展开状态按数组位置串到其他记录。 -->
          {#key toolListRevision}
          <div class="min-w-0 max-h-[36rem] divide-y divide-[var(--color-border)] overflow-x-hidden overflow-y-auto">
            {#each records as record (record.id)}
              {@const detail = detailsById[record.id] ?? record}
              <details class="group min-w-0 transition-colors" ontoggle={(event) => void loadRecordDetail(event, record.id)}>
                <summary
                  class="flex cursor-pointer list-none items-center justify-between gap-3 px-5 py-3 transition-colors hover:bg-[var(--surface-hover)] select-none"
                >
                  <div class="flex min-w-0 flex-1 items-center gap-2.5">
                    <span
                      class="flex size-5 shrink-0 items-center justify-center text-[var(--color-text-muted)] transition-transform duration-200 group-open:rotate-90"
                    >
                      <ChevronRight size={15} />
                    </span>
                    <span class="truncate font-mono text-[13px] font-semibold text-[var(--color-text)]">
                      {record.toolName ?? "未知工具"}
                    </span>
                    {#if record.status !== "success"}
                      <span class="shrink-0 rounded-md border px-2 py-0.5 text-[11px] font-medium {statusClass(record.status)}">
                        {statusLabel(record.status)}
                      </span>
                    {/if}
                    {#if record.status !== "success" && recordAlertText(record)}
                      <span
                        class="hidden max-w-xs truncate text-xs sm:inline md:max-w-md lg:max-w-lg {record.status === 'rejected'
                          ? 'text-amber-600 dark:text-amber-400'
                          : 'text-rose-600 dark:text-rose-400'}"
                      >
                        {recordAlertText(record)}
                      </span>
                    {/if}
                  </div>

                  <div class="flex shrink-0 items-center text-xs text-[var(--color-text-muted)] font-mono">
                    <span>{formatTime(record.startedAtMs)}</span>
                  </div>
                </summary>

                <div class="grid min-w-0 gap-3.5 px-5 pb-5 pt-1">
                  {#if loadingDetails[record.id]}
                    <p class="flex items-center gap-2 py-3 text-xs text-[var(--color-text-muted)]">
                      <RefreshCw size={13} class="animate-spin" />读取详情中...
                    </p>
                  {:else}
                    <div class="grid gap-x-5 gap-y-3 rounded-xl border border-[var(--color-border)] bg-[var(--color-bg)]/60 p-4 text-xs grid-cols-2 sm:grid-cols-3 md:grid-cols-4">
                      <div class="flex flex-col gap-0.5">
                        <span class="text-[11px] text-[var(--color-text-muted)]">工作区</span>
                        <span class="truncate font-mono font-medium text-[var(--color-text)]">{workspaceName(detail.workspaceId)}</span>
                      </div>
                      <div class="flex flex-col gap-0.5">
                        <span class="text-[11px] text-[var(--color-text-muted)]">协议 ID</span>
                        <span class="font-mono font-medium text-[var(--color-text)]">{detail.requestId ?? "无"}</span>
                      </div>
                      <div class="flex flex-col gap-0.5">
                        <span class="text-[11px] text-[var(--color-text-muted)]">来源</span>
                        <span class="font-mono font-medium uppercase text-[var(--color-text)]">{detail.transport}</span>
                      </div>
                      <div class="flex flex-col gap-0.5">
                        <span class="text-[11px] text-[var(--color-text-muted)]">来源 IP</span>
                        <span class="font-mono font-medium text-[var(--color-text)]">{detail.forwardedIp ?? "无"}</span>
                      </div>
                      <!-- 指标沿用上方网格列宽，依次占前三格，与身份信息上下对齐。 -->
                      <div class="col-span-full grid grid-cols-2 gap-x-5 gap-y-3 sm:grid-cols-3 md:grid-cols-4">
                        <div class="flex flex-col gap-0.5">
                          <span class="text-[11px] text-[var(--color-text-muted)]">输入大小</span>
                          <span class="font-mono font-medium text-[var(--color-text)]">{formatBytes(detail.inputBytes)}{detail.inputTruncated ? " (已截断)" : ""}</span>
                        </div>
                        <div class="flex flex-col gap-0.5">
                          <span class="text-[11px] text-[var(--color-text-muted)]">输出大小</span>
                          <span class="font-mono font-medium text-[var(--color-text)]">{formatBytes(detail.outputBytes)}{detail.outputTruncated ? " (已截断)" : ""}</span>
                        </div>
                        <div class="flex flex-col gap-0.5">
                          <span class="text-[11px] text-[var(--color-text-muted)]">耗时</span>
                          <span class="font-mono font-medium text-[var(--color-text)]">{detail.durationMs} ms</span>
                        </div>
                      </div>
                      <div class="flex flex-col gap-0.5 border-t border-[var(--color-border)]/60 pt-2.5 col-span-2 sm:col-span-3 md:col-span-4">
                        <span class="text-[11px] text-[var(--color-text-muted)]">请求 ID</span>
                        <span class="break-all font-mono text-[var(--color-text-secondary)]">{detail.id}</span>
                      </div>
                      {#if detail.reason}
                        <div class="flex flex-col gap-0.5 border-t border-[var(--color-border)]/60 pt-2.5 col-span-2 sm:col-span-3 md:col-span-4">
                          <span class="text-[11px] text-[var(--color-text-muted)]">调用说明</span>
                          <span class="break-all font-mono leading-relaxed text-[var(--color-text-secondary)]">{detail.reason}</span>
                        </div>
                      {/if}
                      {#if detail.userAgent}
                        <div class="flex flex-col gap-0.5 border-t border-[var(--color-border)]/60 pt-2.5 col-span-2 sm:col-span-3 md:col-span-4">
                          <span class="text-[11px] text-[var(--color-text-muted)]">User-Agent</span>
                          <span class="break-all font-mono text-[var(--color-text-secondary)]">{detail.userAgent}</span>
                        </div>
                      {/if}
                    </div>

                    {#if detail.errorMessage}
                      <p class="flex items-start gap-2 rounded-xl border border-rose-500/20 bg-rose-500/10 p-3 text-xs text-rose-600 dark:text-rose-400">
                        <Ban size={14} class="mt-0.5 shrink-0 text-rose-500" />
                        <span class="font-medium">{detail.errorCode ?? "ERROR"}</span>
                        <span>{detail.errorMessage}</span>
                      </p>
                    {/if}

                    {#if detail.command}
                      <div class="min-w-0">
                        <p class="mb-1.5 text-xs font-medium text-[var(--color-text-secondary)]">执行命令</p>
                        <div class="group/code relative w-full min-w-0 max-w-full overflow-hidden rounded-xl border border-slate-800 bg-slate-950 shadow-inner">
                          <div class="absolute right-2.5 top-1/2 z-10 -translate-y-1/2 opacity-0 transition-opacity duration-150 group-hover/code:opacity-100 focus-within:opacity-100">
                            <button
                              type="button"
                              class="inline-flex items-center gap-1.5 rounded-md border border-slate-700/80 bg-slate-800/90 px-2.5 py-1 text-[11px] font-medium text-slate-200 shadow-md backdrop-blur hover:bg-slate-700 hover:text-white active:scale-95"
                              aria-label="复制执行命令"
                              onclick={(e) => {
                                e.stopPropagation();
                                void handleCopy(record.id + '-command', detail.command ?? '');
                              }}
                            >
                              {#if copiedKey === record.id + '-command'}
                                <Check size={12} class="text-emerald-400" />
                                <span class="text-emerald-400">已复制</span>
                              {:else}
                                <Copy size={12} />
                                <span>复制</span>
                              {/if}
                            </button>
                          </div>
                          <pre class="audit-code-scroll max-h-40 w-full min-w-0 max-w-full overflow-auto p-3.5 font-mono text-xs leading-relaxed text-slate-100">{detail.command}</pre>
                        </div>
                      </div>
                    {/if}

                    <div class="grid min-w-0 max-w-full gap-3.5 lg:grid-cols-2">
                      <div class="min-w-0">
                        <p class="mb-1.5 text-xs font-medium text-[var(--color-text-secondary)]">输入参数</p>
                        <div class="group/code relative w-full min-w-0 max-w-full overflow-hidden rounded-xl border border-slate-800 bg-slate-950 shadow-inner">
                          <div class="absolute right-2.5 top-2.5 z-10 opacity-0 transition-opacity duration-150 group-hover/code:opacity-100 focus-within:opacity-100">
                            <button
                              type="button"
                              class="inline-flex items-center gap-1.5 rounded-md border border-slate-700/80 bg-slate-800/90 px-2.5 py-1 text-[11px] font-medium text-slate-200 shadow-md backdrop-blur hover:bg-slate-700 hover:text-white active:scale-95"
                              aria-label="复制输入参数"
                              onclick={(e) => {
                                e.stopPropagation();
                                void handleCopy(record.id + '-input', formatJson(detail.inputJson));
                              }}
                            >
                              {#if copiedKey === record.id + '-input'}
                                <Check size={12} class="text-emerald-400" />
                                <span class="text-emerald-400">已复制</span>
                              {:else}
                                <Copy size={12} />
                                <span>复制</span>
                              {/if}
                            </button>
                          </div>
                          <pre class="audit-code-scroll max-h-72 w-full min-w-0 max-w-full overflow-auto p-3.5 font-mono text-xs leading-relaxed text-slate-100">{formatJson(detail.inputJson)}</pre>
                        </div>
                      </div>
                      <div class="min-w-0">
                        <p class="mb-1.5 text-xs font-medium text-[var(--color-text-secondary)]">返回结果</p>
                        <div class="group/code relative w-full min-w-0 max-w-full overflow-hidden rounded-xl border border-slate-800 bg-slate-950 shadow-inner">
                          <div class="absolute right-2.5 top-2.5 z-10 opacity-0 transition-opacity duration-150 group-hover/code:opacity-100 focus-within:opacity-100">
                            <button
                              type="button"
                              class="inline-flex items-center gap-1.5 rounded-md border border-slate-700/80 bg-slate-800/90 px-2.5 py-1 text-[11px] font-medium text-slate-200 shadow-md backdrop-blur hover:bg-slate-700 hover:text-white active:scale-95"
                              aria-label="复制返回结果"
                              onclick={(e) => {
                                e.stopPropagation();
                                void handleCopy(record.id + '-output', formatJson(detail.outputJson));
                              }}
                            >
                              {#if copiedKey === record.id + '-output'}
                                <Check size={12} class="text-emerald-400" />
                                <span class="text-emerald-400">已复制</span>
                              {:else}
                                <Copy size={12} />
                                <span>复制</span>
                              {/if}
                            </button>
                          </div>
                          <pre class="audit-code-scroll max-h-72 w-full min-w-0 max-w-full overflow-auto p-3.5 font-mono text-xs leading-relaxed text-slate-100">{formatJson(detail.outputJson)}</pre>
                        </div>
                      </div>
                    </div>
                  {/if}
                </div>
              </details>
            {/each}
          </div>
          {/key}
        {/if}
      {:else}
        <!-- HTTP 原始访问日志 -->
        {#if accessLogs.records.length === 0}
          <p class="p-8 text-center text-sm text-[var(--color-text-muted)]">暂无记录</p>
        {:else}
          <div
            class="http-log-scroll min-h-32 max-h-[36rem] divide-y divide-slate-800 overflow-y-auto bg-slate-950"
          >
            {#each accessLogs.records as record}
              <div class="px-5 py-3.5">
                <div class="mb-1.5 flex flex-wrap items-center gap-2 text-[11px]">
                  <span class="rounded-md bg-slate-800 px-2 py-0.5 font-medium text-slate-200">
                    {workspaceName(record.workspaceId)}
                  </span>
                  <span class="rounded-md bg-slate-800 px-2 py-0.5 font-medium uppercase text-slate-300">
                    {record.service}
                  </span>
                  <span class="rounded-md border px-2 py-0.5 font-semibold {httpStatusClass(record.status)}">
                    {record.status}
                  </span>
                  <span class="text-slate-500 font-mono">{formatTime(record.timestampMs)}</span>
                </div>
                <pre class="overflow-x-auto whitespace-pre-wrap break-all font-mono text-xs leading-relaxed text-slate-200">{record.raw}</pre>
              </div>
            {/each}
          </div>
        {/if}
      {/if}

      <!-- 获取上限位于内部滚动区之外，切换时由后端重新筛选后再限量 -->
      <div class="flex flex-wrap items-center justify-between gap-3 border-t border-[var(--color-border)] bg-[var(--color-bg)]/40 px-5 py-2.5 text-xs text-[var(--color-text-muted)]">
        <div>
          <span>已显示最近 <strong class="font-medium text-[var(--color-text)]">{activeTab === "tool" ? records.length : accessLogs.records.length}</strong> 条记录</span>
        </div>
        <div class="flex items-center gap-2">
          <select
            class="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2.5 py-1 text-xs font-medium text-[var(--color-text)] shadow-sm hover:border-[var(--color-accent)] focus:border-[var(--color-accent)] focus:outline-none transition-colors cursor-pointer"
            aria-label={activeTab === "tool" ? "工具调用显示数量" : "HTTP 日志显示数量"}
            value={activeRecordLimit}
            onchange={changeRecordLimit}
          >
            {#each recordLimitOptions as option}
              <option value={option}>最近 {option} 条</option>
            {/each}
          </select>
        </div>
      </div>
    </section>
  </div>
</section>
<div
  bind:this={auditPageScrollbar}
  class="audit-page-scrollbar"
  class:audit-page-scrollbar-visible={auditPageHasScrollbar}
  aria-hidden="true"
  onscroll={syncAuditPageScroll}
>
  <div class="audit-page-scrollbar-spacer" style:height={`${auditPageScrollHeight}px`}></div>
</div>
</div>

<style>
  /*
   * WKWebView 已将 overflow: overlay 视为普通 auto，原生滚动条会缩小内容宽度。
   * 外壳固定布局宽度，正文隐藏原生条；绝对定位的代理条覆盖在最右侧，并由脚本双向同步。
   */
  .audit-page-shell {
    position: relative;
    flex: 1;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  .audit-page-scroll {
    height: 100%;
    overflow-y: auto;
    scrollbar-width: none;
  }

  .audit-page-scroll::-webkit-scrollbar {
    width: 0;
    height: 0;
  }

  .audit-page-scrollbar {
    position: absolute;
    z-index: 50;
    top: 0;
    right: 0;
    bottom: 0;
    width: 6px;
    overflow-x: hidden;
    overflow-y: scroll;
    opacity: 0;
    pointer-events: none;
  }

  .audit-page-scrollbar-visible {
    opacity: 1;
    pointer-events: auto;
  }

  .audit-page-scrollbar-spacer {
    width: 1px;
  }

  summary {
    list-style: none;
  }
  summary::-webkit-details-marker {
    display: none;
  }

  /* 代码块只保留滚轮/触控横向滚动能力，隐藏轨道以避免轨道出现时改变块高度。 */
  .audit-code-scroll::-webkit-scrollbar:horizontal {
    height: 0;
  }

  /* HTTP 日志使用深色背景，单独提高内部滚动条的对比度。 */
  :global(.http-log-scroll) {
    scrollbar-color: rgba(148, 163, 184, 0.55) rgba(148, 163, 184, 0.08);
    scrollbar-width: thin;
  }

  :global(.http-log-scroll::-webkit-scrollbar-thumb) {
    background: rgba(148, 163, 184, 0.55);
  }

  :global(.http-log-scroll::-webkit-scrollbar-track) {
    background: rgba(148, 163, 184, 0.08);
  }
</style>
