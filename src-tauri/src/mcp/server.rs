use std::sync::Arc;

use serde_json::Value;

use crate::audit::{AuditRequestContext, AuditStore};
use crate::mcp::upstream::UpstreamMcpManager;
use crate::tools::{
    call_tool_with_audit, list_tools_for_profile, record_tool_rejection_with_audit,
    wrap_mcp_tool_result, SharedToolContext, ToolContext, Workspace,
};
use crate::workspace::AuthConfig;

pub struct McpState {
    pub tools: SharedToolContext,
    pub upstream: Arc<UpstreamMcpManager>,
}

fn handle_resources_read(params: &Value) -> Result<Value, Value> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| serde_json::json!({ "code": -32602, "message": "Missing resource uri" }))?;
    if uri != crate::monitor::GOAL_WIDGET_URI {
        return Err(serde_json::json!({
            "code": -32602,
            "message": format!("Unknown resource: {uri}")
        }));
    }
    Ok(serde_json::json!({
        "contents": [{
            "uri": crate::monitor::GOAL_WIDGET_URI,
            "mimeType": "text/html;profile=mcp-app",
            "text": crate::monitor::goal_widget_html(),
            "_meta": {
                "ui": {
                    "prefersBorder": true
                }
            }
        }]
    }))
}

impl McpState {
    pub fn audit_store(&self) -> Option<AuditStore> {
        self.tools.audit_store()
    }
}

pub type SharedState = Arc<McpState>;

// 生产入口接收监听层采集的 headers 上下文；原 handle_request 仅为无 HTTP 环境的测试生成
// 最小上下文。协议分发保持与 Axum 解耦，且只让 tools/call 进入工具审计。
#[cfg(test)]
pub fn handle_request(state: &SharedState, body: &Value) -> Value {
    let request = AuditRequestContext {
        transport: "mcp".into(),
        method: body
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string),
        request_id: body.get("id").and_then(crate::audit::request_id_from_value),
        route: Some("/mcp".into()),
        ..AuditRequestContext::default()
    };
    handle_request_with_context(state, body, &request)
}

pub fn handle_request_with_context(
    state: &SharedState,
    body: &Value,
    request: &AuditRequestContext,
) -> Value {
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let params = body.get("params").cloned().unwrap_or(Value::Null);

    if id.is_null() && method.starts_with("notifications/") {
        return Value::Null;
    }

    let result = match method {
        "initialize" => Ok(initialize_result()),
        "ping" => Ok(serde_json::json!({})),
        "resources/list" => Ok(serde_json::json!({
            "resources": [{
                "uri": crate::monitor::GOAL_WIDGET_URI,
                "name": "goal-status",
                "title": "Goal Status",
                "description": "Render the current coding Goal and optionally request a ChatGPT follow-up turn.",
                "mimeType": "text/html;profile=mcp-app"
            }]
        })),
        "resources/read" => handle_resources_read(&params),
        "tools/list" => {
            let mut tools = list_tools_for_profile(&state.tools.tool_profile);
            tools.extend(state.upstream.public_tools().iter().cloned());
            Ok(serde_json::json!({ "tools": tools }))
        }
        "tools/call" => handle_tools_call(state, &params, request),
        _ => Err(serde_json::json!({
            "code": -32601,
            "message": format!("Method not found: {method}")
        })),
    };

    match result {
        Ok(result) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    }
}

fn initialize_result() -> Value {
    const INSTRUCTIONS: &str = "Use these tools only for local coding operations inside the configured workspace. At the start of every new ChatGPT conversation, before answering the user's first request, call history_session_bootstrap exactly once and pass the user's verbatim first request as initial_user_input. Immediately after bootstrap, call harness_status and goal_status. If goal_status returns an active Goal, restore its objective and pending steps before taking new substantial work. When an active Goal has should_continue=true, keep working toward that Goal instead of ending the turn merely because one intermediate step finished; update it with goal_update after meaningful milestones. If progress is impossible without user input or an external dependency, use goal_block with the concrete reason. Only call goal_complete after project-specific verification passes, unless verification truly cannot run and allow_unverified=true is explicit. If the Goal is still active when you are ready to end the current ChatGPT turn, call goal_handoff as your final Goal tool call. Its MCP Apps Goal UI can request a follow-up message from ChatGPT so the web client continues the same Goal without any separate model API key. For a new request that explicitly asks for a long-running/autonomous objective or continued work until a condition is met, call goal_create; it will attach to or create the Harness Task. For ordinary multi-step coding that does not need autonomous continuation, use the Harness Task lifecycle directly. If harness_status reports an active task, use task_context before continuing substantial work. If there is no active task and the user's request requires multiple code edits, command/test iterations, or durable progress tracking, call start_task with a concise objective; simple read-only questions and one-off operations may remain in standalone mode. During a tracked task, call update_task when meaningful milestones or pending steps change. Use project_state, operation_log, git_status, and git_diff to recover context or investigate a baseline mismatch. Before claiming a tracked task is complete, run the relevant project-specific verification. Call finish_task with verified=true only after verification passes; if verification cannot be run, use allow_unverified=true and state the limitation. Treat bootstrap as required conversation initialization: it creates or resumes a lossless Markdown archive and returns bounded current state, not all history. Use history_session_search followed by history_session_read only when exact earlier context is needed. history_session_read returns a bounded UTF-8-safe page; follow next_cursor with the returned content hash until the relevant archive is complete. Repeated successful bootstrap calls in the same conversation resume the same session and must not create duplicates. Preserve session_key and current_path returned by bootstrap, then pass them unchanged as session_key and expected_path to every history_session_checkpoint call. After completing each user-requested task in the conversation, call history_session_checkpoint before the final response and pass that user's verbatim request as raw_user_input. Only state that progress was saved after checkpoint returns ok=true with the same session_key and path. The MCP server never calls an OpenAI model API itself; model reasoning remains in ChatGPT. The Goal UI uses the ChatGPT/MCP Apps follow-up-message bridge when the host supports it. The server cannot access ChatGPT transcript text that was not provided as a tool argument; persistence is not automatic background persistence.";
    serde_json::json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false },
            "logging": {}
        },
        "serverInfo": {
            "name": "coding-tools-mcp",
            "title": "Coding Tools MCP",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": INSTRUCTIONS
    })
}

fn handle_tools_call(
    state: &SharedState,
    params: &Value,
    request: &AuditRequestContext,
) -> Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| serde_json::json!({ "code": -32602, "message": "Missing tool name" }))?;
    let args = tool_arguments(name, params);

    if state.upstream.owns_tool(name) {
        let result = tauri::async_runtime::block_on(state.upstream.call_tool(name, args));
        return Ok(normalize_upstream_result(result));
    }

    let canonical_name = crate::tools::registry::canonical_tool_name(name);
    let known = crate::tools::registry::exposed_tool_names(&state.tools.tool_profile);
    // 未知/未暴露工具在 dispatcher 前提前返回，拒绝记录必须放在这里；通过校验的路径改用
    // audited wrapper，审计成功与否都不改变原 MCP 结果。
    if !known.iter().any(|n| n == &canonical_name) {
        let message = format!("Unknown tool: {name}");
        record_tool_rejection_with_audit(
            state.tools.as_ref(),
            request,
            name,
            &args,
            "UNKNOWN_TOOL",
            &message,
        );
        return Err(serde_json::json!({
            "code": -32602,
            "message": message,
            "data": { "reason": "unknown_tool" }
        }));
    }

    let structured = call_tool_with_audit(state.tools.as_ref(), canonical_name, &args, request);
    Ok(wrap_mcp_tool_result(canonical_name, &args, structured))
}

fn normalize_upstream_result(result: Result<Value, String>) -> Value {
    match result {
        Ok(result) if result.get("content").is_some() => result,
        Ok(result) => serde_json::json!({
            "content": [{ "type": "text", "text": result.to_string() }],
            "structuredContent": result,
            "isError": false
        }),
        Err(message) => serde_json::json!({
            "content": [{ "type": "text", "text": message }],
            "structuredContent": {
                "ok": false,
                "status": "error",
                "error": {
                    "category": "upstream_mcp",
                    "message": "本地 MCP 工具调用失败"
                }
            },
            "isError": true
        }),
    }
}

fn tool_arguments(name: &str, params: &Value) -> Value {
    let mut args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if name.starts_with("history_session_") {
        if let Some(session_key) = params
            .get("_meta")
            .and_then(|meta| meta.get("openai/session"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !args.is_object() {
                args = serde_json::json!({});
            }
            args["_host_session_key"] = Value::String(session_key.to_string());
        }
    }
    args
}

// 审计在 SharedState 创建时绑定：此处同时拥有正式 workspace_id，且尚未被多请求共享；
// 测试构造路径可不调用 with_audit，因此不会写入用户数据库。
pub fn new_state(
    workspace: Workspace,
    workspace_id: String,
    auth: AuthConfig,
    policy: crate::tools::policy::PolicySettings,
    tool_profile: String,
    permission_mode: String,
    upstream: Arc<UpstreamMcpManager>,
) -> SharedState {
    Arc::new(McpState {
        tools: Arc::new(
            ToolContext::from_workspace(workspace, auth, policy, tool_profile, permission_mode)
                .with_audit(workspace_id),
        ),
        upstream,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use serde_json::json;

    use crate::mcp::upstream::UpstreamMcpManager;
    use crate::tools::ToolContext;

    use super::{handle_request, initialize_result, tool_arguments, McpState};

    #[test]
    fn initialize_instructions_define_the_history_persistence_workflow() {
        let initialized = initialize_result();
        let instructions = initialized["instructions"].as_str().expect("instructions");
        assert!(instructions.contains("history_session_bootstrap"));
        assert!(instructions.contains("At the start of every new ChatGPT conversation"));
        assert!(instructions.contains("before answering the user's first request"));
        assert!(instructions.contains("required conversation initialization"));
        assert!(instructions.contains("initial_user_input"));
        assert!(instructions.contains("must not create duplicates"));
        assert!(instructions.contains("history_session_checkpoint"));
        assert!(instructions.contains("raw_user_input"));
        assert!(instructions.contains("history_session_search"));
        assert!(instructions.contains("history_session_read"));
        assert!(instructions.contains("follow next_cursor"));
        assert!(instructions.contains("session_key and current_path returned by bootstrap"));
        assert!(instructions.contains("session_key and expected_path"));
        assert!(instructions.contains("After completing each user-requested task"));
        assert!(instructions.contains("before the final response"));
        assert!(instructions.contains("checkpoint returns ok=true"));
        assert!(instructions.contains("not automatic background persistence"));
    }

    #[test]
    fn initialize_instructions_define_the_harness_workflow() {
        let initialized = initialize_result();
        let instructions = initialized["instructions"].as_str().expect("instructions");

        assert!(instructions.contains("call harness_status"));
        assert!(instructions.contains("multiple code edits"));
        assert!(instructions.contains("call start_task"));
        assert!(instructions.contains("call update_task"));
        assert!(instructions.contains("finish_task with verified=true"));
        assert!(instructions.contains("simple read-only questions"));
        assert!(instructions.contains("standalone mode"));
    }

    #[test]
    fn initialize_instructions_define_the_goal_monitor_workflow() {
        let initialized = initialize_result();
        let instructions = initialized["instructions"].as_str().expect("instructions");

        assert!(instructions.contains("call harness_status and goal_status"));
        assert!(instructions.contains("goal_create"));
        assert!(instructions.contains("goal_update"));
        assert!(instructions.contains("goal_block"));
        assert!(instructions.contains("goal_complete"));
        assert!(instructions.contains("should_continue=true"));
        assert!(instructions.contains("goal_handoff"));
        assert!(instructions.contains("MCP Apps Goal UI"));
        assert!(instructions.contains("never calls an OpenAI model API"));
    }

    #[test]
    fn initialize_does_not_claim_tool_catalog_notifications_without_a_stream() {
        let initialized = initialize_result();

        assert_eq!(initialized["capabilities"]["tools"]["listChanged"], false);
        assert_eq!(initialized["capabilities"]["resources"]["listChanged"], false);
    }

    #[test]
    fn goal_handoff_tool_exposes_the_goal_ui_resource() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(McpState {
            tools: Arc::new(
                ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                    .expect("tool context"),
            ),
            upstream: Arc::new(UpstreamMcpManager::empty()),
        });
        let response = handle_request(
            &state,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        );
        let tools = response["result"]["tools"].as_array().expect("tools");
        let goal = tools
            .iter()
            .find(|tool| tool["name"] == "goal_handoff")
            .expect("goal_handoff");
        assert_eq!(goal["_meta"]["ui"]["resourceUri"], crate::monitor::GOAL_WIDGET_URI);
    }

    #[test]
    fn goal_ui_resource_is_readable_as_mcp_app_html() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(McpState {
            tools: Arc::new(
                ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                    .expect("tool context"),
            ),
            upstream: Arc::new(UpstreamMcpManager::empty()),
        });
        let response = handle_request(
            &state,
            &json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"resources/read",
                "params":{"uri": crate::monitor::GOAL_WIDGET_URI}
            }),
        );
        assert_eq!(
            response["result"]["contents"][0]["mimeType"],
            "text/html;profile=mcp-app"
        );
        assert!(response["result"]["contents"][0]["text"]
            .as_str()
            .expect("widget html")
            .contains("sendFollowUpMessage"));
    }

    #[test]
    fn workspace_prompt_initializes_or_restores_a_chatgpt_session() {
        let component = include_str!("../../../src/lib/components/ChatGptSessionPrompt.svelte");

        assert!(component.contains("ChatGPT 新会话启动提示词"));
        assert!(component.contains("请初始化或恢复当前项目会话"));
        assert!(component.contains("如果没有历史记录"));
        assert!(component.contains("initial_user_input"));
        assert!(component.contains("raw_user_input"));
        assert!(component.contains("history_session_search"));
        assert!(component.contains("history_session_checkpoint"));
        assert!(!component.contains("打开连接器设置"));
    }

    #[test]
    fn chatgpt_session_metadata_is_injected_only_for_history_tools() {
        let params = json!({
            "arguments": {"session_key": "explicit"},
            "_meta": {"openai/session": "chatgpt-conversation"}
        });
        let history = tool_arguments("history_session_bootstrap", &params);
        assert_eq!(history["session_key"], "explicit");
        assert_eq!(history["_host_session_key"], "chatgpt-conversation");

        let existing = tool_arguments("read_file", &params);
        assert_eq!(existing["session_key"], "explicit");
        assert!(existing.get("_host_session_key").is_none());
    }

    #[test]
    fn host_session_key_takes_precedence_over_explicit_session_key() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(McpState {
            tools: Arc::new(
                ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                    .expect("tool context"),
            ),
            upstream: Arc::new(UpstreamMcpManager::empty()),
        });
        let response = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "history_session_bootstrap",
                    "arguments": {
                        "session_key": "explicit-session",
                        "initial_user_input": "保存首轮原文"
                    },
                    "_meta": {"openai/session": "chatgpt-session"}
                }
            }),
        );
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["ok"], true);
        assert_eq!(structured["session_key_source"], "platform_conversation_id");
        assert_eq!(structured["session_key"], "chatgpt-session");
        assert_eq!(structured["initial_input_captured"], true);
        let content = fs::read_to_string(workspace.path().join("docs/history-session/1.md"))
            .expect("read history file");
        assert!(content.contains("**Session key:** chatgpt-session"));
        assert!(!content.contains("**Session key:** explicit-session"));
    }

    #[test]
    fn legacy_grep_calls_are_mapped_to_the_public_grep_text_tool() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        fs::write(workspace.path().join("sample.txt"), "catalog needle")
            .expect("write sample file");
        let state = Arc::new(McpState {
            tools: Arc::new(
                ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                    .expect("tool context"),
            ),
            upstream: Arc::new(UpstreamMcpManager::empty()),
        });

        let response = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "grep",
                    "arguments": {"query": "needle", "path": "."}
                }
            }),
        );

        assert!(response.get("error").is_none());
        assert_eq!(response["result"]["structuredContent"]["ok"], true);
    }
}
