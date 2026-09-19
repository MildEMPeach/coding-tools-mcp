use serde_json::{json, Value};

use crate::harness::TaskStatus;
use crate::monitor::GoalStatus;
use crate::tools::workspace::{tool_ok, WorkspaceError};
use crate::tools::ToolContext;

pub const TOOL_NAMES: &[&str] = &[
    "goal_status",
    "goal_handoff",
    "goal_create",
    "goal_update",
    "goal_pause",
    "goal_resume",
    "goal_block",
    "goal_complete",
    "goal_clear",
];

pub fn call(ctx: &ToolContext, name: &str, args: &Value) -> Result<Value, WorkspaceError> {
    let value = match name {
        "goal_status" => goal_status(ctx, args),
        "goal_handoff" => goal_handoff(ctx, args),
        "goal_create" => goal_create(ctx, args),
        "goal_update" => goal_update(ctx, args),
        "goal_pause" => goal_pause(ctx, args),
        "goal_resume" => goal_resume(ctx, args),
        "goal_block" => goal_block(ctx, args),
        "goal_complete" => goal_complete(ctx, args),
        "goal_clear" => goal_clear(ctx, args),
        _ => Err(tool_error("INVALID_ARGUMENT", "未知 Goal 工具")),
    }?;
    Ok(tool_ok(value))
}

fn goal_status(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let goal = if let Some(goal_id) = args.get("goal_id").and_then(Value::as_str) {
        Some(
            ctx.monitor
                .refresh_goal(goal_id, &ctx.harness)
                .map_err(map_error)?,
        )
    } else {
        ctx.monitor
            .refresh_current(&ctx.harness)
            .map_err(map_error)?
    };
    let Some(goal) = goal else {
        return Ok(json!({
            "goal": null,
            "should_continue": false,
            "next_action": "goal_create",
            "message": "当前工作区没有活动 Goal"
        }));
    };
    Ok(json!({
        "should_continue": goal.should_continue,
        "needs_attention": goal.needs_attention,
        "continuation": if goal.should_continue {
            Some("Goal 仍处于 active 状态。继续执行 pending_steps；若准备结束本轮但目标仍未完成，请调用 goal_handoff。")
        } else {
            None::<&str>
        },
        "goal": goal
    }))
}

fn goal_handoff(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let goal = if let Some(goal_id) = args.get("goal_id").and_then(Value::as_str) {
        ctx.monitor
            .refresh_goal(goal_id, &ctx.harness)
            .map_err(map_error)?
    } else {
        ctx.monitor
            .refresh_current(&ctx.harness)
            .map_err(map_error)?
            .ok_or_else(|| tool_error("GOAL_REQUIRED", "当前工作区没有活动 Goal"))?
    };
    if goal.status != GoalStatus::Active || !goal.should_continue {
        return Ok(json!({
            "goal": goal,
            "should_continue": false,
            "handoff_requested": false,
            "message": "Goal 当前不需要续跑 handoff"
        }));
    }
    let goal = ctx
        .monitor
        .record_continuation(&goal.id)
        .map_err(map_error)?;
    Ok(json!({
        "goal": goal,
        "should_continue": true,
        "handoff_requested": true,
        "message": "ChatGPT Goal 控件将请求发送下一条 follow-up message，继续推进当前 Goal。"
    }))
}

fn goal_create(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let objective = args
        .get("objective")
        .and_then(Value::as_str)
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "objective 是必填项"))?;
    let auto_continue = args
        .get("auto_continue")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let stale_after_secs = args
        .get("stale_after_secs")
        .and_then(Value::as_u64)
        .unwrap_or(180)
        .clamp(30, 86_400);

    let task = match ctx.harness.current_task().map_err(map_harness_error)? {
        Some(task) => {
            if task.objective.trim() != objective.trim() {
                return Err(tool_error(
                    "GOAL_TASK_CONFLICT",
                    format!(
                        "当前 Harness Task 仍未结束：{}。请先完成或处理该 Task，再创建新的 Goal",
                        task.objective
                    ),
                ));
            }
            if matches!(task.status, TaskStatus::Paused | TaskStatus::Failed) {
                ctx.harness
                    .transition(&task.id, TaskStatus::Active)
                    .map_err(map_harness_error)?
            } else {
                task
            }
        }
        None => ctx.harness.start_task(objective).map_err(map_harness_error)?,
    };
    let completed_steps = string_list(args.get("completed_steps"))?;
    let pending_steps = string_list(args.get("pending_steps"))?;
    let mut goal = ctx
        .monitor
        .create_goal(
            objective,
            Some(task.id.clone()),
            auto_continue,
            stale_after_secs,
        )
        .map_err(map_error)?;
    if let Some(completed) = completed_steps.clone() {
        goal = ctx
            .monitor
            .update_progress(&goal.id, Some(completed), None, None)
            .map_err(map_error)?;
    }
    if let Some(pending) = pending_steps.clone() {
        goal = ctx
            .monitor
            .update_progress(&goal.id, None, Some(pending.clone()), None)
            .map_err(map_error)?;
    }
    if completed_steps.is_some() || pending_steps.is_some() {
        ctx.harness
            .update_steps(&task.id, completed_steps, pending_steps)
            .map_err(map_harness_error)?;
    }
    Ok(json!({
        "goal": goal,
        "task_id": task.id,
        "should_continue": auto_continue,
        "next": ["goal_update", "goal_status"]
    }))
}

fn goal_update(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let goal_id = resolve_goal_id(ctx, args)?;
    let completed_steps = string_list(args.get("completed_steps"))?;
    let pending_steps = string_list(args.get("pending_steps"))?;
    let note = args
        .get("note")
        .and_then(Value::as_str)
        .map(str::to_string);
    let goal = ctx
        .monitor
        .update_progress(
            &goal_id,
            completed_steps.clone(),
            pending_steps.clone(),
            note,
        )
        .map_err(map_error)?;
    if let Some(task_id) = goal.task_id.as_deref() {
        let _ = ctx
            .harness
            .update_steps(task_id, completed_steps, pending_steps);
    }
    let should_continue = goal.should_continue;
    Ok(json!({
        "goal": goal,
        "should_continue": should_continue,
        "next": ["continue working", "goal_status"]
    }))
}

fn goal_pause(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let goal_id = resolve_goal_id(ctx, args)?;
    let goal = ctx
        .monitor
        .set_status(&goal_id, GoalStatus::Paused, None)
        .map_err(map_error)?;
    if let Some(task_id) = goal.task_id.as_deref() {
        if let Ok(task) = ctx.harness.task(task_id) {
            if task.status == TaskStatus::Active {
                let _ = ctx.harness.transition(task_id, TaskStatus::Paused);
            }
        }
    }
    Ok(json!({"goal": goal, "should_continue": false}))
}

fn goal_resume(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let goal_id = resolve_goal_id(ctx, args)?;
    let goal = ctx
        .monitor
        .set_status(&goal_id, GoalStatus::Active, None)
        .map_err(map_error)?;
    if let Some(task_id) = goal.task_id.as_deref() {
        if let Ok(task) = ctx.harness.task(task_id) {
            if matches!(task.status, TaskStatus::Paused | TaskStatus::Failed) {
                let _ = ctx.harness.transition(task_id, TaskStatus::Active);
            }
        }
    }
    let should_continue = goal.auto_continue;
    Ok(json!({
        "goal": goal,
        "should_continue": should_continue,
        "continuation": "Goal 已恢复；继续执行剩余步骤，不要在目标完成前结束。"
    }))
}

fn goal_block(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let goal_id = resolve_goal_id(ctx, args)?;
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "reason 是必填项"))?
        .to_string();
    let goal = ctx
        .monitor
        .set_status(&goal_id, GoalStatus::Blocked, Some(reason.clone()))
        .map_err(map_error)?;
    if let Some(task_id) = goal.task_id.as_deref() {
        if let Ok(task) = ctx.harness.task(task_id) {
            if matches!(task.status, TaskStatus::Active | TaskStatus::Verifying) {
                let _ = ctx.harness.transition(task_id, TaskStatus::Failed);
            }
        }
    }
    Ok(json!({
        "goal": goal,
        "should_continue": false,
        "blocked_reason": reason,
        "next": ["ask user for the missing decision or resource", "goal_resume"]
    }))
}

fn goal_complete(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let goal_id = resolve_goal_id(ctx, args)?;
    let verified = args
        .get("verified")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let allow_unverified = args
        .get("allow_unverified")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !verified && !allow_unverified {
        return Err(tool_error(
            "GOAL_VERIFICATION_REQUIRED",
            "Goal 完成前必须完成项目验证并传 verified=true；无法验证时显式传 allow_unverified=true",
        ));
    }
    let current = ctx.monitor.goal(&goal_id).map_err(map_error)?;
    if current.status != GoalStatus::Active {
        return Err(tool_error(
            "GOAL_NOT_ACTIVE",
            "只有 active Goal 可以完成；请先恢复被暂停或阻塞的 Goal",
        ));
    }
    if let Some(task_id) = current.task_id.as_deref() {
        if let Ok(task) = ctx.harness.task(task_id) {
            if allow_unverified {
                if matches!(task.status, TaskStatus::Active | TaskStatus::Verifying) {
                    ctx
                        .harness
                        .transition(task_id, TaskStatus::CompletedUnverified)
                        .map_err(map_harness_error)?;
                } else if !matches!(
                    task.status,
                    TaskStatus::Completed | TaskStatus::CompletedUnverified
                ) {
                    return Err(tool_error(
                        "GOAL_TASK_NOT_COMPLETABLE",
                        "关联 Harness Task 当前不能完成，请先恢复 Task 并处理失败/暂停状态",
                    ));
                }
            } else {
                if task.status == TaskStatus::Active {
                    ctx.harness
                        .transition(task_id, TaskStatus::Verifying)
                        .map_err(map_harness_error)?;
                }
                let task = ctx.harness.task(task_id).map_err(map_harness_error)?;
                if task.status == TaskStatus::Verifying {
                    ctx.harness
                        .transition(task_id, TaskStatus::Completed)
                        .map_err(map_harness_error)?;
                } else if !matches!(
                    task.status,
                    TaskStatus::Completed | TaskStatus::CompletedUnverified
                ) {
                    return Err(tool_error(
                        "GOAL_TASK_NOT_COMPLETABLE",
                        "关联 Harness Task 当前不能完成，请先恢复 Task 并完成验证",
                    ));
                }
            }
        }
    }
    let goal = ctx
        .monitor
        .set_status(&goal_id, GoalStatus::Completed, None)
        .map_err(map_error)?;
    Ok(json!({
        "goal": goal,
        "verified": verified,
        "allow_unverified": allow_unverified,
        "should_continue": false
    }))
}

fn goal_clear(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let goal_id = resolve_goal_id(ctx, args)?;
    let goal = ctx
        .monitor
        .set_status(&goal_id, GoalStatus::Cleared, None)
        .map_err(map_error)?;
    Ok(json!({"goal": goal, "should_continue": false}))
}

fn resolve_goal_id(ctx: &ToolContext, args: &Value) -> Result<String, WorkspaceError> {
    if let Some(goal_id) = args.get("goal_id").and_then(Value::as_str) {
        if !goal_id.trim().is_empty() {
            return Ok(goal_id.to_string());
        }
    }
    ctx.monitor
        .current_goal()
        .map_err(map_error)?
        .map(|goal| goal.id)
        .ok_or_else(|| tool_error("GOAL_REQUIRED", "当前工作区没有活动 Goal"))
}

fn string_list(value: Option<&Value>) -> Result<Option<Vec<String>>, WorkspaceError> {
    let Some(value) = value else { return Ok(None) };
    let list = value
        .as_array()
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "步骤必须是字符串数组"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| tool_error("INVALID_ARGUMENT", "步骤必须是字符串数组"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(list))
}

fn map_error(error: crate::harness::HarnessError) -> WorkspaceError {
    tool_error(error.code(), error.to_string())
}

fn map_harness_error(error: crate::harness::HarnessError) -> WorkspaceError {
    tool_error(error.code(), error.to_string())
}

fn tool_error(code: &'static str, message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code,
        message: message.into(),
        category: "goal",
        retryable: matches!(code, "GOAL_ALREADY_ACTIVE" | "GOAL_REQUIRED"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::tools::context::ToolContext;

    fn context() -> (tempfile::TempDir, tempfile::TempDir, ToolContext) {
        let workspace = tempdir().expect("workspace");
        let harness_root = tempdir().expect("harness");
        std::fs::write(workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
        let ctx = ToolContext::for_test(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("context");
        (workspace, harness_root, ctx)
    }

    #[test]
    fn goal_lifecycle_drives_harness_task() {
        let (_workspace, _harness_root, ctx) = context();
        let created = call(
            &ctx,
            "goal_create",
            &json!({
                "objective": "完成自动化监控",
                "pending_steps": ["实现 monitor", "运行测试"]
            }),
        )
        .expect("create");
        assert_eq!(created["goal"]["status"], "active");
        assert_eq!(created["should_continue"], true);
        let goal_id = created["goal"]["id"].as_str().expect("goal id");
        let task_id = created["task_id"].as_str().expect("task id");

        let updated = call(
            &ctx,
            "goal_update",
            &json!({
                "goal_id": goal_id,
                "completed_steps": ["实现 monitor"],
                "pending_steps": ["运行测试"]
            }),
        )
        .expect("update");
        assert_eq!(updated["goal"]["completed_steps"][0], "实现 monitor");
        assert_eq!(
            ctx.harness.task(task_id).expect("task").pending_steps,
            vec!["运行测试".to_string()]
        );

        let paused = call(&ctx, "goal_pause", &json!({"goal_id": goal_id})).expect("pause");
        assert_eq!(paused["goal"]["status"], "paused");
        assert_eq!(ctx.harness.task(task_id).expect("task").status, TaskStatus::Paused);

        let resumed = call(&ctx, "goal_resume", &json!({"goal_id": goal_id})).expect("resume");
        assert_eq!(resumed["goal"]["status"], "active");
        assert_eq!(ctx.harness.task(task_id).expect("task").status, TaskStatus::Active);

        let handoff = call(&ctx, "goal_handoff", &json!({"goal_id": goal_id}))
            .expect("handoff");
        assert_eq!(handoff["handoff_requested"], true);
        assert_eq!(handoff["should_continue"], true);
        assert_eq!(handoff["goal"]["continuation_count"], 1);

        let rejected = call(&ctx, "goal_complete", &json!({"goal_id": goal_id}))
            .expect_err("verification required");
        assert_eq!(rejected.to_error_value()["code"], "GOAL_VERIFICATION_REQUIRED");

        let completed = call(
            &ctx,
            "goal_complete",
            &json!({"goal_id": goal_id, "verified": true}),
        )
        .expect("complete");
        assert_eq!(completed["goal"]["status"], "completed");
        assert_eq!(ctx.harness.task(task_id).expect("task").status, TaskStatus::Completed);
    }
}

