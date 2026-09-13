//! 工具审计的 Tauri 适配层；业务规则留在 audit/access_log，错误在此统一转换为 AppError。

use serde::Serialize;

use crate::audit::{AuditConfig, AuditQuery, AuditRecord, AuditStats, AuditStore};
use crate::error::{AppError, AppResult};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearLogsResult {
    pub log_files: usize,
    pub audit_records: usize,
}

fn store() -> AppResult<AuditStore> {
    AuditStore::open_default().map_err(|error| AppError::Message(error.to_string()))
}

#[tauri::command]
/// 读取进程内共享的审计配置快照。
pub fn get_audit_config() -> AppResult<AuditConfig> {
    store()?
        .config()
        .map_err(|error| AppError::Message(error.to_string()))
}

#[tauri::command]
/// 保存完整审计配置，并立即应用两类日志的保留期。
///
/// SQLite 工具记录由 AuditStore 清理；HTTP 文本文件随后独立清理。文本清理失败只记录告警，
/// 不回滚已经持久化的配置，后续启动/写入仍会重试。
pub fn set_audit_config(config: AuditConfig) -> AppResult<AuditConfig> {
    let config = store()?
        .set_config(config)
        .map_err(|error| AppError::Message(error.to_string()))?;
    // AuditStore 已处理 SQLite 保留期；HTTP 文本日志是独立存储，需额外清理。
    if let Err(error) = crate::access_log::cleanup_all_access_logs(config.http_log_retention_days) {
        eprintln!("access log retention cleanup failed after settings update: {error}");
    }
    Ok(config)
}

#[tauri::command]
/// 清空所有文本日志和工具审计记录。
///
/// 两种介质没有跨存储事务：若第二步失败，第一步不会回滚。结果分别报告截断文件数与删除
/// 记录数，供 UI 准确提示实际影响。
pub async fn clear_all_logs() -> AppResult<ClearLogsResult> {
    // 递归文件遍历、DELETE、checkpoint 和 VACUUM 都可能长时间阻塞，不能运行在 Tauri IPC 线程。
    tokio::task::spawn_blocking(|| {
        let log_files = crate::access_log::clear_all_log_files()?;
        let audit_records = store()?
            .clear_records()
            .map_err(|error| AppError::Message(error.to_string()))?;
        Ok(ClearLogsResult {
            log_files,
            audit_records,
        })
    })
    .await
    .map_err(|error| AppError::Message(format!("log cleanup worker failed: {error}")))?
}

#[tauri::command]
/// 按查询条件返回倒序审计记录；是否包含大字段由 includeDetails 决定。
pub fn query_audit_records(query: AuditQuery) -> AppResult<Vec<AuditRecord>> {
    store()?
        .query(&query)
        .map_err(|error| AppError::Message(error.to_string()))
}

#[tauri::command]
/// 按内部审计 UUID 返回单条完整记录；不存在时返回 null。
pub fn get_audit_record(id: String) -> AppResult<Option<AuditRecord>> {
    store()?
        .get_record(&id)
        .map_err(|error| AppError::Message(error.to_string()))
}

#[tauri::command]
/// 使用与明细查询一致的过滤条件聚合工具调用统计。
pub async fn get_audit_stats(query: AuditQuery) -> AppResult<AuditStats> {
    // 聚合可能扫描完整时间范围；自动刷新只等待结果，不再占用桌面 IPC/UI 执行线程。
    tokio::task::spawn_blocking(move || {
        store()?
            .stats(&query)
            .map_err(|error| AppError::Message(error.to_string()))
    })
    .await
    .map_err(|error| AppError::Message(format!("audit stats worker failed: {error}")))?
}
