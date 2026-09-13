//! HTTP 访问日志：按工作区、服务和本地日期写入 combined 格式，并支持跨文件聚合查询。
//! 这是旁路能力；配置读取、清理或写盘失败只能告警，不能改变真实 HTTP 响应。

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::HttpBody;
use axum::extract::{Request, State};
use axum::http::{header::CONTENT_LENGTH, Version};
use axum::middleware::Next;
use axum::response::Response;
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::audit::{client_ip_from_headers, header_text, AuditStore};
use crate::platform::platform;
use crate::tunnel::log_dir_for_profile;

const DEFAULT_ACCESS_LOG_RECORD_LIMIT: usize = 100;
const MAX_ACCESS_LOG_RECORD_LIMIT: usize = 1000;
// 合并多文件时周期性裁剪候选集，避免为“最新 N 条”保留全部历史记录。
const ACCESS_LOG_COMPACT_SLACK: usize = 128;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
/// HTTP 日志查询条件。
///
/// 时间采用左闭右开区间 `[from_ms, to_ms)`；`successful` 只影响返回明细，不改变同一
/// 工作区、服务和时间范围内的统计总量。`limit` 最终会被限制在 1..=1000。
pub struct AccessLogQuery {
    pub workspace_id: Option<String>,
    pub service: Option<String>,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub successful: Option<bool>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
/// 查询范围内的完整 HTTP 统计，不受成功/失败明细筛选影响。
pub struct AccessLogStats {
    pub total_requests: u64,
    pub status_2xx: u64,
    pub status_3xx: u64,
    pub status_4xx: u64,
    pub status_5xx: u64,
    pub response_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
/// 从 combined 日志行提取的索引字段与原始文本。
pub struct AccessLogRecord {
    pub workspace_id: String,
    pub service: String,
    pub timestamp_ms: i64,
    pub status: u16,
    #[serde(skip_serializing)]
    pub response_bytes: u64,
    // combined 时间戳只有秒精度；文件内行号用于同秒记录的稳定新旧顺序。
    #[serde(skip_serializing)]
    file_order: u64,
    pub raw: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessLogResult {
    pub stats: AccessLogStats,
    pub records: Vec<AccessLogRecord>,
}

#[derive(Clone)]
/// 单个工作区、单个服务的写入状态；克隆实例共享文件句柄与维护状态。
pub struct AccessLogState {
    profile_id: Arc<str>,
    service: &'static str,
    audit: Option<AuditStore>,
    write_gate: Arc<AsyncMutex<()>>,
    maintenance: Arc<Mutex<MaintenanceState>>,
    log_dir_override: Option<Arc<PathBuf>>,
}

#[derive(Default)]
// 文件句柄轮转和保留期清理由同一把锁串行化；AccessLogState 的所有克隆共享此状态。
struct MaintenanceState {
    last_cleanup_date: Option<NaiveDate>,
    last_retention_days: Option<u32>,
    open_file_date: Option<NaiveDate>,
    open_file: Option<File>,
}

impl AccessLogState {
    /// 创建中间件状态。`service` 必须是固定的 `mcp` 或 `actions`，以保证文件命名和查询一致。
    pub fn new(
        profile_id: impl Into<String>,
        service: &'static str,
        audit: Option<AuditStore>,
    ) -> Self {
        debug_assert!(matches!(service, "mcp" | "actions"));
        Self {
            profile_id: Arc::from(profile_id.into()),
            service,
            audit,
            write_gate: Arc::new(AsyncMutex::new(())),
            maintenance: Arc::new(Mutex::new(MaintenanceState::default())),
            log_dir_override: None,
        }
    }

    #[cfg(test)]
    fn for_test(log_dir: PathBuf, service: &'static str, audit: AuditStore) -> Self {
        let mut state = Self::new("test-profile", service, Some(audit));
        state.log_dir_override = Some(Arc::new(log_dir));
        state
    }

    /// 追加一行并维护按日文件句柄。
    ///
    /// 同一自然日复用已打开句柄；跨日或保留策略变化时先轮转/清理。锁覆盖整个状态转换，
    /// 防止并发请求重复打开文件或把行写入错误日期。
    fn append(&self, date: NaiveDate, retention_days: u32, line: &str) -> io::Result<()> {
        let mut maintenance = self
            .maintenance
            .lock()
            .map_err(|_| io::Error::other("access log lock poisoned"))?;
        let log_dir = self
            .log_dir_override
            .as_deref()
            .cloned()
            .unwrap_or_else(|| log_dir_for_profile(&self.profile_id));
        fs::create_dir_all(&log_dir)?;
        // 日期变化时必须先丢弃旧句柄，否则新请求会继续写入前一天的分片。
        if maintenance.open_file_date != Some(date) {
            maintenance.open_file = None;
            maintenance.open_file_date = None;
        }
        if maintenance.last_cleanup_date != Some(date)
            || maintenance.last_retention_days != Some(retention_days)
        {
            // 同一天修改保留天数也要重跑；失败不阻塞写入，下一次日期/配置变化会再尝试。
            if let Err(error) =
                cleanup_access_logs_in_dir(&log_dir, Some(self.service), retention_days, date)
            {
                eprintln!("access log retention cleanup failed: {error}");
            }
            maintenance.last_cleanup_date = Some(date);
            maintenance.last_retention_days = Some(retention_days);
        }
        if maintenance.open_file_date != Some(date) || maintenance.open_file.is_none() {
            let path = log_dir.join(access_log_file_name(self.service, date));
            maintenance.open_file = Some(OpenOptions::new().create(true).append(true).open(path)?);
            maintenance.open_file_date = Some(date);
        }
        writeln!(
            maintenance
                .open_file
                .as_mut()
                .ok_or_else(|| io::Error::other("access log file is unavailable"))?,
            "{line}"
        )
    }
}

/// 在下游响应完成后生成并持久化访问日志。
///
/// 异步门闩先按服务串行化写入，避免多个 blocking 任务在文件锁上堆积；实际文件操作仍在
/// blocking 线程池执行。等待写入使请求完成、监听器关闭和日志落盘共享同一生命周期边界。
/// 写入失败只输出错误日志，业务响应仍原样返回。
pub async fn middleware(
    State(state): State<AccessLogState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(audit) = state.audit.as_ref() else {
        return next.run(request).await;
    };
    let Ok(config) = audit.config() else {
        return next.run(request).await;
    };
    if !config.http_log_enabled {
        return next.run(request).await;
    }

    // Request 会被下游消费，因此先保存请求侧字段；状态码和响应大小只能在 next 返回后补齐。
    let method = request.method().as_str().to_string();
    let uri = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or_else(|| request.uri().path())
        .to_string();
    let version = http_version(request.version());
    let client_ip = client_ip_from_headers(request.headers()).unwrap_or_else(|| "-".into());
    let referer = header_text(request.headers(), "referer").unwrap_or_else(|| "-".into());
    let user_agent = header_text(request.headers(), "user-agent").unwrap_or_else(|| "-".into());

    let response = next.run(request).await;
    let completed_at = Local::now();
    let line = format_combined_line(
        &client_ip,
        &completed_at.format("%d/%b/%Y:%H:%M:%S %z").to_string(),
        &method,
        &uri,
        version,
        response.status().as_u16(),
        response_size(&response).unwrap_or(0),
        &referer,
        &user_agent,
    );
    let completed_date = completed_at.date_naive();
    let retention_days = config.http_log_retention_days;
    let write_gate = Arc::clone(&state.write_gate);
    let _write_guard = write_gate.lock().await;
    match tokio::task::spawn_blocking(move || state.append(completed_date, retention_days, &line))
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("access log write failed: {error}"),
        Err(error) => eprintln!("access log worker failed: {error}"),
    }
    response
}

#[allow(clippy::too_many_arguments)]
/// 生成与 Nginx combined 结构兼容的一行文本，供写入与解析器共同约定字段位置。
fn format_combined_line(
    client_ip: &str,
    timestamp: &str,
    method: &str,
    uri: &str,
    version: &str,
    status: u16,
    response_bytes: u64,
    referer: &str,
    user_agent: &str,
) -> String {
    format!(
        "{} - - [{}] \"{} {} {}\" {} {} \"{}\" \"{}\"",
        escape_log_value(client_ip),
        timestamp,
        escape_log_value(method),
        escape_log_value(uri),
        version,
        status,
        response_bytes,
        escape_log_value(referer),
        escape_log_value(user_agent)
    )
}

/// 将任意字段编码为单行安全 ASCII；输出可嵌入引号包围的 combined 字段。
fn escape_log_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b' '..=b'~' if !matches!(byte, b'"' | b'\\') => escaped.push(char::from(byte)),
            _ => escaped.push_str(&format!("\\x{byte:02X}")),
        }
    }
    escaped
}

fn http_version(version: Version) -> &'static str {
    match version {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_11 => "HTTP/1.1",
        Version::HTTP_2 => "HTTP/2.0",
        Version::HTTP_3 => "HTTP/3.0",
        _ => "HTTP/?",
    }
}

/// 返回可确定的响应体字节数；无法从响应头或精确 size hint 得到时返回 None。
fn response_size(response: &Response) -> Option<u64> {
    response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .or_else(|| response.body().size_hint().exact())
}

fn access_log_file_name(service: &str, date: NaiveDate) -> String {
    format!("{service}-access-{}.log", date.format("%Y-%m-%d"))
}

/// 合并查询多个工作区的日志。
///
/// 每个工作区可同时扫描 MCP 与 Actions 文件；结果在所有来源之间统一按时间倒序，最后只
/// 保留 `limit` 条。目录不存在视为空结果，I/O 错误则向上返回。
pub fn query_access_logs(
    workspace_ids: &[String],
    query: &AccessLogQuery,
) -> io::Result<AccessLogResult> {
    let root = access_log_directory(None)?;
    query_access_logs_from_root(&root, workspace_ids, query)
}

fn query_access_logs_from_root(
    root: &Path,
    workspace_ids: &[String],
    query: &AccessLogQuery,
) -> io::Result<AccessLogResult> {
    // 服务名决定允许扫描的文件前缀；拒绝未知值，避免把目录内其他日志混入 HTTP 统计。
    let services: &[&str] = match query.service.as_deref().unwrap_or("all") {
        "all" => &["mcp", "actions"],
        "mcp" => &["mcp"],
        "actions" => &["actions"],
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown access log service: {other}"),
            ))
        }
    };
    let record_limit = query
        .limit
        .unwrap_or(DEFAULT_ACCESS_LOG_RECORD_LIMIT as u32)
        .clamp(1, MAX_ACCESS_LOG_RECORD_LIMIT as u32) as usize;
    let mut result = AccessLogResult::default();
    for workspace_id in workspace_ids {
        for service in services {
            scan_access_log_directory(
                root,
                workspace_id,
                service,
                query,
                record_limit,
                &mut result,
            )?;
        }
    }
    trim_access_records(&mut result.records, record_limit);
    Ok(result)
}

/// 扫描一个“工作区 + 服务”的所有日期分片，并累加到共享结果。
///
/// 统计在状态筛选前更新；明细先应用时间/状态条件，再进入有界候选集。调用方因此可以得到
/// 全量概览，同时获得指定状态下真正最新的 N 条记录。
fn scan_access_log_directory(
    root: &Path,
    workspace_id: &str,
    service: &str,
    query: &AccessLogQuery,
    record_limit: usize,
    result: &mut AccessLogResult,
) -> io::Result<()> {
    let directory = workspace_log_directory(root, workspace_id)?;
    if !directory.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some((file_service, file_date)) = parse_access_log_name(&file_name) else {
            continue;
        };
        if file_service != service || !file_date_may_overlap(file_date, query) {
            continue;
        }
        // 文件名先做按日粗筛，行内时间再做毫秒级精筛；格式损坏的单行直接跳过。
        let file = File::open(entry.path())?;
        for (file_order, line) in BufReader::new(file).lines().enumerate() {
            let line = line?;
            let Some(mut record) = parse_access_log_line(workspace_id, service, line) else {
                continue;
            };
            record.file_order = file_order as u64;
            if query
                .from_ms
                .is_some_and(|from_ms| record.timestamp_ms < from_ms)
                || query
                    .to_ms
                    .is_some_and(|to_ms| record.timestamp_ms >= to_ms)
            {
                continue;
            }
            // 概览统计覆盖完整时间范围；成功/失败只筛选明细，且必须在 limit 之前生效。
            update_access_stats(&mut result.stats, &record);
            if !matches_success_filter(record.status, query.successful) {
                continue;
            }
            result.records.push(record);
            // 目录遍历无时间顺序，保留少量余量后重新排序裁剪，控制峰值内存。
            if result.records.len() >= record_limit.saturating_add(ACCESS_LOG_COMPACT_SLACK) {
                trim_access_records(&mut result.records, record_limit);
            }
        }
    }
    Ok(())
}

/// 将跨目录候选集收敛到最新 N 条；相同时间先按工作区和服务排序。
fn trim_access_records(records: &mut Vec<AccessLogRecord>, limit: usize) {
    records.sort_unstable_by(|left, right| {
        right
            .timestamp_ms
            .cmp(&left.timestamp_ms)
            .then_with(|| left.workspace_id.cmp(&right.workspace_id))
            .then_with(|| left.service.cmp(&right.service))
            .then_with(|| right.file_order.cmp(&left.file_order))
    });
    records.truncate(limit);
}

fn matches_success_filter(status: u16, successful: Option<bool>) -> bool {
    // 产品口径：2xx/3xx 为成功，4xx/5xx 为失败。
    match successful {
        None => true,
        Some(true) => matches!(status, 200..=399),
        Some(false) => matches!(status, 400..=599),
    }
}

/// 判断某个本地日期分片是否可能与查询区间相交。
///
/// 这里只做安全的粗筛；DST 或本地时间歧义无法确定时返回 true，交给逐行时间判断，避免
/// 错误跳过有效日志。
fn file_date_may_overlap(date: NaiveDate, query: &AccessLogQuery) -> bool {
    // 文件按本地自然日分片；查询区间统一为 [from_ms, to_ms)。
    let Some(start) = date
        .and_hms_opt(0, 0, 0)
        .and_then(|value| Local.from_local_datetime(&value).earliest())
    else {
        return true;
    };
    let Some(next_start) = date
        .succ_opt()
        .and_then(|value| value.and_hms_opt(0, 0, 0))
        .and_then(|value| Local.from_local_datetime(&value).earliest())
    else {
        return true;
    };
    query
        .from_ms
        .is_none_or(|from_ms| next_start.timestamp_millis() > from_ms)
        && query
            .to_ms
            .is_none_or(|to_ms| start.timestamp_millis() < to_ms)
}

fn update_access_stats(stats: &mut AccessLogStats, record: &AccessLogRecord) {
    stats.total_requests = stats.total_requests.saturating_add(1);
    stats.response_bytes = stats.response_bytes.saturating_add(record.response_bytes);
    match record.status {
        200..=299 => stats.status_2xx = stats.status_2xx.saturating_add(1),
        300..=399 => stats.status_3xx = stats.status_3xx.saturating_add(1),
        400..=499 => stats.status_4xx = stats.status_4xx.saturating_add(1),
        500..=599 => stats.status_5xx = stats.status_5xx.saturating_add(1),
        _ => {}
    }
}

/// 解析本模块生成的 combined 行。
///
/// 任一结构或数值字段不合法即返回 None，让查询跳过单条脏数据；原始行仍完整保存在成功
/// 解析的记录中，供 UI 无损展示。
fn parse_access_log_line(
    workspace_id: &str,
    service: &str,
    raw: String,
) -> Option<AccessLogRecord> {
    let mut quoted = raw.split('"');
    let prefix = quoted.next()?.trim();
    let request = quoted.next()?;
    let response = quoted.next()?.trim();
    quoted.next()?;
    quoted.next()?;
    quoted.next()?;

    let client_ip = prefix.split_whitespace().next()?;
    let timestamp = prefix.strip_prefix(client_ip)?.trim();
    let timestamp = timestamp.strip_prefix("- - [")?.strip_suffix(']')?;
    let timestamp_ms = DateTime::parse_from_str(timestamp, "%d/%b/%Y:%H:%M:%S %z")
        .ok()?
        .timestamp_millis();

    let mut request_parts = request.split_whitespace();
    request_parts.next()?;
    request_parts.next()?;
    request_parts.next()?;
    if request_parts.next().is_some() {
        return None;
    }

    let mut response_parts = response.split_whitespace();
    let status = response_parts.next()?.parse().ok()?;
    let response_bytes = response_parts.next()?.parse().ok()?;

    Some(AccessLogRecord {
        workspace_id: workspace_id.to_string(),
        service: service.to_string(),
        timestamp_ms,
        status,
        response_bytes,
        file_order: 0,
        raw,
    })
}

/// 返回日志总目录，或经过路径安全校验的指定工作区目录。
pub fn access_log_directory(profile_id: Option<&str>) -> io::Result<PathBuf> {
    let root = platform()
        .app_config_dir()
        .map_err(|error| io::Error::other(error.to_string()))?
        .join("logs");
    match profile_id {
        Some(profile_id) => workspace_log_directory(&root, profile_id),
        None => Ok(root),
    }
}

/// 删除一个工作区的整个日志目录；目录不存在时返回 false。
pub fn remove_workspace_logs(profile_id: &str) -> io::Result<bool> {
    let root = access_log_directory(None)?;
    remove_workspace_logs_from_root(&root, profile_id)
}

fn remove_workspace_logs_from_root(root: &Path, profile_id: &str) -> io::Result<bool> {
    let directory = workspace_log_directory(root, profile_id)?;
    if !directory.exists() {
        return Ok(false);
    }
    fs::remove_dir_all(directory)?;
    Ok(true)
}

fn workspace_log_directory(root: &Path, profile_id: &str) -> io::Result<PathBuf> {
    validate_profile_id(profile_id)?;
    Ok(root.join(profile_id))
}

fn validate_profile_id(profile_id: &str) -> io::Result<()> {
    // ID 会成为目录名：只接受单个普通路径分量，拒绝绝对路径、`..` 和嵌套路径。
    let mut components = Path::new(profile_id).components();
    let valid = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !profile_id.is_empty();
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid workspace id for log directory",
        ))
    }
}

/// 清理全部工作区中过期的 MCP/Actions 日期分片，返回删除文件数。
///
/// `retention_days == 0` 表示永久保留，不执行目录扫描。
pub fn cleanup_all_access_logs(retention_days: u32) -> io::Result<usize> {
    if retention_days == 0 {
        return Ok(0);
    }
    let root = platform()
        .app_config_dir()
        .map_err(|error| io::Error::other(error.to_string()))?
        .join("logs");
    if !root.is_dir() {
        return Ok(0);
    }
    let today = Local::now().date_naive();
    let mut removed = 0;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            removed += cleanup_access_logs_in_dir(&entry.path(), None, retention_days, today)?;
        }
    }
    Ok(removed)
}

/// 递归截断日志总目录内的所有文件，返回处理文件数。
///
/// 该操作涵盖 access、stdout/stderr 与 cloudflared 日志；保留路径与 inode，确保现有写入
/// 句柄无需重启即可继续产生日志。
pub fn clear_all_log_files() -> io::Result<usize> {
    let root = platform()
        .app_config_dir()
        .map_err(|error| io::Error::other(error.to_string()))?
        .join("logs");
    clear_log_files_in_directory(&root)
}

fn clear_log_files_in_directory(directory: &Path) -> io::Result<usize> {
    if !directory.is_dir() {
        return Ok(0);
    }
    let mut cleared = 0;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            cleared += clear_log_files_in_directory(&entry.path())?;
        } else if file_type.is_file() {
            // 截断而非删除：长期运行的日志写入者仍持有原句柄，删除会让新日志写入不可见 inode。
            OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(entry.path())?;
            cleared += 1;
        }
    }
    Ok(cleared)
}

/// 按文件名日期删除指定服务的过期访问日志；非 access 文件和其他服务文件保持不变。
fn cleanup_access_logs_in_dir(
    directory: &Path,
    service: Option<&str>,
    retention_days: u32,
    today: NaiveDate,
) -> io::Result<usize> {
    if retention_days == 0 || !directory.is_dir() {
        return Ok(0);
    }
    // 保留天数包含今天；N 天对应从 today-(N-1) 起的日期分片。
    let keep_from = today - ChronoDuration::days(i64::from(retention_days.saturating_sub(1)));
    let mut removed = 0;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some((file_service, date)) = parse_access_log_name(&name) else {
            continue;
        };
        if service.is_some_and(|expected| expected != file_service) || date >= keep_from {
            continue;
        }
        fs::remove_file(entry.path())?;
        removed += 1;
    }
    Ok(removed)
}

fn parse_access_log_name(name: &str) -> Option<(&str, NaiveDate)> {
    let (service, rest) = if let Some(rest) = name.strip_prefix("mcp-access-") {
        ("mcp", rest)
    } else if let Some(rest) = name.strip_prefix("actions-access-") {
        ("actions", rest)
    } else {
        return None;
    };
    let date = rest.strip_suffix(".log")?;
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .map(|date| (service, date))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use chrono::NaiveDate;
    use tempfile::tempdir;
    use tower::ServiceExt;

    use super::{
        cleanup_access_logs_in_dir, clear_log_files_in_directory, escape_log_value,
        format_combined_line, middleware as access_middleware, parse_access_log_name,
        query_access_logs_from_root, remove_workspace_logs_from_root, AccessLogQuery,
        AccessLogState,
    };
    use crate::audit::AuditStore;

    #[test]
    fn combined_line_matches_nginx_shape() {
        let line = format_combined_line(
            "203.0.113.8",
            "10/Sep/2026:14:22:31 +0800",
            "GET",
            "/.env?probe=1",
            "HTTP/1.1",
            404,
            67,
            "-",
            "curl/8.7.1",
        );
        assert_eq!(
            line,
            "203.0.113.8 - - [10/Sep/2026:14:22:31 +0800] \"GET /.env?probe=1 HTTP/1.1\" 404 67 \"-\" \"curl/8.7.1\""
        );
    }

    #[test]
    fn unsafe_bytes_are_escaped_without_breaking_the_line() {
        assert_eq!(escape_log_value("a\"b\\c\n"), "a\\x22b\\x5Cc\\x0A");
    }

    #[test]
    fn daily_retention_keeps_requested_number_of_dates() {
        let dir = tempdir().expect("temp dir");
        for name in [
            "mcp-access-2026-09-08.log",
            "mcp-access-2026-09-09.log",
            "mcp-access-2026-09-10.log",
            "stdout.log",
        ] {
            fs::write(dir.path().join(name), "test\n").expect("write log");
        }
        let today = NaiveDate::from_ymd_opt(2026, 9, 10).expect("date");
        let removed =
            cleanup_access_logs_in_dir(dir.path(), Some("mcp"), 2, today).expect("cleanup");
        assert_eq!(removed, 1);
        assert!(!dir.path().join("mcp-access-2026-09-08.log").exists());
        assert!(dir.path().join("mcp-access-2026-09-09.log").exists());
        assert!(dir.path().join("stdout.log").exists());
    }

    #[test]
    fn parser_ignores_non_access_logs() {
        assert!(parse_access_log_name("stderr.log").is_none());
        assert!(parse_access_log_name("mcp-access-2026-09-10.log").is_some());
    }

    #[tokio::test]
    async fn middleware_logs_an_unmatched_scanner_path() {
        let dir = tempdir().expect("temp dir");
        let audit = AuditStore::open(dir.path().join("audit.sqlite")).expect("audit store");
        let log_dir = dir.path().join("logs");
        let state = AccessLogState::for_test(log_dir.clone(), "mcp", audit);
        let app = Router::new().route("/known", get(|| async { "ok" })).layer(
            axum::middleware::from_fn_with_state(state, access_middleware),
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/.env?scanner=1")
                    .header("x-forwarded-for", "203.0.113.8")
                    .header("user-agent", "scanner-test")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/known")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        // 中间件返回即代表本次日志已经落盘，无需轮询后台任务。
        let log_path = fs::read_dir(log_dir)
            .expect("log dir")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("log"))
            .expect("access log");
        let content = fs::read_to_string(log_path).expect("read access log");
        assert!(content.contains("203.0.113.8 - - ["));
        assert!(content.contains("\"GET /.env?scanner=1 HTTP/1.1\" 404"));
        assert!(content.contains("\"scanner-test\""));
        assert!(content.contains("\"GET /known HTTP/1.1\" 200 2"));
    }

    #[tokio::test]
    async fn disabled_http_logging_does_not_create_a_file() {
        let dir = tempdir().expect("temp dir");
        let audit = AuditStore::open(dir.path().join("audit.sqlite")).expect("audit store");
        let mut config = audit.config().expect("config");
        config.http_log_enabled = false;
        audit.set_config(config).expect("disable HTTP log");
        let log_dir = dir.path().join("logs");
        let state = AccessLogState::for_test(log_dir.clone(), "mcp", audit);
        let app = Router::new().route("/known", get(|| async { "ok" })).layer(
            axum::middleware::from_fn_with_state(state, access_middleware),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/known")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!log_dir.exists());
    }

    #[test]
    fn workspace_log_cleanup_removes_only_the_exact_workspace_directory() {
        let dir = tempdir().expect("temp dir");
        let target = dir.path().join("workspace-a");
        let sibling = dir.path().join("workspace-b");
        fs::create_dir_all(&target).expect("target dir");
        fs::create_dir_all(&sibling).expect("sibling dir");
        fs::write(target.join("mcp-access-2026-09-10.log"), "request\n").expect("target log");
        fs::write(sibling.join("keep.log"), "keep\n").expect("sibling log");

        assert!(remove_workspace_logs_from_root(dir.path(), "workspace-a").expect("cleanup"));
        assert!(!target.exists());
        assert!(sibling.join("keep.log").exists());
        assert!(remove_workspace_logs_from_root(dir.path(), "../workspace-b").is_err());
        assert!(sibling.exists());
    }

    #[test]
    fn clearing_logs_truncates_files_and_keeps_active_writers_usable() {
        let dir = tempdir().expect("temp dir");
        let workspace = dir.path().join("workspace-a");
        fs::create_dir_all(&workspace).expect("workspace dir");
        let active_path = workspace.join("cloudflared.log");
        fs::write(&active_path, "old tunnel log\n").expect("active log");
        fs::write(workspace.join("stdout.log"), "old stdout\n").expect("stdout log");
        fs::write(
            workspace.join("mcp-access-2026-09-10.log"),
            "old access log\n",
        )
        .expect("access log");
        let mut active = OpenOptions::new()
            .append(true)
            .open(&active_path)
            .expect("open active writer");

        assert_eq!(clear_log_files_in_directory(dir.path()).expect("clear logs"), 3);
        assert_eq!(fs::metadata(&active_path).expect("active metadata").len(), 0);
        assert_eq!(
            fs::metadata(workspace.join("stdout.log"))
                .expect("stdout metadata")
                .len(),
            0
        );
        active.write_all(b"new tunnel log\n").expect("append after clear");
        assert_eq!(
            fs::read_to_string(active_path).expect("read active log"),
            "new tunnel log\n"
        );
    }

    #[test]
    fn access_log_query_merges_workspaces_and_keeps_full_stats() {
        let dir = tempdir().expect("temp dir");
        let workspace_a = dir.path().join("workspace-a");
        let workspace_b = dir.path().join("workspace-b");
        fs::create_dir_all(&workspace_a).expect("workspace a");
        fs::create_dir_all(&workspace_b).expect("workspace b");
        let first = format_combined_line(
            "192.0.2.1",
            "10/Sep/2026:10:00:00 +0800",
            "GET",
            "/.env",
            "HTTP/1.1",
            404,
            0,
            "-",
            "scanner",
        );
        let second = format_combined_line(
            "192.0.2.2",
            "10/Sep/2026:10:01:00 +0800",
            "POST",
            "/mcp",
            "HTTP/1.1",
            200,
            120,
            "-",
            "client-a",
        );
        let third = format_combined_line(
            "192.0.2.3",
            "10/Sep/2026:10:02:00 +0800",
            "POST",
            "/actions/read_file",
            "HTTP/1.1",
            200,
            80,
            "-",
            "client-b",
        );
        fs::write(
            workspace_a.join("mcp-access-2026-09-10.log"),
            format!("{first}\n{second}\n"),
        )
        .expect("mcp log");
        fs::write(
            workspace_b.join("actions-access-2026-09-10.log"),
            format!("{third}\n"),
        )
        .expect("actions log");

        let workspace_ids = vec!["workspace-a".to_string(), "workspace-b".to_string()];
        let result =
            query_access_logs_from_root(dir.path(), &workspace_ids, &AccessLogQuery::default())
                .expect("query logs");
        assert_eq!(result.stats.total_requests, 3);
        assert_eq!(result.stats.status_2xx, 2);
        assert_eq!(result.stats.status_4xx, 1);
        assert_eq!(result.stats.response_bytes, 200);
        assert_eq!(result.records.len(), 3);
        assert_eq!(result.records[0].workspace_id, "workspace-b");
        assert!(result.records[0].raw.contains("POST /actions/read_file"));
    }

    #[test]
    fn access_log_query_keeps_the_latest_hundred_same_second_records() {
        let dir = tempdir().expect("temp dir");
        let workspace = dir.path().join("workspace-a");
        fs::create_dir_all(&workspace).expect("workspace");
        let mut content = String::new();
        for index in 0..150 {
            let line = format_combined_line(
                "192.0.2.1",
                "10/Sep/2026:10:00:00 +0800",
                "GET",
                &format!("/scanner/{index}?padding={}", "x".repeat(64)),
                "HTTP/1.1",
                404,
                0,
                "-",
                "security-scanner",
            );
            content.push_str(&line);
            content.push('\n');
        }
        let path = workspace.join("mcp-access-2026-09-10.log");
        fs::write(&path, content).expect("access log");

        let result = query_access_logs_from_root(
            dir.path(),
            &["workspace-a".to_string()],
            &AccessLogQuery::default(),
        )
        .expect("query logs");
        assert_eq!(result.stats.total_requests, 150);
        assert_eq!(result.records.len(), 100);
        assert!(result.records[0].raw.contains("/scanner/149?"));
        assert!(result.records[99].raw.contains("/scanner/50?"));

        let expanded = query_access_logs_from_root(
            dir.path(),
            &["workspace-a".to_string()],
            &AccessLogQuery {
                limit: Some(200),
                ..AccessLogQuery::default()
            },
        )
        .expect("query expanded logs");
        assert_eq!(expanded.records.len(), 150);
        assert!(expanded.records[149].raw.contains("/scanner/0?"));
    }

    #[test]
    fn access_log_query_filters_status_before_limit() {
        let dir = tempdir().expect("temp dir");
        let workspace = dir.path().join("workspace-a");
        fs::create_dir_all(&workspace).expect("workspace");
        let mut content = String::new();
        for (second, status) in [(0, 404), (1, 500), (2, 200), (3, 302)] {
            let line = format_combined_line(
                "192.0.2.1",
                &format!("10/Sep/2026:10:00:{second:02} +0800"),
                "GET",
                &format!("/status/{status}"),
                "HTTP/1.1",
                status,
                0,
                "-",
                "test-client",
            );
            content.push_str(&line);
            content.push('\n');
        }
        fs::write(
            workspace.join("mcp-access-2026-09-10.log"),
            content,
        )
        .expect("access log");

        let failures = query_access_logs_from_root(
            dir.path(),
            &["workspace-a".to_string()],
            &AccessLogQuery {
                successful: Some(false),
                limit: Some(1),
                ..AccessLogQuery::default()
            },
        )
        .expect("query failures");
        assert_eq!(failures.stats.total_requests, 4);
        assert_eq!(failures.records.len(), 1);
        assert_eq!(failures.records[0].status, 500);

        let successes = query_access_logs_from_root(
            dir.path(),
            &["workspace-a".to_string()],
            &AccessLogQuery {
                successful: Some(true),
                limit: Some(100),
                ..AccessLogQuery::default()
            },
        )
        .expect("query successes");
        assert_eq!(successes.records.len(), 2);
        assert_eq!(successes.records[0].status, 302);
        assert_eq!(successes.records[1].status, 200);
    }
}
