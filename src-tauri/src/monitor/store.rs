use std::fs;
use std::path::{Path, PathBuf};

use crate::harness::{HarnessError, HarnessResult};

use super::model::GoalRecord;

#[derive(Debug, Clone)]
pub struct GoalStore {
    root: PathBuf,
}

impl GoalStore {
    pub fn new(root: PathBuf) -> HarnessResult<Self> {
        fs::create_dir_all(&root)
            .map_err(|error| HarnessError::new("STORE_UNAVAILABLE", error.to_string()))?;
        Ok(Self { root })
    }

    fn workspace_dir(&self, workspace_id: &str) -> PathBuf {
        self.root.join("workspaces").join(workspace_id)
    }

    fn goals_dir(&self, workspace_id: &str) -> PathBuf {
        self.workspace_dir(workspace_id).join("goals")
    }

    pub fn save_goal(&self, goal: &GoalRecord) -> HarnessResult<()> {
        let dir = self.goals_dir(&goal.workspace_id);
        fs::create_dir_all(&dir).map_err(io_error)?;
        atomic_write_json(&dir.join(format!("{}.json", goal.id)), goal)
    }

    pub fn load_goal(&self, workspace_id: &str, goal_id: &str) -> HarnessResult<GoalRecord> {
        read_json(&self.goals_dir(workspace_id).join(format!("{goal_id}.json")))
    }

    pub fn list_goals(&self, workspace_id: &str) -> HarnessResult<Vec<GoalRecord>> {
        let dir = self.goals_dir(workspace_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut goals: Vec<GoalRecord> = Vec::new();
        for entry in fs::read_dir(dir).map_err(io_error)? {
            let path = entry.map_err(io_error)?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            if let Ok(goal) = read_json(&path) {
                goals.push(goal);
            }
        }
        goals.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(goals)
    }
}

fn io_error(error: std::io::Error) -> HarnessError {
    HarnessError::new("STORE_IO_FAILED", error.to_string())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> HarnessResult<T> {
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| HarnessError::new("STORE_CORRUPT", format!("{}: {error}", path.display())))
}

fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> HarnessResult<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| HarnessError::new("STORE_SERIALIZE_FAILED", error.to_string()))?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes).map_err(io_error)?;
    fs::rename(&temp, path).map_err(io_error)
}

