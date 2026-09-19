use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::harness::{Harness, HarnessError, HarnessResult, TaskStatus};

use super::model::{GoalHealth, GoalRecord, GoalStatus, GOAL_SCHEMA_VERSION};
use super::store::GoalStore;

const IDLE_AFTER_SECS: u64 = 20;
static GOAL_STATE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone)]
pub struct GoalMonitor {
    workspace_id: String,
    profile_id: Option<String>,
    store: GoalStore,
}

impl GoalMonitor {
    pub fn new(
        workspace_id: impl Into<String>,
        profile_id: Option<String>,
        root: PathBuf,
    ) -> HarnessResult<Self> {
        Ok(Self {
            workspace_id: workspace_id.into(),
            profile_id,
            store: GoalStore::new(root)?,
        })
    }

    pub fn default_root() -> HarnessResult<PathBuf> {
        let root = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| HarnessError::new("STORE_UNAVAILABLE", "无法确定应用数据目录"))?;
        Ok(root.join("coding-tools-mcp").join("monitor"))
    }

    pub fn create_goal(
        &self,
        objective: &str,
        task_id: Option<String>,
        auto_continue: bool,
        stale_after_secs: u64,
    ) -> HarnessResult<GoalRecord> {
        if objective.trim().is_empty() {
            return Err(HarnessError::new("INVALID_ARGUMENT", "Goal objective 不能为空"));
        }
        let _guard = lock_goal_state()?;
        if let Some(existing) = self.current_goal_unlocked()? {
            return Err(HarnessError::new(
                "GOAL_ALREADY_ACTIVE",
                format!("当前工作区已有未结束 Goal {}", existing.id),
            ));
        }
        let now = timestamp();
        let goal = GoalRecord {
            schema_version: GOAL_SCHEMA_VERSION,
            id: Uuid::new_v4().simple().to_string(),
            workspace_id: self.workspace_id.clone(),
            profile_id: self.profile_id.clone(),
            task_id,
            objective: objective.trim().to_string(),
            status: GoalStatus::Active,
            health: GoalHealth::Running,
            auto_continue,
            stale_after_secs: stale_after_secs.clamp(30, 86_400),
            should_continue: auto_continue,
            needs_attention: false,
            completed_steps: Vec::new(),
            pending_steps: Vec::new(),
            note: None,
            blocked_reason: None,
            monitor_message: "Goal 已启动，等待 Agent 持续推进".into(),
            last_activity_at: now.clone(),
            last_progress_at: now.clone(),
            continuation_count: 0,
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.save_goal(&goal)?;
        Ok(goal)
    }

    pub fn goal(&self, goal_id: &str) -> HarnessResult<GoalRecord> {
        let _guard = lock_goal_state()?;
        self.goal_unlocked(goal_id)
    }

    pub fn list_goals(&self) -> HarnessResult<Vec<GoalRecord>> {
        let _guard = lock_goal_state()?;
        self.list_goals_unlocked()
    }

    pub fn current_goal(&self) -> HarnessResult<Option<GoalRecord>> {
        let _guard = lock_goal_state()?;
        self.current_goal_unlocked()
    }

    fn goal_unlocked(&self, goal_id: &str) -> HarnessResult<GoalRecord> {
        self.store.load_goal(&self.workspace_id, goal_id)
    }

    fn list_goals_unlocked(&self) -> HarnessResult<Vec<GoalRecord>> {
        self.store.list_goals(&self.workspace_id)
    }

    fn current_goal_unlocked(&self) -> HarnessResult<Option<GoalRecord>> {
        Ok(self
            .list_goals_unlocked()?
            .into_iter()
            .find(|goal| goal.status.is_current()))
    }

    pub fn update_progress(
        &self,
        goal_id: &str,
        completed_steps: Option<Vec<String>>,
        pending_steps: Option<Vec<String>>,
        note: Option<String>,
    ) -> HarnessResult<GoalRecord> {
        let _guard = lock_goal_state()?;
        let mut goal = self.goal_unlocked(goal_id)?;
        if goal.status.is_terminal() {
            return Err(HarnessError::new("GOAL_TERMINAL", "已结束 Goal 不能继续更新"));
        }
        let mut progressed = false;
        if let Some(steps) = completed_steps {
            progressed |= steps != goal.completed_steps;
            goal.completed_steps = steps;
        }
        if let Some(steps) = pending_steps {
            progressed |= steps != goal.pending_steps;
            goal.pending_steps = steps;
        }
        if let Some(note) = note {
            let note = note.trim().to_string();
            if !note.is_empty() {
                goal.note = Some(note);
                progressed = true;
            }
        }
        let now = timestamp();
        goal.last_activity_at = now.clone();
        if progressed {
            goal.last_progress_at = now.clone();
        }
        goal.updated_at = now;
        goal.health = GoalHealth::Running;
        goal.needs_attention = false;
        goal.should_continue = goal.status == GoalStatus::Active && goal.auto_continue;
        goal.monitor_message = "Agent 已更新 Goal 进度".into();
        self.store.save_goal(&goal)?;
        Ok(goal)
    }

    pub fn set_status(
        &self,
        goal_id: &str,
        status: GoalStatus,
        reason: Option<String>,
    ) -> HarnessResult<GoalRecord> {
        let _guard = lock_goal_state()?;
        let mut goal = self.goal_unlocked(goal_id)?;
        if !goal.status.can_transition_to(status) {
            return Err(HarnessError::new(
                "INVALID_GOAL_TRANSITION",
                format!("不允许从 {:?} 转换到 {:?}", goal.status, status),
            ));
        }
        goal.status = status;
        goal.updated_at = timestamp();
        match status {
            GoalStatus::Active => {
                goal.health = GoalHealth::Running;
                goal.blocked_reason = None;
                goal.should_continue = goal.auto_continue;
                goal.needs_attention = false;
                goal.monitor_message = "Goal 已恢复，等待继续推进".into();
            }
            GoalStatus::Paused => {
                goal.health = GoalHealth::Paused;
                goal.should_continue = false;
                goal.needs_attention = false;
                goal.monitor_message = "Goal 已暂停".into();
            }
            GoalStatus::Blocked => {
                goal.health = GoalHealth::Blocked;
                goal.blocked_reason = reason.filter(|value| !value.trim().is_empty());
                goal.should_continue = false;
                goal.needs_attention = true;
                goal.monitor_message = goal
                    .blocked_reason
                    .clone()
                    .unwrap_or_else(|| "Goal 被阻塞，需要用户处理".into());
            }
            GoalStatus::Completed => {
                goal.health = GoalHealth::Completed;
                goal.should_continue = false;
                goal.needs_attention = false;
                goal.monitor_message = "Goal 已完成并停止监控推进".into();
            }
            GoalStatus::Failed => {
                goal.health = GoalHealth::Failed;
                goal.should_continue = false;
                goal.needs_attention = true;
                goal.monitor_message = reason.unwrap_or_else(|| "Goal 执行失败".into());
            }
            GoalStatus::Cleared => {
                goal.health = GoalHealth::Cleared;
                goal.should_continue = false;
                goal.needs_attention = false;
                goal.monitor_message = "Goal 已清除".into();
            }
        }
        self.store.save_goal(&goal)?;
        Ok(goal)
    }

    pub fn touch_task(&self, task_id: Option<&str>) -> HarnessResult<()> {
        let _guard = lock_goal_state()?;
        let Some(mut goal) = self.current_goal_unlocked()? else {
            return Ok(());
        };
        if let (Some(goal_task), Some(task_id)) = (goal.task_id.as_deref(), task_id) {
            if goal_task != task_id {
                return Ok(());
            }
        }
        let now = timestamp();
        goal.last_activity_at = now.clone();
        goal.updated_at = now;
        if goal.status == GoalStatus::Active {
            goal.health = GoalHealth::Running;
            goal.needs_attention = false;
            goal.should_continue = goal.auto_continue;
            goal.monitor_message = "检测到 Agent 活动".into();
        }
        self.store.save_goal(&goal)
    }

    pub fn record_continuation(&self, goal_id: &str) -> HarnessResult<GoalRecord> {
        let _guard = lock_goal_state()?;
        let mut goal = self.goal_unlocked(goal_id)?;
        if goal.status != GoalStatus::Active {
            return Ok(goal);
        }
        goal.continuation_count = goal.continuation_count.saturating_add(1);
        goal.updated_at = timestamp();
        goal.monitor_message = format!(
            "ChatGPT 已请求第 {} 次 Goal 续跑 handoff",
            goal.continuation_count
        );
        self.store.save_goal(&goal)?;
        Ok(goal)
    }

    pub fn refresh_current(&self, harness: &Harness) -> HarnessResult<Option<GoalRecord>> {
        let _guard = lock_goal_state()?;
        let Some(goal) = self.current_goal_unlocked()? else {
            return Ok(None);
        };
        self.refresh_goal_unlocked(&goal.id, harness).map(Some)
    }

    pub fn refresh_goal(&self, goal_id: &str, harness: &Harness) -> HarnessResult<GoalRecord> {
        let _guard = lock_goal_state()?;
        self.refresh_goal_unlocked(goal_id, harness)
    }

    fn refresh_goal_unlocked(&self, goal_id: &str, harness: &Harness) -> HarnessResult<GoalRecord> {
        let mut goal = self.goal_unlocked(goal_id)?;
        if goal.status != GoalStatus::Active {
            return Ok(goal);
        }

        let mut latest_ms = parse_ms(&goal.last_activity_at);
        if let Some(task_id) = goal.task_id.as_deref() {
            if let Ok(task) = harness.task(task_id) {
                latest_ms = latest_ms.max(parse_ms(&task.updated_at));
                if matches!(task.status, TaskStatus::Completed | TaskStatus::CompletedUnverified) {
                    goal.status = GoalStatus::Completed;
                    goal.health = GoalHealth::Completed;
                    goal.should_continue = false;
                    goal.needs_attention = false;
                    goal.monitor_message = "关联 Harness Task 已完成".into();
                    goal.updated_at = timestamp();
                    self.store.save_goal(&goal)?;
                    return Ok(goal);
                }
                if task.status == TaskStatus::RolledBack {
                    goal.health = GoalHealth::Failed;
                    goal.needs_attention = true;
                    goal.should_continue = false;
                    goal.monitor_message = "关联 Harness Task 已回滚，需要重新规划 Goal".into();
                    goal.updated_at = timestamp();
                    self.store.save_goal(&goal)?;
                    return Ok(goal);
                }
                if task.status == TaskStatus::Paused {
                    goal.health = GoalHealth::Paused;
                    goal.needs_attention = false;
                    goal.should_continue = false;
                    goal.monitor_message = "关联 Harness Task 已暂停".into();
                    goal.updated_at = timestamp();
                    self.store.save_goal(&goal)?;
                    return Ok(goal);
                }
                if task.status == TaskStatus::Failed {
                    goal.health = GoalHealth::Blocked;
                    goal.needs_attention = true;
                    goal.should_continue = false;
                    goal.monitor_message = "关联 Harness Task 失败，等待恢复或处理".into();
                    goal.updated_at = timestamp();
                    self.store.save_goal(&goal)?;
                    return Ok(goal);
                }
            }
        }
        if let Ok(operations) = harness.recent_operations(8) {
            for operation in operations {
                if goal.task_id.is_none() || operation.task_id.as_deref() == goal.task_id.as_deref() {
                    latest_ms = latest_ms.max(parse_ms(&operation.created_at));
                }
            }
        }

        let now_ms = timestamp_ms();
        let idle_ms = now_ms.saturating_sub(latest_ms);
        let stale_ms = goal.stale_after_secs.saturating_mul(1000);
        if idle_ms >= stale_ms {
            goal.health = GoalHealth::Stalled;
            goal.needs_attention = true;
            goal.should_continue = goal.auto_continue;
            goal.monitor_message = format!(
                "已超过 {} 秒没有检测到进展，需要继续推进或人工检查",
                goal.stale_after_secs
            );
        } else if idle_ms >= IDLE_AFTER_SECS * 1000 {
            goal.health = GoalHealth::Idle;
            goal.needs_attention = false;
            goal.should_continue = goal.auto_continue;
            goal.monitor_message = "Agent 当前空闲，Goal 仍未完成".into();
        } else {
            goal.health = GoalHealth::Running;
            goal.needs_attention = false;
            goal.should_continue = goal.auto_continue;
            goal.monitor_message = "Goal 正在推进".into();
        }
        if latest_ms > parse_ms(&goal.last_activity_at) {
            goal.last_activity_at = latest_ms.to_string();
        }
        goal.updated_at = timestamp();
        self.store.save_goal(&goal)?;
        Ok(goal)
    }
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn timestamp() -> String {
    timestamp_ms().to_string()
}

fn parse_ms(value: &str) -> u64 {
    value.parse::<u64>().unwrap_or(0)
}

fn lock_goal_state() -> HarnessResult<MutexGuard<'static, ()>> {
    GOAL_STATE_LOCK
        .lock()
        .map_err(|_| HarnessError::new("STORE_LOCK_POISONED", "Goal Monitor 状态锁已损坏"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn goal_lifecycle_is_persistent() {
        let store = tempdir().expect("monitor store");
        let monitor = GoalMonitor::new("workspace", Some("profile".into()), store.path().to_path_buf())
            .expect("monitor");
        let goal = monitor
            .create_goal("完成长期任务", Some("task-1".into()), true, 60)
            .expect("goal");
        assert_eq!(goal.status, GoalStatus::Active);
        assert!(monitor.current_goal().expect("current").is_some());

        let paused = monitor
            .set_status(&goal.id, GoalStatus::Paused, None)
            .expect("pause");
        assert_eq!(paused.health, GoalHealth::Paused);
        let resumed = monitor
            .set_status(&goal.id, GoalStatus::Active, None)
            .expect("resume");
        assert!(resumed.should_continue);
        let completed = monitor
            .set_status(&goal.id, GoalStatus::Completed, None)
            .expect("complete");
        assert_eq!(completed.health, GoalHealth::Completed);
        assert!(monitor.current_goal().expect("current").is_none());
        assert!(fs::read_dir(store.path()).is_ok());
    }
}

