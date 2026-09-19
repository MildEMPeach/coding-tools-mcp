use std::time::Duration;

use serde::Serialize;

use crate::workspace::WorkspaceProfile;

const TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthItem {
    pub label: String,
    pub ok: bool,
    pub detail: String,
    pub hint: String,
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .expect("failed to build HTTP client")
}

fn format_single_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

fn format_field_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(format_single_value)
            .collect::<Vec<_>>()
            .join(" / "),
        other => format_single_value(other),
    }
}

async fn check_url(client: &reqwest::Client, url: &str) -> (bool, String) {
    if url.is_empty() {
        return (false, "URL not configured".to_string());
    }
    match client.get(url).send().await {
        Ok(response) => {
            let code = response.status().as_u16();
            let ok = matches!(code, 200 | 401 | 404);
            (ok, format!("HTTP {code}"))
        }
        Err(err) => (false, err.to_string()),
    }
}

async fn check_mcp_public_url(client: &reqwest::Client, url: &str) -> (bool, String) {
    if url.is_empty() {
        return (false, "URL not configured".to_string());
    }
    match client.get(url).send().await {
        Ok(response) => {
            let code = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            let lower = body.to_ascii_lowercase();
            if (lower.contains("powered by") && lower.contains("frp"))
                || (lower.contains("the page you requested was not found")
                    && lower.contains("frp"))
            {
                return (
                    false,
                    format!("HTTP {code}; FRP 未挂载代理（返回 frp 404 页）"),
                );
            }
            let ok = matches!(code, 200 | 401 | 405);
            (ok, format!("HTTP {code}"))
        }
        Err(err) => (false, err.to_string()),
    }
}

async fn check_json_field(client: &reqwest::Client, url: &str, field: &str) -> (bool, String) {
    if url.is_empty() {
        return (false, "URL not configured".to_string());
    }
    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status();
            if !status.is_success() {
                return (false, format!("HTTP {}", status.as_u16()));
            }
            match response.json::<serde_json::Value>().await {
                Ok(payload) => {
                    let value = payload
                        .get(field)
                        .map(format_field_value)
                        .unwrap_or_default();
                    (
                        true,
                        format!("HTTP {}; {field}={value}", status.as_u16()),
                    )
                }
                Err(err) => (false, err.to_string()),
            }
        }
        Err(err) => (false, err.to_string()),
    }
}

fn well_known_url(base: &str, path: &str) -> String {
    if base.is_empty() {
        return String::new();
    }
    format!("{}/{}", base.trim_end_matches('/'), path)
}

pub async fn run_health_checks(profile: &WorkspaceProfile) -> Vec<HealthItem> {
    let client = http_client();
    let openai_tunnel = profile.tunnel.tunnel_type == "openai";
    let mcp_public = profile.effective_public_url();
    let actions_local = profile.actions_local_base_url();
    let actions_public = profile.actions_effective_public_url();
    let actions_oauth_base = if actions_public.is_empty() {
        actions_local.clone()
    } else {
        actions_public.clone()
    };

    let (mcp_local_ok, mcp_local_detail) = check_url(&client, &profile.local_endpoint()).await;
    let (mcp_public_ok, mcp_public_detail, mcp_oauth_ok, mcp_oauth_detail, mcp_protected_ok, mcp_protected_detail) =
        if openai_tunnel {
            let id_ok = crate::tunnel::valid_tunnel_id(&profile.tunnel.openai_tunnel_id);
            let client_ok = crate::tunnel::resolve_tunnel_client().is_ok();
            let secret_ok = crate::secret::SecretStore::get(&profile.id, "openai_tunnel_api_key")
                .ok()
                .flatten()
                .is_some_and(|value| !value.trim().is_empty());
            let embedded_oauth = profile.auth.oauth_enabled();
            let ready = id_ok && client_ok && secret_ok && !embedded_oauth;
            (
                ready,
                if ready {
                    format!("Secure MCP Tunnel · {}", profile.tunnel.openai_tunnel_id)
                } else {
                    format!(
                        "Tunnel ID={} · tunnel-client={} · Runtime API Key={} · 内置 OAuth={}",
                        if id_ok { "OK" } else { "缺失/无效" },
                        if client_ok { "OK" } else { "未安装" },
                        if secret_ok { "OK" } else { "未配置" },
                        if embedded_oauth { "不兼容" } else { "未启用" },
                    )
                },
                !embedded_oauth,
                if embedded_oauth {
                    "当前项目的 OAuth 授权页位于本机 /oauth/authorize；OpenAI Tunnel 不会公开该浏览器授权页。请改用“不启用认证”或 Bearer Token。".to_string()
                } else {
                    "当前未使用内置 OAuth。Secure MCP Tunnel 会把 MCP 请求转发到本机服务。".to_string()
                },
                !embedded_oauth,
                if embedded_oauth {
                    "内置 OAuth 需要可从浏览器直接访问的 authorization endpoint。".to_string()
                } else {
                    "当前不需要内置 OAuth protected-resource discovery。".to_string()
                },
            )
        } else {
            let (public_ok, public_detail) =
                check_mcp_public_url(&client, &profile.public_endpoint()).await;
            let (oauth_ok, oauth_detail) = check_json_field(
                &client,
                &well_known_url(&mcp_public, ".well-known/oauth-authorization-server"),
                "token_endpoint_auth_methods_supported",
            )
            .await;
            let (protected_ok, protected_detail) = check_json_field(
                &client,
                &well_known_url(&mcp_public, ".well-known/oauth-protected-resource"),
                "authorization_servers",
            )
            .await;
            (
                public_ok,
                public_detail,
                oauth_ok,
                oauth_detail,
                protected_ok,
                protected_detail,
            )
        };

    let actions_health_url = format!("{actions_local}/health");
    let actions_openapi_local = format!("{actions_local}/openapi.json");
    let actions_openapi_public = profile.actions_openapi_url();

    let (actions_local_ok, actions_local_detail) =
        check_url(&client, &actions_health_url).await;
    let (actions_openapi_local_ok, actions_openapi_local_detail) =
        check_url(&client, &actions_openapi_local).await;
    let (actions_openapi_public_ok, actions_openapi_public_detail) =
        check_url(&client, &actions_openapi_public).await;
    let (actions_oauth_ok, actions_oauth_detail) = check_json_field(
        &client,
        &well_known_url(&actions_oauth_base, ".well-known/oauth-authorization-server"),
        "token_endpoint_auth_methods_supported",
    )
    .await;

    vec![
        health_item("本地 /mcp", mcp_local_ok, mcp_local_detail, "确认 MCP 服务已启动，端口与工作区配置一致。"),
        health_item(if openai_tunnel { "OpenAI Secure MCP Tunnel" } else { "公网 /mcp" }, mcp_public_ok, mcp_public_detail, if openai_tunnel { "确认 Tunnel ID、Runtime API Key、tunnel-client 已配置，并关闭当前项目的内置 OAuth。" } else { "检查隧道是否已连接，或公网 URL 是否填写正确。" }),
        health_item("MCP OAuth 授权元数据", mcp_oauth_ok, mcp_oauth_detail, "MCP 认证需设为 OAuth，且公网地址可访问。"),
        health_item("MCP OAuth 受保护资源", mcp_protected_ok, mcp_protected_detail, "确认公网 MCP 根地址与 OAuth 配置一致。"),
        health_item("本地 Actions /health", actions_local_ok, actions_local_detail, "确认 Actions 服务已启动。"),
        health_item("本地 Actions /openapi.json", actions_openapi_local_ok, actions_openapi_local_detail, "Actions 监听器异常时请查看 actions-stderr.log。"),
        health_item("公网 Actions /openapi.json", actions_openapi_public_ok, actions_openapi_public_detail, "检查 Actions 隧道与子域名配置。"),
        health_item("Actions OAuth 授权元数据", actions_oauth_ok, actions_oauth_detail, "Actions 认证需设为 OAuth，公网地址需可达。"),
    ]
}

fn health_item(label: &str, ok: bool, detail: String, hint: &str) -> HealthItem {
    HealthItem {
        label: label.into(),
        ok,
        detail,
        hint: if ok { String::new() } else { hint.into() },
    }
}
