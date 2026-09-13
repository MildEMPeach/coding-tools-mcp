//! HTTP 访问日志的 Tauri 边界：校验工作区范围，并将同步文件扫描移出异步执行器。

use tauri::State;

use crate::access_log::{access_log_directory, query_access_logs, AccessLogQuery, AccessLogResult};
use crate::app_state::AppState;
use crate::error::{AppError, AppResult};
use crate::platform::open_path_in_file_manager;

#[tauri::command]
/// 查询 HTTP 访问日志。
///
/// 指定 workspaceId 时先确认工作区存在；未指定时对当前全部工作区生成 ID 快照。实际文件
/// 扫描在 blocking 线程池执行，返回值已完成跨工作区排序、筛选和统计。
pub async fn query_http_access_logs(
    state: State<'_, AppState>,
    query: AccessLogQuery,
) -> AppResult<AccessLogResult> {
    // 锁内只校验并复制 ID；目录遍历期间不能持续占用工作区存储锁。
    let workspace_ids = state.with_workspaces(|store| {
        if let Some(id) = query
            .workspace_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            if store.get(id).is_none() {
                return Err(AppError::Message(format!("workspace not found: {id}")));
            }
            Ok(vec![id.to_string()])
        } else {
            Ok(store.list().iter().map(|profile| profile.id.clone()).collect())
        }
    })?;
    // 查询会同步遍历并逐行解析多个文件，放入 blocking 池避免阻塞监听服务。
    tokio::task::spawn_blocking(move || query_access_logs(&workspace_ids, &query))
        .await
        .map_err(|error| AppError::Message(format!("HTTP access log worker failed: {error}")))?
        .map_err(AppError::Io)
}

/// 解析“全部日志”或“单工作区日志”目录，并阻止不存在的工作区 ID 进入路径层。
fn checked_access_log_directory(
    state: &AppState,
    workspace_id: Option<&str>,
) -> AppResult<std::path::PathBuf> {
    // ID 最终参与路径构造：除底层防穿越校验外，还必须属于当前工作区集合。
    let workspace_id = workspace_id.map(str::trim).filter(|id| !id.is_empty());
    if let Some(id) = workspace_id {
        state.with_workspaces(|store| {
            if store.get(id).is_some() {
                Ok(())
            } else {
                Err(AppError::Message(format!("workspace not found: {id}")))
            }
        })?;
    }
    access_log_directory(workspace_id).map_err(AppError::Io)
}

#[tauri::command]
/// 在系统文件管理器打开日志目录。
///
/// 尚未产生任何日志时会先创建目录，使 UI 操作不依赖当前是否已经启动过服务。
pub fn open_http_access_log_directory(
    state: State<'_, AppState>,
    workspace_id: Option<String>,
) -> AppResult<()> {
    let directory = checked_access_log_directory(&state, workspace_id.as_deref())?;
    std::fs::create_dir_all(&directory)?;
    open_path_in_file_manager(&directory)
}
