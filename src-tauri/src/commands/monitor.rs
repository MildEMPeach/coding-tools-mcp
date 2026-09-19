use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use crate::app_state::AppState;
use crate::error::{AppError, AppResult};
use crate::harness::{Harness, TaskStatus};
use crate::monitor::{GoalMonitor, GoalRecord, GoalStatus};
use crate::workspace::WorkspaceProfile;

#[derive(Debug, Serialize)]
pub struct GoalDashboardItem {
    pub profile_id: String,
    pub workspace_name: String,
    pub workspace_path: String,
    pub goal: GoalRecord,
}

fn profile(state: &State<'_, AppState>, id: &str) -> AppResult<WorkspaceProfile> {
    state.with_workspaces(|store| {
        store
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })
}

fn monitor_for_profile(profile: &WorkspaceProfile) -> AppResult<(Harness, GoalMonitor)> {
    let harness_root = Harness::default_root().map_err(|error| AppError::Message(error.to_string()))?;
    let harness = Harness::new(PathBuf::from(&profile.path), harness_root)
        .map_err(|error| AppError::Message(error.to_string()))?;
    let monitor_root =
        GoalMonitor::default_root().map_err(|error| AppError::Message(error.to_string()))?;
    let monitor = GoalMonitor::new(
        harness.workspace_id().to_string(),
        Some(profile.id.clone()),
        monitor_root,
    )
    .map_err(|error| AppError::Message(error.to_string()))?;
    Ok((harness, monitor))
}

fn item(profile: &WorkspaceProfile, goal: GoalRecord) -> GoalDashboardItem {
    GoalDashboardItem {
        profile_id: profile.id.clone(),
        workspace_name: profile.name.clone(),
        workspace_path: profile.path.clone(),
        goal,
    }
}

#[tauri::command]
pub fn list_goal_dashboard(state: State<'_, AppState>) -> AppResult<Vec<GoalDashboardItem>> {
    let profiles = state.with_workspaces(|store| Ok(store.list().to_vec()))?;
    let mut result = Vec::new();
    for profile in profiles {
        let Ok((harness, monitor)) = monitor_for_profile(&profile) else {
            continue;
        };
        let _ = monitor.refresh_current(&harness);
        if let Ok(goals) = monitor.list_goals() {
            result.extend(goals.into_iter().map(|goal| item(&profile, goal)));
        }
    }
    result.sort_by(|a, b| b.goal.updated_at.cmp(&a.goal.updated_at));
    Ok(result)
}

#[tauri::command]
pub fn create_workspace_goal(
    state: State<'_, AppState>,
    id: String,
    objective: String,
    auto_continue: Option<bool>,
    stale_after_secs: Option<u64>,
) -> AppResult<GoalDashboardItem> {
    let profile = profile(&state, &id)?;
    let (harness, monitor) = monitor_for_profile(&profile)?;
    let task = match harness
        .current_task()
        .map_err(|error| AppError::Message(error.to_string()))?
    {
        Some(task) => {
            if task.objective.trim() != objective.trim() {
                return Err(AppError::Message(format!(
                    "当前 Harness Task 仍未结束：{}。请先完成或处理该 Task，再创建新的 Goal",
                    task.objective
                )));
            }
            if matches!(task.status, TaskStatus::Paused | TaskStatus::Failed) {
                harness
                    .transition(&task.id, TaskStatus::Active)
                    .map_err(|error| AppError::Message(error.to_string()))?
            } else {
                task
            }
        }
        None => harness
            .start_task(&objective)
            .map_err(|error| AppError::Message(error.to_string()))?,
    };
    let goal = monitor
        .create_goal(
            &objective,
            Some(task.id),
            auto_continue.unwrap_or(true),
            stale_after_secs.unwrap_or(180),
        )
        .map_err(|error| AppError::Message(error.to_string()))?;
    Ok(item(&profile, goal))
}

#[tauri::command]
pub fn pause_workspace_goal(
    state: State<'_, AppState>,
    id: String,
    goal_id: String,
) -> AppResult<GoalDashboardItem> {
    let profile = profile(&state, &id)?;
    let (harness, monitor) = monitor_for_profile(&profile)?;
    let goal = monitor
        .set_status(&goal_id, GoalStatus::Paused, None)
        .map_err(|error| AppError::Message(error.to_string()))?;
    if let Some(task_id) = goal.task_id.as_deref() {
        if let Ok(task) = harness.task(task_id) {
            if task.status == TaskStatus::Active {
                let _ = harness.transition(task_id, TaskStatus::Paused);
            }
        }
    }
    Ok(item(&profile, goal))
}

#[tauri::command]
pub fn resume_workspace_goal(
    state: State<'_, AppState>,
    id: String,
    goal_id: String,
) -> AppResult<GoalDashboardItem> {
    let profile = profile(&state, &id)?;
    let (harness, monitor) = monitor_for_profile(&profile)?;
    let goal = monitor
        .set_status(&goal_id, GoalStatus::Active, None)
        .map_err(|error| AppError::Message(error.to_string()))?;
    if let Some(task_id) = goal.task_id.as_deref() {
        if let Ok(task) = harness.task(task_id) {
            if matches!(task.status, TaskStatus::Paused | TaskStatus::Failed) {
                let _ = harness.transition(task_id, TaskStatus::Active);
            }
        }
    }
    Ok(item(&profile, goal))
}

#[tauri::command]
pub fn clear_workspace_goal(
    state: State<'_, AppState>,
    id: String,
    goal_id: String,
) -> AppResult<GoalDashboardItem> {
    let profile = profile(&state, &id)?;
    let (_harness, monitor) = monitor_for_profile(&profile)?;
    let goal = monitor
        .set_status(&goal_id, GoalStatus::Cleared, None)
        .map_err(|error| AppError::Message(error.to_string()))?;
    Ok(item(&profile, goal))
}

