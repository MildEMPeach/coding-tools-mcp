import { invoke } from "@tauri-apps/api/core";

export type GoalStatus = "active" | "paused" | "blocked" | "completed" | "failed" | "cleared";
export type GoalHealth =
  | "running"
  | "idle"
  | "stalled"
  | "paused"
  | "blocked"
  | "completed"
  | "failed"
  | "cleared";

export interface GoalRecord {
  schema_version: number;
  id: string;
  workspace_id: string;
  profile_id: string | null;
  task_id: string | null;
  objective: string;
  status: GoalStatus;
  health: GoalHealth;
  auto_continue: boolean;
  stale_after_secs: number;
  should_continue: boolean;
  needs_attention: boolean;
  completed_steps: string[];
  pending_steps: string[];
  note: string | null;
  blocked_reason: string | null;
  monitor_message: string;
  last_activity_at: string;
  last_progress_at: string;
  continuation_count: number;
  created_at: string;
  updated_at: string;
}

export interface GoalDashboardItem {
  profile_id: string;
  workspace_name: string;
  workspace_path: string;
  goal: GoalRecord;
}

export interface CreateGoalOptions {
  autoContinue?: boolean;
  staleAfterSecs?: number;
}

export async function listGoalDashboard(): Promise<GoalDashboardItem[]> {
  return invoke<GoalDashboardItem[]>("list_goal_dashboard");
}

export async function createWorkspaceGoal(
  id: string,
  objective: string,
  options: CreateGoalOptions = {},
): Promise<GoalDashboardItem> {
  return invoke<GoalDashboardItem>("create_workspace_goal", {
    id,
    objective,
    autoContinue: options.autoContinue ?? true,
    staleAfterSecs: options.staleAfterSecs ?? 180,
  });
}

export async function pauseWorkspaceGoal(id: string, goalId: string): Promise<GoalDashboardItem> {
  return invoke<GoalDashboardItem>("pause_workspace_goal", { id, goalId });
}

export async function resumeWorkspaceGoal(id: string, goalId: string): Promise<GoalDashboardItem> {
  return invoke<GoalDashboardItem>("resume_workspace_goal", { id, goalId });
}

export async function clearWorkspaceGoal(id: string, goalId: string): Promise<GoalDashboardItem> {
  return invoke<GoalDashboardItem>("clear_workspace_goal", { id, goalId });
}

