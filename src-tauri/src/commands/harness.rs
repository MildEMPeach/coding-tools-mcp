use serde::Serialize;
use tauri::State;

use crate::app_state::AppState;
use crate::error::{AppError, AppResult};
use crate::harness::model::{HarnessStatus, OperationRecord};
use crate::harness::{Harness, TaskStatus};

#[derive(Debug, Serialize)]
pub struct HarnessTaskSummary {
    pub id: String,
    pub objective: String,
    pub status: TaskStatus,
    pub completed_steps: Vec<String>,
    pub pending_steps: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct HarnessDashboard {
    pub status: HarnessStatus,
    pub task: Option<HarnessTaskSummary>,
    pub operations: Vec<OperationRecord>,
}

fn harness_for_workspace(state: &State<'_, AppState>, id: &str) -> AppResult<Harness> {
    let path = state.with_workspaces(|store| {
        store
            .get(id)
            .map(|profile| profile.path.clone())
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })?;
    let root = Harness::default_root().map_err(|error| AppError::Message(error.to_string()))?;
    Harness::new(path.into(), root).map_err(|error| AppError::Message(error.to_string()))
}

#[tauri::command]
pub fn get_harness_dashboard(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<HarnessDashboard> {
    let harness = harness_for_workspace(&state, &id)?;
    let status = harness
        .status()
        .map_err(|error| AppError::Message(error.to_string()))?;
    let task = harness
        .current_task()
        .map_err(|error| AppError::Message(error.to_string()))?
        .map(|task| HarnessTaskSummary {
            id: task.id,
            objective: task.objective,
            status: task.status,
            completed_steps: task.completed_steps,
            pending_steps: task.pending_steps,
            updated_at: task.updated_at,
        });
    let operations = harness
        .recent_operations(16)
        .map_err(|error| AppError::Message(error.to_string()))?;
    Ok(HarnessDashboard {
        status,
        task,
        operations,
    })
}
