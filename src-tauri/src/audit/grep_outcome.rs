//! 系统 grep 的审计分类：无匹配也是正常查询结果。
//!
//! | 结果 | 审计状态 |
//! | --- | --- |
//! | 正常匹配 | success |
//! | 正常无匹配 | success |
//! | 文件/权限/参数错误、崩溃或超时 | failure |
//! | 安全策略拒绝 | rejected |
//!
//! 无匹配须正常退出 1、无工具/传输错误，且 stderr 为空、未截断。
//! grep -c 输出 0 也属无匹配；只修正审计分类，保留原始退出码与输出。
//!
//! ## 支持边界
//!
//! 仅支持系统标准 grep 的退出码约定，按命令写法识别，不校验可执行文件身份。
//! 工作目录或 PATH 中的同名自定义 grep（含包装器）可能误判。
//! 这是预期边界，暂不处理自定义 grep。
//!
//! 内置 MCP grep/grep_text/search_text 不走此规则；直接执行也不读取 Shell 别名。
//!
//! ## 异步关联
//!
//! - 按工作区、传输来源与 session_id 关联已知 grep 会话。
//! - 只缓存会话身份，不保存命令正文；详情关闭时仍可分类。
//! - 来源未知、重启、淘汰或锁异常时，沿用基础分类。

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::Value;

use super::ResponseMetadata;

// 限制长期运行时的内存占用；淘汰最早记录后，对该会话保守分类。
const MAX_GREP_SESSIONS: usize = 256;

#[derive(Default)]
pub(super) struct GrepSessions {
    sessions: VecDeque<(String, String, String)>,
}

/// 先保留基础三态，仅将满足模块契约的标准 grep 无匹配结果转为 success。
pub(super) fn classify(
    sessions: &Mutex<GrepSessions>,
    workspace_id: &str,
    transport: &str,
    tool_name: &str,
    args: &Value,
    output: &Value,
) -> ResponseMetadata {
    let mut response = super::response_metadata(tool_name, output);
    // 显式工具/传输错误优先，不能被进程退出码覆盖。
    if output.get("ok").and_then(Value::as_bool) != Some(true)
        || output.get("transport_ok").and_then(Value::as_bool) != Some(true)
        || output.get("error").is_some_and(|error| !error.is_null())
    {
        return response;
    }

    let session_id = output.get("session_id").and_then(Value::as_str);
    let is_grep = match tool_name {
        "exec_command" => args
            .get("cmd")
            .and_then(Value::as_str)
            .and_then(|command| shell_words::split(command).ok())
            .and_then(|parts| parts.into_iter().next())
            // 仅匹配约定写法，身份假设见模块说明。
            .is_some_and(|program| {
                matches!(
                    program.as_str(),
                    "grep" | "grep.exe" | "/usr/bin/grep" | "/bin/grep"
                )
            }),
        "write_stdin" => {
            let input_id = args.get("session_id").and_then(Value::as_str);
            input_id.is_some()
                && input_id == session_id
                && sessions
                    .lock()
                    .map(|state| {
                        state.sessions.iter().any(|(workspace, source, id)| {
                            workspace == workspace_id
                                && source == transport
                                && Some(id.as_str()) == input_id
                        })
                    })
                    .unwrap_or(false)
        }
        _ => false,
    };

    if is_grep
        && tool_name == "exec_command"
        && output.get("status").and_then(Value::as_str) == Some("running")
    {
        if let Some(id) = session_id.filter(|id| !id.is_empty()) {
            if let Ok(mut state) = sessions.lock() {
                let key = (
                    workspace_id.to_string(),
                    transport.to_string(),
                    id.to_string(),
                );
                if !state.sessions.contains(&key) {
                    if state.sessions.len() == MAX_GREP_SESSIONS {
                        state.sessions.pop_front();
                    }
                    state.sessions.push_back(key);
                }
            }
        }
    }

    let no_match = is_grep
        && output.get("command_ok").and_then(Value::as_bool) == Some(false)
        && output.get("status").and_then(Value::as_str) == Some("exited")
        && output.get("termination_reason").and_then(Value::as_str) == Some("exited")
        && output.get("exit_code").and_then(Value::as_i64) == Some(1)
        && output.get("stderr").and_then(Value::as_str) == Some("")
        && output.get("stderr_truncated").and_then(Value::as_bool) != Some(true);
    if no_match {
        response.status = "success".into();
        response.is_error = false;
        response.error_code = None;
        response.error_message = None;
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{current_time_ms, AuditQuery, AuditRequestContext, AuditStore};
    use serde_json::json;
    use tempfile::tempdir;

    fn no_match() -> Value {
        json!({"ok":true, "transport_ok":true, "command_ok":false, "status":"exited",
            "termination_reason":"exited", "exit_code":1, "stdout":"", "stderr":"",
            "stderr_truncated":false})
    }

    fn record(store: &AuditStore, tool: &str, args: &Value, output: &Value) -> String {
        let now = current_time_ms();
        store
            .record_tool_call(
                &AuditRequestContext {
                    transport: "mcp".into(),
                    ..Default::default()
                },
                "workspace",
                "/tmp/workspace",
                tool,
                args,
                output,
                now,
                now + 1,
            )
            .expect("审计写入成功")
            .expect("返回审计 ID")
    }

    #[test]
    fn no_match_is_success_but_original_exit_and_output_are_preserved() {
        let dir = tempdir().unwrap();
        let store = AuditStore::open(dir.path().join("audit.sqlite")).unwrap();
        for command in [
            "grep missing file.txt",
            "grep -c missing file.txt",
            "grep -v present file.txt",
        ] {
            let mut output = no_match();
            if command.contains("-c") {
                output["stdout"] = json!("0\n");
            }
            let id = record(&store, "exec_command", &json!({"cmd":command}), &output);
            let row = store.get_record(&id).unwrap().unwrap();
            assert_eq!(row.status, "success");
            assert!(!row.is_error);
            assert_eq!(row.exit_code, Some(1));
            assert_eq!(
                serde_json::from_str::<Value>(row.output_json.as_deref().unwrap()).unwrap(),
                output
            );
        }
        let stats = store.stats(&AuditQuery::default()).unwrap();
        assert_eq!(
            (stats.total_calls, stats.success_calls, stats.failure_calls),
            (3, 3, 0)
        );
        assert!(store
            .query(&AuditQuery {
                successful: Some(false),
                ..Default::default()
            })
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .query(&AuditQuery {
                    successful: Some(true),
                    ..Default::default()
                })
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn runtime_errors_rejections_and_other_exit_one_commands_remain_errors() {
        let sessions = Mutex::new(GrepSessions::default());
        for (key, value) in [
            ("ok", json!(false)),
            ("transport_ok", json!(false)),
            ("exit_code", json!(2)),
            ("status", json!("spawn_failed")),
            ("termination_reason", json!("timeout")),
            ("stderr", json!("grep: file: Permission denied\n")),
            ("stderr_truncated", json!(true)),
            ("stderr", Value::Null),
            (
                "error",
                json!({"category":"policy", "code":"POLICY_REJECTED"}),
            ),
        ] {
            let mut output = no_match();
            output[key] = value;
            let result = classify(
                &sessions,
                "workspace",
                "mcp",
                "exec_command",
                &json!({"cmd":"grep missing file"}),
                &output,
            );
            assert_ne!(result.status, "success", "{key} 必须保留失败/拒绝");
            assert!(result.is_error);
        }
        for command in [
            "false",
            "python3 -c 'invalid'",
            "./grep missing file",
            "custom/grep x f",
            "sh -c 'grep missing file'",
            "echo grep",
            "git diff --check",
            "grep '",
        ] {
            assert_eq!(
                classify(
                    &sessions,
                    "workspace",
                    "mcp",
                    "exec_command",
                    &json!({"cmd":command}),
                    &no_match()
                )
                .status,
                "failure",
                "{command}"
            );
        }
    }

    #[test]
    fn async_grep_polling_works_with_metadata_only_and_store_clones() {
        let dir = tempdir().unwrap();
        let store = AuditStore::open(dir.path().join("audit.sqlite")).unwrap();
        let mut config = store.config().unwrap();
        config.detail_limit_bytes = -1;
        store.set_config(config).unwrap();
        let running = json!({"ok":true,"transport_ok":true,"status":"running","session_id":"grep-session",
            "command_ok":null,"termination_reason":"running","exit_code":null,"stdout":"","stderr":""});
        record(
            &store,
            "exec_command",
            &json!({"cmd":"grep -R missing ."}),
            &running,
        );
        let clone = store.clone();
        let mut output = no_match();
        output["session_id"] = json!("grep-session");
        // 重复读取同一终态也应稳定，不能第一次读成功、第二次又变失败。
        for _ in 0..2 {
            let id = record(
                &clone,
                "write_stdin",
                &json!({"session_id":"grep-session","chars":""}),
                &output,
            );
            let row = clone.get_record(&id).unwrap().unwrap();
            assert_eq!(row.status, "success");
            assert_eq!(row.exit_code, Some(1));
            assert!(row.input_json.is_none() && row.output_json.is_none() && row.command.is_none());
        }
    }

    #[test]
    fn session_identity_is_scoped_and_unknown_sessions_are_conservative() {
        let sessions = Mutex::new(GrepSessions::default());
        let running =
            json!({"ok":true,"transport_ok":true,"status":"running","session_id":"shared-id"});
        classify(
            &sessions,
            "workspace",
            "mcp",
            "exec_command",
            &json!({"cmd":"grep x file"}),
            &running,
        );
        let mut output = no_match();
        output["session_id"] = json!("shared-id");
        for (workspace, transport, id) in [
            ("other", "mcp", "shared-id"),
            ("workspace", "actions", "shared-id"),
            ("workspace", "mcp", "unknown"),
        ] {
            assert_eq!(
                classify(
                    &sessions,
                    workspace,
                    transport,
                    "write_stdin",
                    &json!({"session_id":id}),
                    &output
                )
                .status,
                "failure"
            );
        }
        output["termination_reason"] = json!("timeout");
        assert_eq!(
            classify(
                &sessions,
                "workspace",
                "mcp",
                "write_stdin",
                &json!({"session_id":"shared-id"}),
                &output
            )
            .status,
            "failure"
        );
    }

    #[test]
    fn remembered_session_types_have_a_fixed_upper_bound() {
        let sessions = Mutex::new(GrepSessions::default());
        for i in 0..=MAX_GREP_SESSIONS {
            classify(
                &sessions,
                "workspace",
                "mcp",
                "exec_command",
                &json!({"cmd":"grep x file"}),
                &json!({"ok":true,"transport_ok":true,"status":"running","session_id":format!("s-{i}")}),
            );
        }
        let state = sessions.lock().unwrap();
        assert_eq!(state.sessions.len(), MAX_GREP_SESSIONS);
        assert_eq!(state.sessions.front().unwrap().2, "s-1");
    }

    #[cfg(unix)]
    #[test]
    fn real_grep_through_dispatcher_counts_no_match_but_not_missing_file() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("sample.txt"), "present\n").unwrap();
        let ctx =
            crate::tools::context::ToolContext::for_test(workspace, dir.path().join("harness"))
                .unwrap();
        let store = AuditStore::open(dir.path().join("audit.sqlite")).unwrap();
        for (command, expected) in [
            ("grep absent sample.txt", "success"),
            ("grep present sample.txt", "success"),
            ("grep absent missing.txt", "failure"),
        ] {
            let args = json!({"cmd":command,"workdir":".","yield_time_ms":1000,"timeout_ms":5000});
            let output = crate::tools::call_tool(&ctx, "exec_command", &args);
            let id = record(&store, "exec_command", &args, &output);
            assert_eq!(
                store.get_record(&id).unwrap().unwrap().status,
                expected,
                "{command}: {output}"
            );
            if command == "grep absent sample.txt" {
                assert_eq!(output["exit_code"], 1);
                assert_eq!(output["command_ok"], false);
            }
        }
        let stats = store.stats(&AuditQuery::default()).unwrap();
        assert_eq!(
            (stats.total_calls, stats.success_calls, stats.failure_calls),
            (3, 2, 1)
        );
        assert_eq!(
            store
                .query(&AuditQuery {
                    successful: Some(false),
                    ..Default::default()
                })
                .unwrap()
                .len(),
            1
        );
    }
}
