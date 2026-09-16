//! MCP/Actions 共用的调用审计：SQLite 存储、详情裁剪、查询与保留期清理。
//! 列表读取摘要，展开记录时按 UUID 加载命令与输入/输出。
//!
//! ## 分类口径
//!
//! 统计“调用是否正常完成”，不表示“被检查的代码已通过检查”。
//!
//! | 结果 | 审计状态 |
//! | --- | --- |
//! | 正常搜索，包括无匹配 | success |
//! | 检查完成，发现代码或格式问题 | success（规则待实现） |
//! | 启动、参数/配置、文件权限错误，崩溃或超时 | failure |
//! | 安全/权限策略拒绝 | rejected |
//!
//! 退出码按程序与选项解释，不能仅凭收到响应或非零退出判定成败。
//! 例如默认 ruff check：1 表示发现问题，2 表示检查器异常。
//!
//! 当前仅接入 grep 无匹配修正，具体边界见 grep_outcome。
//! 保留原始 exit_code、command_ok 与 stdout/stderr；running 仅表示已受理。
//! 本次不新增状态或 UI 标签，不重算历史记录。

mod grep_outcome;

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::HeaderMap;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const DEFAULT_DETAIL_LIMIT_BYTES: i64 = 256 * 1024;
const MILLIS_PER_DAY: i64 = 86_400_000;
const MAX_RETENTION_DAYS: u32 = 36_500;

// 进程级单例确保所有入口共享同一连接、配置快照和清理状态。
static DEFAULT_AUDIT_STORE: Mutex<Option<AuditStore>> = Mutex::new(None);

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("audit storage is unavailable: {0}")]
    Unavailable(String),
    #[error("audit database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("audit JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("audit storage lock is poisoned")]
    Poisoned,
    #[error("detail_limit_bytes must be -1, 0, or a positive integer")]
    InvalidDetailLimit,
    #[error("invalid audit configuration value: {0}")]
    InvalidConfig(String),
    #[error("retention days must be between 0 and {MAX_RETENTION_DAYS}")]
    InvalidRetentionDays,
}

pub type AuditResult<T> = Result<T, AuditError>;

#[derive(Clone)]
/// 线程安全的审计存储句柄。
///
/// 克隆只复制 Arc；所有调用方共享一个 SQLite 连接、配置快照和每日清理标记。
pub struct AuditStore {
    // Connection 需串行访问；配置独立缓存于 RwLock，避免每次调用查询 SQLite。
    connection: Arc<Mutex<Connection>>,
    config: Arc<RwLock<AuditConfig>>,
    last_cleanup_day: Arc<AtomicI64>,
    // 仅审计分类共享的有界会话类型缓存，不保存命令正文。
    grep_sessions: Arc<Mutex<grep_outcome::GrepSessions>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// 同时控制 HTTP 文本日志与工具审计的开关、保留期和详情保存策略。
pub struct AuditConfig {
    pub http_log_enabled: bool,
    pub http_log_retention_days: u32,
    pub tool_audit_enabled: bool,
    pub tool_audit_retention_days: u32,
    // -1=不保存正文，0=不限制，正数=每个输入/输出允许保存的最大字节数。
    pub detail_limit_bytes: i64,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            http_log_enabled: true,
            http_log_retention_days: 30,
            tool_audit_enabled: true,
            tool_audit_retention_days: 30,
            detail_limit_bytes: DEFAULT_DETAIL_LIMIT_BYTES,
        }
    }
}

#[derive(Debug, Default)]
pub struct AuditRequestContext {
    pub transport: String,
    pub method: Option<String>,
    // 协议载荷 ID，可能重复或固定为 0；不能充当审计记录的唯一身份。
    pub request_id: Option<String>,
    pub route: Option<String>,
    pub forwarded_ip: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
/// 工具审计查询条件。时间为左闭右开区间，过滤在排序和 limit 之前执行。
pub struct AuditQuery {
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub workspace_id: Option<String>,
    pub tool_name: Option<String>,
    pub transport: Option<String>,
    pub status: Option<String>,
    // false 同时匹配 failure（执行错误）与 rejected（策略拒绝）。
    pub successful: Option<bool>,
    pub record_type: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    // 列表保持 false；单条详情查询才读取大字段。
    #[serde(default)]
    pub include_details: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRecord {
    // 审计层生成的唯一 UUID；UI 中“请求 ID”和 keyed 列表均使用此字段。
    pub id: String,
    pub record_type: String,
    pub parent_id: Option<String>,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub duration_ms: i64,
    pub workspace_id: Option<String>,
    pub workspace_path: Option<String>,
    pub transport: String,
    pub method: Option<String>,
    pub route: Option<String>,
    // 上游协议 ID，仅用于关联原始请求。
    pub request_id: Option<String>,
    pub tool_name: Option<String>,
    // success / failure / rejected 三态由 response_metadata 统一推导。
    pub status: String,
    pub is_error: bool,
    pub reason: Option<String>,
    pub forwarded_ip: Option<String>,
    pub user_agent: Option<String>,
    pub input_json: Option<String>,
    pub output_json: Option<String>,
    pub input_bytes: i64,
    pub output_bytes: i64,
    pub input_truncated: bool,
    pub output_truncated: bool,
    pub input_sha256: Option<String>,
    pub output_sha256: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub exit_code: Option<i64>,
    pub termination_reason: Option<String>,
    pub command: Option<String>,
    pub command_args_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AuditStats {
    pub total_calls: i64,
    pub success_calls: i64,
    pub failure_calls: i64,
    pub rejected_calls: i64,
    pub success_rate: f64,
    pub average_duration_ms: f64,
    pub input_bytes: i64,
    pub output_bytes: i64,
}

#[derive(Debug, Clone)]
struct CapturedDetail {
    // 正文被禁用或截断时，原始大小与哈希仍完整保留。
    text: Option<String>,
    bytes: i64,
    truncated: bool,
    sha256: String,
}

impl AuditStore {
    /// 获取进程级默认存储。
    ///
    /// 首次调用创建配置目录、打开数据库并执行启动清理；后续调用返回共享句柄。初始化错误
    /// 不会写入缓存，因此下一次调用仍可重试。
    pub fn open_default() -> AuditResult<Self> {
        let mut cached = DEFAULT_AUDIT_STORE
            .lock()
            .map_err(|_| AuditError::Poisoned)?;
        if let Some(store) = cached.as_ref() {
            return Ok(store.clone());
        }
        let root = crate::platform::platform()
            .app_config_dir()
            .map_err(|error| AuditError::Unavailable(error.to_string()))?
            .join("audit");
        let store = Self::open(root.join("audit.sqlite"))?;
        // HTTP 日志不在 SQLite 中，启动清理需在默认存储初始化后单独触发。
        if let Ok(config) = store.config() {
            if let Err(error) =
                crate::access_log::cleanup_all_access_logs(config.http_log_retention_days)
            {
                eprintln!("access log retention cleanup failed during startup: {error}");
            }
        }
        *cached = Some(store.clone());
        Ok(store)
    }

    /// 打开指定数据库，幂等初始化表、索引和缺失配置，并立即应用当前工具审计保留期。
    /// 测试使用独立路径调用此函数，避免写入用户真实审计库。
    pub fn open(path: PathBuf) -> AuditResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| AuditError::Unavailable(error.to_string()))?;
        }
        let connection = Connection::open(&path)?;
        // WAL + busy_timeout 降低监听器并发读写时的锁冲突；建表和建索引均须保持幂等。
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS audit_config (
                 key TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS audit_records (
                 id TEXT PRIMARY KEY NOT NULL,
                 record_type TEXT NOT NULL,
                 parent_id TEXT,
                 started_at_ms INTEGER NOT NULL,
                 finished_at_ms INTEGER NOT NULL,
                 duration_ms INTEGER NOT NULL,
                 workspace_id TEXT,
                 workspace_path TEXT,
                 transport TEXT NOT NULL,
                 method TEXT,
                 route TEXT,
                 request_id TEXT,
                 tool_name TEXT,
                 status TEXT NOT NULL,
                 is_error INTEGER NOT NULL DEFAULT 0,
                 reason TEXT,
                 forwarded_ip TEXT,
                 user_agent TEXT,
                 input_json TEXT,
                 output_json TEXT,
                 input_bytes INTEGER NOT NULL DEFAULT 0,
                 output_bytes INTEGER NOT NULL DEFAULT 0,
                 input_truncated INTEGER NOT NULL DEFAULT 0,
                 output_truncated INTEGER NOT NULL DEFAULT 0,
                 input_sha256 TEXT,
                 output_sha256 TEXT,
                 error_code TEXT,
                 error_message TEXT,
                 exit_code INTEGER,
                 termination_reason TEXT,
                 command TEXT,
                 command_args_json TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_audit_started_at
                 ON audit_records(started_at_ms);
             CREATE INDEX IF NOT EXISTS idx_audit_tool_name
                 ON audit_records(tool_name, started_at_ms);
             CREATE INDEX IF NOT EXISTS idx_audit_status
                 ON audit_records(status, started_at_ms);
             CREATE INDEX IF NOT EXISTS idx_audit_workspace
                 ON audit_records(workspace_id, started_at_ms);
             CREATE INDEX IF NOT EXISTS idx_audit_transport
                 ON audit_records(transport, started_at_ms);",
        )?;
        // 仅补缺失键，升级时不得覆盖用户已保存的配置。
        let defaults = AuditConfig::default();
        for (key, value) in config_entries(&defaults) {
            connection.execute(
                "INSERT OR IGNORE INTO audit_config(key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        let config = load_config(&connection)?;
        let store = Self {
            connection: Arc::new(Mutex::new(connection)),
            config: Arc::new(RwLock::new(config.clone())),
            last_cleanup_day: Arc::new(AtomicI64::new(-1)),
            grep_sessions: Arc::new(Mutex::new(grep_outcome::GrepSessions::default())),
        };
        if let Err(error) =
            store.cleanup_expired_records(config.tool_audit_retention_days, current_time_ms(), true)
        {
            eprintln!("audit retention cleanup failed during startup: {error}");
        }
        Ok(store)
    }

    /// 返回内存中的配置快照，不访问 SQLite。
    pub fn config(&self) -> AuditResult<AuditConfig> {
        self.config
            .read()
            .map(|config| config.clone())
            .map_err(|_| AuditError::Poisoned)
    }

    /// 原子保存全部配置，并在提交成功后更新内存快照。
    ///
    /// 工具审计保留期立即强制执行；HTTP 文件清理由命令层单独触发，因为它不属于 SQLite。
    pub fn set_config(&self, config: AuditConfig) -> AuditResult<AuditConfig> {
        validate_config(&config)?;
        // 先提交数据库事务再替换内存快照，保证持久化失败时读者仍看到完整旧配置。
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        for (key, value) in config_entries(&config) {
            transaction.execute(
                "INSERT INTO audit_config(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        *self.config.write().map_err(|_| AuditError::Poisoned)? = config.clone();
        if let Err(error) =
            self.cleanup_expired_records(config.tool_audit_retention_days, current_time_ms(), true)
        {
            eprintln!("audit retention cleanup failed after settings update: {error}");
        }
        Ok(config)
    }

    #[allow(clippy::too_many_arguments)]
    /// 记录一次已完成的工具调用。
    ///
    /// 返回 Some(UUID) 表示成功落库；审计关闭时返回 None。输入/输出先按配置计算完整元数据
    /// 并裁剪正文，再从结构化输出统一判定 success、failure 或 rejected。
    pub fn record_tool_call(
        &self,
        request: &AuditRequestContext,
        workspace_id: &str,
        workspace_path: &str,
        tool_name: &str,
        args: &Value,
        output: &Value,
        started_at_ms: i64,
        finished_at_ms: i64,
    ) -> AuditResult<Option<String>> {
        let config = self.config()?;
        if !config.tool_audit_enabled {
            return Ok(None);
        }
        self.cleanup_expired_records(config.tool_audit_retention_days, finished_at_ms, false)?;
        // MCP 与 Actions 共用同一捕获和结果分类链路，防止两个入口产生不同统计口径。
        let details = capture_pair(args, output, config.detail_limit_bytes)?;
        let response = grep_outcome::classify(
            &self.grep_sessions, workspace_id, &request.transport, tool_name, args, output,
        );
        // command 是输入参数的派生展示；输入正文未保存或已截断时不得单独保留完整命令。
        let command = details
            .0
            .text
            .as_ref()
            .filter(|_| !details.0.truncated)
            .and_then(|_| args.get("cmd"))
            .and_then(Value::as_str);
        let id = Uuid::new_v4().simple().to_string();
        self.insert_record(&AuditInsert {
            id: &id,
            record_type: "tool",
            parent_id: None,
            started_at_ms,
            finished_at_ms,
            workspace_id: Some(workspace_id),
            workspace_path: Some(workspace_path),
            request,
            tool_name: Some(tool_name),
            status: response.status,
            is_error: response.is_error,
            reason: args.get("reason").and_then(Value::as_str),
            input: details.0,
            output: details.1,
            error_code: response.error_code.as_deref(),
            error_message: response.error_message.as_deref(),
            exit_code: response.exit_code,
            termination_reason: response.termination_reason.as_deref(),
            command,
            command_args_json: None,
        })?;
        Ok(Some(id))
    }

    #[allow(clippy::too_many_arguments)]
    /// 记录未进入工具执行阶段的拒绝事件。
    ///
    /// 拒绝被包装为标准 policy 输出后交给 record_tool_call，确保详情限制、状态统计和 UUID
    /// 生成与正常调用完全一致。
    pub fn record_tool_rejection(
        &self,
        request: &AuditRequestContext,
        workspace_id: &str,
        workspace_path: &str,
        tool_name: &str,
        args: &Value,
        error_code: &str,
        error_message: &str,
    ) -> AuditResult<Option<String>> {
        let now = current_time_ms();
        self.record_tool_call(
            request,
            workspace_id,
            workspace_path,
            tool_name,
            args,
            &json!({
                "ok": false,
                "error": {
                    "code": error_code,
                    "category": "policy",
                    "message": error_message
                }
            }),
            now,
            now,
        )
    }

    /// 按开始时间倒序查询明细。
    ///
    /// 默认返回 100 条，最大 1000 条。include_details=false 时大字段由 SQL 直接替换为 NULL，
    /// 从数据库读取阶段就避免拷贝命令、输入和输出。
    pub fn query(&self, query: &AuditQuery) -> AuditResult<Vec<AuditRecord>> {
        let config = self.config()?;
        self.cleanup_expired_records(config.tool_audit_retention_days, current_time_ms(), false)?;
        let connection = self.lock()?;
        // WHERE 在 LIMIT 前生成，因此“失败 N 条”是目标状态的最新 N 条，而非先取 N 条再筛选。
        let mut sql = record_select_sql(query.include_details);
        sql.push_str(" WHERE 1 = 1");
        let mut values = Vec::<SqlValue>::new();
        append_filters(&mut sql, &mut values, query);
        sql.push_str(" ORDER BY started_at_ms DESC LIMIT ? OFFSET ?");
        values.push(SqlValue::Integer(
            query.limit.unwrap_or(100).min(1000) as i64
        ));
        values.push(SqlValue::Integer(query.offset.unwrap_or(0) as i64));
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), read_record)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AuditError::from)
    }

    /// 按审计 UUID 获取单条完整记录；不存在返回 None，不使用协议 request_id 查询。
    pub fn get_record(&self, id: &str) -> AuditResult<Option<AuditRecord>> {
        let config = self.config()?;
        self.cleanup_expired_records(config.tool_audit_retention_days, current_time_ms(), false)?;
        let connection = self.lock()?;
        let mut sql = record_select_sql(true);
        sql.push_str(" WHERE id = ?1");
        connection
            .query_row(&sql, [id], read_record)
            .optional()
            .map_err(AuditError::from)
    }

    /// 仅删除指定工作区 ID 的记录；共享目录的其他 profile 必须保持独立。
    pub fn remove_workspace_records(&self, workspace_id: &str) -> AuditResult<usize> {
        self.lock()?
            .execute(
                "DELETE FROM audit_records WHERE workspace_id = ?1",
                params![workspace_id],
            )
            .map_err(AuditError::from)
    }

    /// 删除全部审计记录并回收数据库空间，但保留 audit_config。
    pub fn clear_records(&self) -> AuditResult<usize> {
        let connection = self.lock()?;
        let removed = connection.execute("DELETE FROM audit_records", [])?;
        // 配置表不清空；checkpoint + VACUUM 只回收记录占用的 WAL/数据库空间。
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(removed)
    }

    /// 使用与明细相同的工作区、时间、工具和传输过滤条件聚合统计。
    /// success_rate 的分母包含成功、错误和拒绝；无记录时返回 0.0。
    pub fn stats(&self, query: &AuditQuery) -> AuditResult<AuditStats> {
        let config = self.config()?;
        self.cleanup_expired_records(config.tool_audit_retention_days, current_time_ms(), false)?;
        let connection = self.lock()?;
        let mut sql = String::from(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'failure' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'rejected' THEN 1 ELSE 0 END), 0),
                    COALESCE(AVG(duration_ms), 0),
                    COALESCE(SUM(input_bytes), 0), COALESCE(SUM(output_bytes), 0)
             FROM audit_records WHERE record_type = 'tool'",
        );
        let mut values = vec![];
        append_filters(&mut sql, &mut values, query);
        let row = connection.query_row(&sql, params_from_iter(values), |row| {
            let total_calls: i64 = row.get(0)?;
            let success_calls: i64 = row.get(1)?;
            Ok(AuditStats {
                total_calls,
                success_calls,
                failure_calls: row.get(2)?,
                rejected_calls: row.get(3)?,
                success_rate: if total_calls == 0 {
                    0.0
                } else {
                    success_calls as f64 / total_calls as f64
                },
                average_duration_ms: row.get(4)?,
                input_bytes: row.get(5)?,
                output_bytes: row.get(6)?,
            })
        })?;
        Ok(row)
    }

    /// 将已经完成分类和裁剪的记录写入固定列结构；此处不再执行业务判断。
    fn insert_record(&self, record: &AuditInsert<'_>) -> AuditResult<()> {
        let connection = self.lock()?;
        // 防御异常时钟顺序，持久化耗时不得为负数。
        let duration_ms = record.finished_at_ms.saturating_sub(record.started_at_ms);
        connection.execute(
            "INSERT INTO audit_records(
                id, record_type, parent_id, started_at_ms, finished_at_ms, duration_ms,
                workspace_id, workspace_path, transport, method, route, request_id, tool_name,
                status, is_error, reason, forwarded_ip, user_agent, input_json, output_json,
                input_bytes, output_bytes, input_truncated, output_truncated,
                input_sha256, output_sha256, error_code, error_message, exit_code,
                termination_reason, command, command_args_json
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31,
                ?32
             )",
            params![
                record.id,
                record.record_type,
                record.parent_id,
                record.started_at_ms,
                record.finished_at_ms,
                duration_ms,
                record.workspace_id,
                record.workspace_path,
                record.request.transport,
                record.request.method,
                record.request.route,
                record.request.request_id,
                record.tool_name,
                &record.status,
                record.is_error,
                record.reason,
                record.request.forwarded_ip,
                record.request.user_agent,
                record.input.text.as_deref(),
                record.output.text.as_deref(),
                record.input.bytes,
                record.output.bytes,
                record.input.truncated,
                record.output.truncated,
                &record.input.sha256,
                &record.output.sha256,
                record.error_code,
                record.error_message,
                record.exit_code,
                record.termination_reason,
                record.command,
                record.command_args_json.as_deref(),
            ],
        )?;
        Ok(())
    }

    fn lock(&self) -> AuditResult<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| AuditError::Poisoned)
    }

    /// 删除超出保留窗口的记录。
    ///
    /// 普通读写调用共享原子日标记，避免高频执行 DELETE；启动和配置更新使用 force 绕过
    /// 节流。retention_days=0 表示永久保留。
    fn cleanup_expired_records(
        &self,
        retention_days: u32,
        now_ms: i64,
        force: bool,
    ) -> AuditResult<()> {
        if retention_days == 0 {
            return Ok(());
        }
        // 热路径每天至多清理一次；失败时复位标记，使后续调用仍可重试。
        let day = now_ms.div_euclid(MILLIS_PER_DAY);
        if !force && self.last_cleanup_day.swap(day, Ordering::Relaxed) == day {
            return Ok(());
        }
        let cutoff = now_ms.saturating_sub(i64::from(retention_days) * MILLIS_PER_DAY);
        let result = self.lock()?.execute(
            "DELETE FROM audit_records WHERE started_at_ms < ?1",
            [cutoff],
        );
        if result.is_err() {
            self.last_cleanup_day.store(-1, Ordering::Relaxed);
        }
        result?;
        self.last_cleanup_day.store(day, Ordering::Relaxed);
        Ok(())
    }
}

struct AuditInsert<'a> {
    id: &'a str,
    record_type: &'a str,
    parent_id: Option<&'a str>,
    started_at_ms: i64,
    finished_at_ms: i64,
    workspace_id: Option<&'a str>,
    workspace_path: Option<&'a str>,
    request: &'a AuditRequestContext,
    tool_name: Option<&'a str>,
    status: String,
    is_error: bool,
    reason: Option<&'a str>,
    input: CapturedDetail,
    output: CapturedDetail,
    error_code: Option<&'a str>,
    error_message: Option<&'a str>,
    exit_code: Option<i64>,
    termination_reason: Option<&'a str>,
    command: Option<&'a str>,
    command_args_json: Option<String>,
}

fn config_entries(config: &AuditConfig) -> [(&'static str, String); 5] {
    [
        ("http_log_enabled", config.http_log_enabled.to_string()),
        (
            "http_log_retention_days",
            config.http_log_retention_days.to_string(),
        ),
        ("tool_audit_enabled", config.tool_audit_enabled.to_string()),
        (
            "tool_audit_retention_days",
            config.tool_audit_retention_days.to_string(),
        ),
        ("detail_limit_bytes", config.detail_limit_bytes.to_string()),
    ]
}

/// 从键值表完整加载并校验配置；任一键损坏都拒绝构造部分有效的运行时配置。
fn load_config(connection: &Connection) -> AuditResult<AuditConfig> {
    let config = AuditConfig {
        http_log_enabled: read_bool_config(connection, "http_log_enabled")?,
        http_log_retention_days: read_u32_config(connection, "http_log_retention_days")?,
        tool_audit_enabled: read_bool_config(connection, "tool_audit_enabled")?,
        tool_audit_retention_days: read_u32_config(connection, "tool_audit_retention_days")?,
        detail_limit_bytes: read_i64_config(connection, "detail_limit_bytes")?,
    };
    validate_config(&config)?;
    Ok(config)
}

fn read_config_value(connection: &Connection, key: &str) -> AuditResult<String> {
    connection
        .query_row(
            "SELECT value FROM audit_config WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .map_err(AuditError::from)
}

fn read_bool_config(connection: &Connection, key: &str) -> AuditResult<bool> {
    match read_config_value(connection, key)?.as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        value => Err(AuditError::InvalidConfig(format!("{key}={value}"))),
    }
}

fn read_u32_config(connection: &Connection, key: &str) -> AuditResult<u32> {
    let value = read_config_value(connection, key)?;
    value
        .parse()
        .map_err(|_| AuditError::InvalidConfig(format!("{key}={value}")))
}

fn read_i64_config(connection: &Connection, key: &str) -> AuditResult<i64> {
    let value = read_config_value(connection, key)?;
    let value = value
        .parse()
        .map_err(|_| AuditError::InvalidConfig(format!("{key}={value}")))?;
    validate_detail_limit(value)?;
    Ok(value)
}

fn capture_pair(
    input: &Value,
    output: &Value,
    limit: i64,
) -> AuditResult<(CapturedDetail, CapturedDetail)> {
    Ok((capture_value(input, limit)?, capture_value(output, limit)?))
}

/// 生成摘要或详情查询的公共 SELECT。
///
/// 两种模式列数与顺序完全相同；摘要模式仅将四个大字段替换为 NULL，避免维护两套行映射。
fn record_select_sql(include_details: bool) -> String {
    let input_json = if include_details {
        "input_json"
    } else {
        "NULL"
    };
    let output_json = if include_details {
        "output_json"
    } else {
        "NULL"
    };
    let command_args_json = if include_details {
        "command_args_json"
    } else {
        "NULL"
    };
    let command = if include_details { "command" } else { "NULL" };
    format!(
        "SELECT id, record_type, parent_id, started_at_ms, finished_at_ms, duration_ms,
                workspace_id, workspace_path, transport, method, route, request_id, tool_name,
                status, is_error, reason, forwarded_ip, user_agent, {input_json}, {output_json},
                input_bytes, output_bytes, input_truncated, output_truncated,
                input_sha256, output_sha256, error_code, error_message, exit_code,
                termination_reason, {command}, {command_args_json}
         FROM audit_records"
    )
}

#[derive(Debug)]
struct ResponseMetadata {
    status: String,
    is_error: bool,
    error_code: Option<String>,
    error_message: Option<String>,
    exit_code: Option<i64>,
    termination_reason: Option<String>,
}

/// 基础三态：工具、传输或命令失败按错误类别记为 failure/rejected。
/// 已完成的 kill_session 保持成功；grep 无匹配例外由 grep_outcome 修正。
fn response_metadata(tool_name: &str, output: &Value) -> ResponseMetadata {
    let successful_session_kill = tool_name == "kill_session"
        && matches!(
            output.get("status").and_then(Value::as_str),
            Some("killed" | "exited")
        );
    let is_error = output.get("ok").and_then(Value::as_bool) == Some(false)
        || output.get("transport_ok").and_then(Value::as_bool) == Some(false)
        || (!successful_session_kill
            && output.get("command_ok").and_then(Value::as_bool) == Some(false));
    let category = output
        .get("error")
        .and_then(|value| value.get("category"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let status = if !is_error {
        "success"
    } else if matches!(category, "policy" | "permission" | "security") {
        "rejected"
    } else {
        "failure"
    };
    ResponseMetadata {
        status: status.to_string(),
        is_error,
        error_code: output
            .get("error")
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string),
        error_message: output
            .get("error")
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                output
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .filter(|_| is_error),
        exit_code: output.get("exit_code").and_then(Value::as_i64),
        termination_reason: output
            .get("termination_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// 捕获单个输入或输出。
///
/// 无论正文策略如何，均基于完整 JSON 计算字节数和 SHA-256。正数上限超出时保存一个
/// 合法 JSON 截断信封，而不是不可解析的原始字节前缀。
fn capture_value(value: &Value, limit: i64) -> AuditResult<CapturedDetail> {
    validate_detail_limit(limit)?;
    let bytes = serde_json::to_vec(value)?;
    let size = bytes.len() as i64;
    let sha256 = sha256_hex(&bytes);
    if limit == -1 {
        return Ok(CapturedDetail {
            text: None,
            bytes: size,
            truncated: false,
            sha256,
        });
    }
    if limit == 0 || size <= limit {
        return Ok(CapturedDetail {
            text: Some(String::from_utf8(bytes).map_err(|error| {
                AuditError::Unavailable(format!("serialized audit value is not UTF-8: {error}"))
            })?),
            bytes: size,
            truncated: false,
            sha256,
        });
    }
    // 截断结果仍包装成合法 JSON，并保留完整原文哈希；preview 必须停在 UTF-8 边界。
    let prefix_len = utf8_prefix_len(&bytes, limit as usize);
    let preview = String::from_utf8_lossy(&bytes[..prefix_len]).into_owned();
    let text = serde_json::to_string(&json!({
        "_audit_truncated": true,
        "original_bytes": size,
        "sha256": sha256,
        "preview": preview
    }))?;
    Ok(CapturedDetail {
        text: Some(text),
        bytes: size,
        truncated: true,
        sha256,
    })
}

/// 将查询条件追加到 WHERE，并同步维护参数顺序。
///
/// 外部值全部使用参数绑定；successful=false 使用 `status <> 'success'`，因此同时包含错误与
/// 拒绝，且筛选天然发生在 ORDER BY/LIMIT 之前。
fn append_filters(sql: &mut String, values: &mut Vec<SqlValue>, query: &AuditQuery) {
    // 所有外部值均参数绑定；时间范围统一为 [from_ms, to_ms)。
    if let Some(value) = query.from_ms {
        sql.push_str(" AND started_at_ms >= ?");
        values.push(SqlValue::Integer(value));
    }
    if let Some(value) = query.to_ms {
        sql.push_str(" AND started_at_ms < ?");
        values.push(SqlValue::Integer(value));
    }
    if let Some(value) = query.workspace_id.as_deref() {
        sql.push_str(" AND workspace_id = ?");
        values.push(SqlValue::Text(value.to_string()));
    }
    if let Some(value) = query.tool_name.as_deref() {
        sql.push_str(" AND tool_name = ?");
        values.push(SqlValue::Text(value.to_string()));
    }
    if let Some(value) = query.transport.as_deref() {
        sql.push_str(" AND transport = ?");
        values.push(SqlValue::Text(value.to_string()));
    }
    if let Some(value) = query.status.as_deref() {
        sql.push_str(" AND status = ?");
        values.push(SqlValue::Text(value.to_string()));
    }
    if let Some(successful) = query.successful {
        if successful {
            sql.push_str(" AND status = 'success'");
        } else {
            sql.push_str(" AND status <> 'success'");
        }
    }
    if let Some(value) = query.record_type.as_deref() {
        sql.push_str(" AND record_type = ?");
        values.push(SqlValue::Text(value.to_string()));
    }
}

fn read_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditRecord> {
    // 下标必须与 record_select_sql 的列序同步；摘要模式只替换值，不改变位置。
    Ok(AuditRecord {
        id: row.get(0)?,
        record_type: row.get(1)?,
        parent_id: row.get(2)?,
        started_at_ms: row.get(3)?,
        finished_at_ms: row.get(4)?,
        duration_ms: row.get(5)?,
        workspace_id: row.get(6)?,
        workspace_path: row.get(7)?,
        transport: row.get(8)?,
        method: row.get(9)?,
        route: row.get(10)?,
        request_id: row.get(11)?,
        tool_name: row.get(12)?,
        status: row.get(13)?,
        is_error: row.get(14)?,
        reason: row.get(15)?,
        forwarded_ip: row.get(16)?,
        user_agent: row.get(17)?,
        input_json: row.get(18)?,
        output_json: row.get(19)?,
        input_bytes: row.get(20)?,
        output_bytes: row.get(21)?,
        input_truncated: row.get(22)?,
        output_truncated: row.get(23)?,
        input_sha256: row.get(24)?,
        output_sha256: row.get(25)?,
        error_code: row.get(26)?,
        error_message: row.get(27)?,
        exit_code: row.get(28)?,
        termination_reason: row.get(29)?,
        command: row.get(30)?,
        command_args_json: row.get(31)?,
    })
}

fn validate_detail_limit(limit: i64) -> AuditResult<()> {
    if limit < -1 {
        Err(AuditError::InvalidDetailLimit)
    } else {
        Ok(())
    }
}

fn validate_config(config: &AuditConfig) -> AuditResult<()> {
    validate_detail_limit(config.detail_limit_bytes)?;
    if config.http_log_retention_days > MAX_RETENTION_DAYS
        || config.tool_audit_retention_days > MAX_RETENTION_DAYS
    {
        return Err(AuditError::InvalidRetentionDays);
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 返回不超过 limit 且不会切断 UTF-8 多字节字符的前缀长度。
/// capture_value 只在 `0 < limit < bytes.len()` 时调用，因此可直接检查边界字节。
fn utf8_prefix_len(bytes: &[u8], limit: usize) -> usize {
    let mut end = limit.min(bytes.len());
    while end > 0 && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
        end -= 1;
    }
    end
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn current_time_ms() -> i64 {
    now_ms()
}

/// 将入口协议字段与代理头归一化为审计上下文；不在此生成审计 UUID。
pub fn request_context_from_headers(
    headers: &HeaderMap,
    transport: impl Into<String>,
    method: Option<String>,
    request_id: Option<String>,
    route: Option<String>,
) -> AuditRequestContext {
    AuditRequestContext {
        transport: transport.into(),
        method,
        request_id,
        route,
        forwarded_ip: client_ip_from_headers(headers),
        user_agent: header_text(headers, "user-agent"),
    }
}

pub(crate) fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// 按隧道类型提取客户端 IP，并兼容代理链、RFC Forwarded 与 IPv4/IPv6 端口写法。
///
/// 出现 `cf-*` 头时优先读取 `CF-Connecting-IP`；其余情况按常见转发头回退。
/// FRP HTTP 代理会把连接来源追加到 XFF 末端，因此 XFF 必须读取最右侧值。
pub(crate) fn client_ip_from_headers(headers: &HeaderMap) -> Option<String> {
    if headers.keys().any(|name| name.as_str().starts_with("cf-")) {
        return header_text(headers, "cf-connecting-ip")
            .map(|value| normalize_forwarded_value(&value))
            .or_else(|| forwarded_header_ip(headers));
    }

    forwarded_header_ip(headers)
}

fn forwarded_header_ip(headers: &HeaderMap) -> Option<String> {
    header_text(headers, "x-forwarded-for")
        .and_then(|value| {
            value
                .rsplit(',')
                .find(|part| !part.trim().is_empty())
                .map(normalize_forwarded_value)
        })
        .or_else(|| {
            header_text(headers, "forwarded").and_then(|value| {
                let proxy = value.rsplit(',').find(|part| !part.trim().is_empty())?;
                proxy.split(';').find_map(|item| {
                    let (key, value) = item.trim().split_once('=')?;
                    key.eq_ignore_ascii_case("for")
                        .then(|| normalize_forwarded_value(value))
                })
            })
        })
        .or_else(|| {
            header_text(headers, "x-real-ip").map(|value| normalize_forwarded_value(&value))
        })
}

fn normalize_forwarded_value(value: &str) -> String {
    // 先识别 [IPv6]:port；裸 IPv6 不能按最后一个冒号误删地址段。
    let value = value.trim().trim_matches('"');
    if let Some(rest) = value.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return rest[..end].to_string();
        }
    }
    let mut parts = value.rsplitn(2, ':');
    let suffix = parts.next().unwrap_or_default();
    let prefix = parts.next();
    if prefix.is_some()
        && !prefix.unwrap_or_default().contains(':')
        && suffix.chars().all(|ch| ch.is_ascii_digit())
    {
        return prefix.unwrap_or_default().to_string();
    }
    value.to_string()
}

/// 字符串 ID 保留原文，数字等其他非 null ID 保留 JSON 表示；该值不承诺唯一性。
pub fn request_id_from_value(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        _ => Some(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        capture_value, current_time_ms, normalize_forwarded_value, request_context_from_headers,
        request_id_from_value, AuditQuery, AuditStore, DEFAULT_DETAIL_LIMIT_BYTES, MILLIS_PER_DAY,
    };
    use axum::http::{HeaderMap, HeaderValue};
    use serde_json::json;

    #[test]
    fn detail_capture_supports_unlimited_and_disabled_modes() {
        let value = json!({"text": "hello 世界"});
        let unlimited = capture_value(&value, 0).expect("unlimited detail");
        assert!(!unlimited.truncated);
        assert!(unlimited.text.is_some());

        let disabled = capture_value(&value, -1).expect("disabled detail");
        assert!(!disabled.truncated);
        assert!(disabled.text.is_none());
        assert!(disabled.bytes > 0);
    }

    #[test]
    fn detail_capture_keeps_valid_json_when_truncated() {
        let value = json!({"text": "这是一个很长的文本"});
        let captured = capture_value(&value, 8).expect("truncated detail");
        assert!(captured.truncated);
        let stored: serde_json::Value = serde_json::from_str(captured.text.as_deref().unwrap())
            .expect("truncated representation remains JSON");
        assert_eq!(stored["_audit_truncated"], true);
    }

    #[test]
    fn sqlite_store_records_queries_and_stats() {
        let dir = tempdir().expect("temp dir");
        let store = AuditStore::open(dir.path().join("audit.sqlite")).expect("open audit db");
        assert_eq!(
            store.config().expect("config").detail_limit_bytes,
            DEFAULT_DETAIL_LIMIT_BYTES
        );
        assert!(store.config().expect("config").http_log_enabled);
        assert_eq!(store.config().expect("config").http_log_retention_days, 30);
        assert!(store.config().expect("config").tool_audit_enabled);
        assert_eq!(store.config().expect("config").tool_audit_retention_days, 30);
        let mut config = store.config().expect("config");
        config.detail_limit_bytes = 0;
        store.set_config(config).expect("set config");
        let request = super::AuditRequestContext {
            transport: "mcp".into(),
            method: Some("tools/call".into()),
            request_id: Some("1".into()),
            route: Some("/mcp".into()),
            forwarded_ip: None,
            user_agent: Some("test".into()),
        };
        let now = current_time_ms();
        store
            .record_tool_call(
                &request,
                "workspace",
                "/tmp/workspace",
                "read_file",
                &json!({"path":"README.md", "reason":"inspect"}),
                &json!({"ok":true, "content":"ok"}),
                now,
                now + 20,
            )
            .expect("record");
        let records = store.query(&AuditQuery::default()).expect("query");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason.as_deref(), Some("inspect"));
        assert!(records[0].input_json.is_none());
        assert!(store
            .get_record(&records[0].id)
            .expect("detail")
            .and_then(|record| record.input_json)
            .is_some());
        let stats = store.stats(&AuditQuery::default()).expect("stats");
        assert_eq!(stats.success_calls, 1);
        assert_eq!(stats.success_rate, 1.0);
    }

    #[test]
    fn clearing_records_keeps_audit_configuration() {
        let dir = tempdir().expect("temp dir");
        let store = AuditStore::open(dir.path().join("audit.sqlite")).expect("open audit db");
        let mut config = store.config().expect("config");
        config.http_log_retention_days = 7;
        store.set_config(config.clone()).expect("set config");
        store
            .record_tool_call(
                &super::AuditRequestContext::default(),
                "workspace",
                "/tmp/workspace",
                "read_file",
                &json!({"path":"README.md"}),
                &json!({"ok":true}),
                100,
                101,
            )
            .expect("record");

        assert_eq!(store.clear_records().expect("clear records"), 1);
        assert!(store
            .query(&AuditQuery::default())
            .expect("query")
            .is_empty());
        assert_eq!(
            store.config().expect("config").http_log_retention_days,
            7
        );
    }

    #[test]
    fn request_context_uses_frp_xff_tail_and_keeps_reason_optional() {
        let mut headers = HeaderMap::new();
        headers.insert("forwarded_ip", HeaderValue::from_static("192.0.2.1"));
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.2, 203.0.113.7"),
        );
        headers.insert("user-agent", HeaderValue::from_static("audit-test"));
        let request = request_context_from_headers(
            &headers,
            "mcp",
            Some("tools/call".into()),
            Some("1".into()),
            Some("/mcp".into()),
        );
        assert_eq!(request.forwarded_ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(request.user_agent.as_deref(), Some("audit-test"));

        let dir = tempdir().expect("temp dir");
        let store = AuditStore::open(dir.path().join("audit.sqlite")).expect("open audit db");
        let now = current_time_ms();
        store
            .record_tool_call(
                &request,
                "workspace",
                "/tmp/workspace",
                "list_dir",
                &json!({"path":"."}),
                &json!({"ok":true}),
                now,
                now + 1,
            )
            .expect("record");
        let record = store
            .query(&AuditQuery::default())
            .expect("query")
            .remove(0);
        assert_eq!(record.reason, None);
    }

    #[test]
    fn cloudflare_prefers_connecting_ip_over_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-ray", HeaderValue::from_static("1234567890abcdef-SJC"));
        headers.insert("cf-connecting-ip", HeaderValue::from_static("192.0.2.10"));
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.20, 203.0.113.30"),
        );

        assert_eq!(
            super::client_ip_from_headers(&headers).as_deref(),
            Some("192.0.2.10")
        );
    }

    #[test]
    fn cloudflare_without_connecting_ip_uses_xff_tail() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-ray", HeaderValue::from_static("1234567890abcdef-SJC"));
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.20, 203.0.113.30"),
        );

        assert_eq!(
            super::client_ip_from_headers(&headers).as_deref(),
            Some("203.0.113.30")
        );
    }

    #[test]
    fn custom_forwarded_ip_is_not_used_as_a_proxy_header() {
        let mut headers = HeaderMap::new();
        headers.insert("forwarded_ip", HeaderValue::from_static("192.0.2.1"));

        assert_eq!(super::client_ip_from_headers(&headers), None);
    }

    #[test]
    fn disabled_tool_audit_does_not_insert_records() {
        let dir = tempdir().expect("temp dir");
        let store = AuditStore::open(dir.path().join("audit.sqlite")).expect("open audit db");
        let mut config = store.config().expect("config");
        config.tool_audit_enabled = false;
        store.set_config(config).expect("disable audit");
        let request = super::AuditRequestContext::default();
        let recorded = store
            .record_tool_call(
                &request,
                "workspace",
                "/tmp/workspace",
                "list_dir",
                &json!({}),
                &json!({"ok":true}),
                100,
                101,
            )
            .expect("record attempt");
        assert!(recorded.is_none());
        assert!(store
            .query(&AuditQuery::default())
            .expect("query")
            .is_empty());
    }

    #[test]
    fn retention_removes_records_older_than_the_configured_window() {
        let dir = tempdir().expect("temp dir");
        let store = AuditStore::open(dir.path().join("audit.sqlite")).expect("open audit db");
        let mut config = store.config().expect("config");
        config.tool_audit_retention_days = 1;
        store.set_config(config).expect("retention");
        let request = super::AuditRequestContext::default();
        let now = current_time_ms();
        store
            .record_tool_call(
                &request,
                "workspace",
                "/tmp/workspace",
                "old_tool",
                &json!({}),
                &json!({"ok":true}),
                now - 3 * MILLIS_PER_DAY,
                now - 3 * MILLIS_PER_DAY + 1,
            )
            .expect("old record");
        store
            .record_tool_call(
                &request,
                "workspace",
                "/tmp/workspace",
                "new_tool",
                &json!({}),
                &json!({"ok":true}),
                now,
                now + 1,
            )
            .expect("new record");
        let records = store.query(&AuditQuery::default()).expect("query");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tool_name.as_deref(), Some("new_tool"));
    }

    #[test]
    fn command_failure_is_not_counted_as_a_successful_tool_call() {
        let dir = tempdir().expect("temp dir");
        let store = AuditStore::open(dir.path().join("audit.sqlite")).expect("open audit db");
        let request = super::AuditRequestContext::default();
        let now = current_time_ms();
        store
            .record_tool_call(
                &request,
                "workspace",
                "/tmp/workspace",
                "exec_command",
                &json!({"cmd":"false"}),
                &json!({"ok":true, "command_ok":false, "exit_code":1, "summary":"exit 1"}),
                now,
                now + 5,
            )
            .expect("record");
        let record = store
            .query(&AuditQuery::default())
            .expect("query")
            .remove(0);
        assert_eq!(record.status, "failure");
        assert_eq!(record.error_message.as_deref(), Some("exit 1"));
        assert!(record.command.is_none());
        assert_eq!(
            store
                .get_record(&record.id)
                .expect("detail")
                .and_then(|detail| detail.command)
                .as_deref(),
            Some("false")
        );
        assert_eq!(
            store
                .stats(&AuditQuery::default())
                .expect("stats")
                .failure_calls,
            1
        );
    }

    #[test]
    fn command_follows_input_detail_retention() {
        let dir = tempdir().expect("temp dir");
        let store = AuditStore::open(dir.path().join("audit.sqlite")).expect("open audit db");
        let request = super::AuditRequestContext::default();
        let now = current_time_ms();

        let mut config = store.config().expect("config");
        config.detail_limit_bytes = -1;
        store.set_config(config.clone()).expect("disable details");
        let metadata_only_id = store
            .record_tool_call(
                &request,
                "workspace",
                "/tmp/workspace",
                "exec_command",
                &json!({"cmd":"echo secret"}),
                &json!({"ok":true}),
                now,
                now + 1,
            )
            .expect("record")
            .expect("record id");
        let metadata_only = store
            .get_record(&metadata_only_id)
            .expect("detail")
            .expect("record");
        assert!(metadata_only.input_json.is_none());
        assert!(metadata_only.command.is_none());

        config.detail_limit_bytes = 8;
        store.set_config(config).expect("limit details");
        let truncated_id = store
            .record_tool_call(
                &request,
                "workspace",
                "/tmp/workspace",
                "exec_command",
                &json!({"cmd":"echo a-command-longer-than-the-limit"}),
                &json!({"ok":true}),
                now + 2,
                now + 3,
            )
            .expect("record")
            .expect("record id");
        let truncated = store
            .get_record(&truncated_id)
            .expect("detail")
            .expect("record");
        assert!(truncated.input_truncated);
        assert!(truncated.command.is_none());
    }

    #[test]
    fn successful_kill_session_is_not_an_audit_failure() {
        let killed = super::response_metadata(
            "kill_session",
            &json!({"ok":true, "transport_ok":true, "command_ok":false, "status":"killed"}),
        );
        assert_eq!(killed.status, "success");
        assert!(!killed.is_error);

        let terminating = super::response_metadata(
            "kill_session",
            &json!({"ok":true, "transport_ok":true, "command_ok":false, "status":"terminating"}),
        );
        assert_eq!(terminating.status, "failure");
        assert!(terminating.is_error);
    }

    #[test]
    fn outcome_filter_applies_before_limit() {
        let dir = tempdir().expect("temp dir");
        let store = AuditStore::open(dir.path().join("audit.sqlite")).expect("open audit db");
        let request = super::AuditRequestContext::default();
        let now = current_time_ms();
        for (index, (tool_name, output)) in [
            (
                "failure",
                json!({"ok":false,"error":{"category":"tool","message":"failed"}}),
            ),
            ("success-1", json!({"ok":true})),
            (
                "rejected",
                json!({"ok":false,"error":{"category":"policy","message":"blocked"}}),
            ),
            ("success-2", json!({"ok":true})),
        ]
        .into_iter()
        .enumerate()
        {
            store
                .record_tool_call(
                    &request,
                    "workspace",
                    "/tmp/workspace",
                    tool_name,
                    &json!({}),
                    &output,
                    now + index as i64,
                    now + index as i64 + 1,
                )
                .expect("record");
        }

        let failures = store
            .query(&AuditQuery {
                successful: Some(false),
                limit: Some(2),
                ..AuditQuery::default()
            })
            .expect("query failures");
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].status, "rejected");
        assert_eq!(failures[1].status, "failure");

        let successes = store
            .query(&AuditQuery {
                successful: Some(true),
                limit: Some(1),
                ..AuditQuery::default()
            })
            .expect("query successes");
        assert_eq!(successes.len(), 1);
        assert_eq!(successes[0].tool_name.as_deref(), Some("success-2"));
    }

    #[test]
    fn rejected_tool_attempt_is_queryable_as_a_tool_record() {
        let dir = tempdir().expect("temp dir");
        let store = AuditStore::open(dir.path().join("audit.sqlite")).expect("open audit db");
        store
            .record_tool_rejection(
                &super::AuditRequestContext::default(),
                "workspace",
                "/tmp/workspace",
                "unknown_tool",
                &json!({"reason":"probe"}),
                "UNKNOWN_TOOL",
                "Unknown tool",
            )
            .expect("record rejection");

        let record = store
            .query(&AuditQuery {
                record_type: Some("tool".into()),
                ..AuditQuery::default()
            })
            .expect("query")
            .remove(0);
        assert_eq!(record.status, "rejected");
        assert_eq!(record.tool_name.as_deref(), Some("unknown_tool"));
        assert_eq!(record.reason.as_deref(), Some("probe"));
        assert_eq!(record.error_code.as_deref(), Some("UNKNOWN_TOOL"));
    }

    #[test]
    fn forwarded_ipv6_keeps_the_complete_address() {
        assert_eq!(
            normalize_forwarded_value("[2001:db8::1]:443"),
            "2001:db8::1"
        );
        assert_eq!(normalize_forwarded_value("2001:db8::2"), "2001:db8::2");
    }

    #[test]
    fn request_id_keeps_string_values_unquoted() {
        assert_eq!(
            request_id_from_value(&json!("request-1")).as_deref(),
            Some("request-1")
        );
        assert_eq!(request_id_from_value(&json!(0)).as_deref(), Some("0"));
        assert_eq!(request_id_from_value(&serde_json::Value::Null), None);
    }

    #[test]
    fn config_and_workspace_id_persist() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("audit.sqlite");
        let store = AuditStore::open(path.clone()).expect("open audit db");
        let mut config = store.config().expect("config");
        config.http_log_enabled = false;
        config.http_log_retention_days = 14;
        config.tool_audit_retention_days = 30;
        store.set_config(config).expect("save config");
        let request = super::AuditRequestContext::default();
        store
            .record_tool_call(
                &request,
                "profile-id",
                "/tmp/workspace",
                "list_dir",
                &json!({}),
                &json!({"ok":true}),
                current_time_ms(),
                current_time_ms() + 1,
            )
            .expect("record");
        drop(store);

        let reopened = AuditStore::open(path).expect("reopen audit db");
        let loaded = reopened.config().expect("load config");
        assert!(!loaded.http_log_enabled);
        assert_eq!(loaded.http_log_retention_days, 14);
        assert_eq!(loaded.tool_audit_retention_days, 30);
        let records = reopened.query(&AuditQuery::default()).expect("query");
        assert_eq!(records[0].workspace_id.as_deref(), Some("profile-id"));
    }

    #[test]
    fn removing_a_workspace_keeps_other_profiles_on_the_same_path() {
        let dir = tempdir().expect("temp dir");
        let store = AuditStore::open(dir.path().join("audit.sqlite")).expect("open audit db");
        let request = super::AuditRequestContext::default();
        for (workspace_id, workspace_path, tool_name) in [
            ("profile-a", "/tmp/shared-workspace", "remove"),
            ("profile-b", "/tmp/shared-workspace", "keep-shared"),
            ("profile-c", "/tmp/other-workspace", "keep-other"),
        ] {
            store
                .record_tool_call(
                    &request,
                    workspace_id,
                    workspace_path,
                    tool_name,
                    &json!({}),
                    &json!({"ok":true}),
                    current_time_ms(),
                    current_time_ms() + 1,
                )
                .expect("record");
        }

        assert_eq!(store.remove_workspace_records("profile-a").expect("remove rows"), 1);
        let records = store.query(&AuditQuery::default()).expect("query");
        assert_eq!(records.len(), 2);
        assert!(records
            .iter()
            .any(|record| record.workspace_id.as_deref() == Some("profile-b")));
        assert!(records
            .iter()
            .any(|record| record.workspace_id.as_deref() == Some("profile-c")));
    }
}
