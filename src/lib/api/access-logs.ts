import { invoke } from "@tauri-apps/api/core";

/** `all` 仅用于查询；每条实际记录的服务一定是 mcp 或 actions。 */
export type AccessLogService = "mcp" | "actions" | "all";

/**
 * HTTP 访问日志查询条件。
 * 时间范围为 `[fromMs, toMs)`；未指定工作区时查询当前全部工作区。`successful` 仅筛选
 * 明细，stats 始终统计相同工作区、服务和时间范围内的全部请求。
 */
export interface AccessLogQuery {
  workspaceId?: string;
  service?: AccessLogService;
  fromMs?: number;
  toMs?: number;
  successful?: boolean;
  limit?: number;
}

export interface AccessLogStats {
  /** 时间范围内全部可解析请求数，不受 successful 影响。 */
  totalRequests: number;
  status2xx: number;
  status3xx: number;
  status4xx: number;
  status5xx: number;
  responseBytes: number;
}

export interface AccessLogRecord {
  workspaceId: string;
  service: Exclude<AccessLogService, "all">;
  timestampMs: number;
  status: number;
  raw: string;
}

export interface AccessLogResult {
  stats: AccessLogStats;
  /** 后端已跨工作区按时间倒序并应用 limit，前端不应再次截断或排序。 */
  records: AccessLogRecord[];
}

/**
 * 查询 HTTP 日志与概览统计。文件遍历、状态筛选和数量上限均由后端完成。
 */
export async function queryHttpAccessLogs(query: AccessLogQuery = {}): Promise<AccessLogResult> {
  return invoke<AccessLogResult>("query_http_access_logs", { query });
}

/**
 * 打开日志总目录或指定工作区目录；后端会校验工作区 ID，并在目录不存在时创建它。
 */
export async function openHttpAccessLogDirectory(workspaceId?: string): Promise<void> {
  return invoke("open_http_access_log_directory", { workspaceId });
}
