import { invoke } from "@tauri-apps/api/core";

/** 后端持久化的完整审计配置；保存时必须提交全部字段，而不是局部补丁。 */
export interface AuditConfig {
  httpLogEnabled: boolean;
  httpLogRetentionDays: number;
  toolAuditEnabled: boolean;
  toolAuditRetentionDays: number;
  // -1=不保存正文，0=不限制，正数=每个输入/输出的保存上限。
  detailLimitBytes: number;
}

export interface ClearLogsResult {
  /** 被截断的普通文本日志文件数。 */
  logFiles: number;
  /** 从 SQLite 删除的工具审计记录数。 */
  auditRecords: number;
}

/**
 * 工具审计查询条件。
 * 时间范围为 `[fromMs, toMs)`；过滤在 limit 前执行。`successful=false` 同时包含 failure
 * 与 rejected，确保“失败 100 条”返回目标状态的最新 100 条。
 */
export interface AuditQuery {
  fromMs?: number;
  toMs?: number;
  workspaceId?: string;
  toolName?: string;
  transport?: string;
  status?: string;
  successful?: boolean;
  recordType?: string;
  limit?: number;
  offset?: number;
  /** false 时命令、输入和输出为 null；列表应保持 false，展开时改用 getAuditRecord。 */
  includeDetails?: boolean;
}

/** 工具调用的统一审计模型；MCP 与 Actions 使用相同字段和状态口径。 */
export interface AuditRecord {
  /** 审计 UUID：唯一、稳定，用于 UI keyed 列表与详情查询。 */
  id: string;
  recordType: string;
  parentId: string | null;
  startedAtMs: number;
  finishedAtMs: number;
  durationMs: number;
  workspaceId: string | null;
  workspacePath: string | null;
  transport: string;
  method: string | null;
  route: string | null;
  /** 协议载荷 ID：可能重复、缺失或固定为 0，不可替代 id。 */
  requestId: string | null;
  toolName: string | null;
  /** success=成功，failure=执行错误，rejected=策略/权限/安全拒绝。 */
  status: string;
  isError: boolean;
  reason: string | null;
  forwardedIp: string | null;
  userAgent: string | null;
  inputJson: string | null;
  outputJson: string | null;
  inputBytes: number;
  outputBytes: number;
  inputTruncated: boolean;
  outputTruncated: boolean;
  inputSha256: string | null;
  outputSha256: string | null;
  errorCode: string | null;
  errorMessage: string | null;
  exitCode: number | null;
  terminationReason: string | null;
  command: string | null;
  commandArgsJson: string | null;
}

export interface AuditStats {
  /** 分母包含 success、failure 和 rejected。 */
  totalCalls: number;
  successCalls: number;
  /** 已进入执行流程但返回错误的调用数。 */
  failureCalls: number;
  /** 在策略、权限或安全检查阶段被拒绝的调用数。 */
  rejectedCalls: number;
  successRate: number;
  averageDurationMs: number;
  inputBytes: number;
  outputBytes: number;
}

/** 读取当前进程共享的审计配置快照。 */
export async function getAuditConfig(): Promise<AuditConfig> {
  return invoke<AuditConfig>("get_audit_config");
}

/** 校验并整体保存配置；返回后端归一化后的持久化结果。 */
export async function setAuditConfig(config: AuditConfig): Promise<AuditConfig> {
  return invoke<AuditConfig>("set_audit_config", { config });
}

/** 同时清空普通文本日志与 SQLite 工具审计；返回两种介质各自的处理数量。 */
export async function clearAllLogs(): Promise<ClearLogsResult> {
  return invoke<ClearLogsResult>("clear_all_logs");
}

/**
 * 查询倒序审计记录。默认使用摘要模式，大字段为 null；筛选和数量限制由后端完成。
 */
export async function queryAuditRecords(query: AuditQuery = {}): Promise<AuditRecord[]> {
  return invoke<AuditRecord[]>("query_audit_records", { query });
}

/** 按审计 UUID 获取包含命令、输入和输出的完整记录；不存在时返回 null。 */
export async function getAuditRecord(id: string): Promise<AuditRecord | null> {
  return invoke<AuditRecord | null>("get_audit_record", { id });
}

/** 使用与明细相同的查询条件聚合工具调用统计。 */
export async function getAuditStats(query: AuditQuery = {}): Promise<AuditStats> {
  return invoke<AuditStats>("get_audit_stats", { query });
}
