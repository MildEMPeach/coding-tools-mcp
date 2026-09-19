import { invoke } from "@tauri-apps/api/core";

export type HarnessTaskStatus =
  | "active"
  | "paused"
  | "verifying"
  | "failed"
  | "completed"
  | "completed_unverified"
  | "rolled_back";

export interface HarnessCapability {
  status: string;
  reason: string;
  recoverable: boolean;
}

export interface HarnessStatus {
  schema_version: number;
  workspace_id: string;
  mode: "standalone" | "task";
  task_id: string | null;
  task_objective: string | null;
  task_state: HarnessTaskStatus | null;
  task_updated_at: string | null;
  writable: boolean;
  reason: string;
  recoverable: boolean;
  branch: string | null;
  head: string | null;
  baseline_matches: boolean | null;
  capabilities: Record<string, HarnessCapability>;
  next_actions: string[];
}

export interface HarnessTaskSummary {
  id: string;
  objective: string;
  status: HarnessTaskStatus;
  completed_steps: string[];
  pending_steps: string[];
  updated_at: string;
}

export interface HarnessOperation {
  id: string;
  workspace_id: string;
  task_id: string | null;
  tool: string;
  kind: string;
  input_summary: Record<string, unknown>;
  result_summary: Record<string, unknown>;
  reason: string | null;
  affected_files: Array<{
    path: string;
    status: string;
    before_sha256: string | null;
    after_sha256: string | null;
  }>;
  created_at: string;
}

export interface HarnessDashboard {
  status: HarnessStatus;
  task: HarnessTaskSummary | null;
  operations: HarnessOperation[];
}

export async function getHarnessDashboard(id: string): Promise<HarnessDashboard> {
  return invoke<HarnessDashboard>("get_harness_dashboard", { id });
}
