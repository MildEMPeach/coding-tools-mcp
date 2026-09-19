use serde::{Deserialize, Serialize};

pub const GOAL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    Completed,
    Failed,
    Cleared,
}

impl GoalStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cleared)
    }

    pub fn is_current(self) -> bool {
        !self.is_terminal()
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (
                    Self::Active,
                    Self::Paused | Self::Blocked | Self::Completed | Self::Failed | Self::Cleared
                ) | (Self::Paused, Self::Active | Self::Cleared)
                    | (Self::Blocked, Self::Active | Self::Failed | Self::Cleared)
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalHealth {
    Running,
    Idle,
    Stalled,
    Paused,
    Blocked,
    Completed,
    Failed,
    Cleared,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalRecord {
    pub schema_version: u32,
    pub id: String,
    pub workspace_id: String,
    pub profile_id: Option<String>,
    pub task_id: Option<String>,
    pub objective: String,
    pub status: GoalStatus,
    pub health: GoalHealth,
    #[serde(default = "default_auto_continue")]
    pub auto_continue: bool,
    #[serde(default = "default_stale_after_secs")]
    pub stale_after_secs: u64,
    #[serde(default)]
    pub should_continue: bool,
    #[serde(default)]
    pub needs_attention: bool,
    #[serde(default)]
    pub completed_steps: Vec<String>,
    #[serde(default)]
    pub pending_steps: Vec<String>,
    pub note: Option<String>,
    pub blocked_reason: Option<String>,
    pub monitor_message: String,
    pub last_activity_at: String,
    pub last_progress_at: String,
    #[serde(default)]
    pub continuation_count: u32,
    pub created_at: String,
    pub updated_at: String,
}

fn default_auto_continue() -> bool {
    true
}

fn default_stale_after_secs() -> u64 {
    180
}

