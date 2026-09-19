use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::time::sleep;

use crate::error::{AppError, AppResult};
use crate::platform::platform;

use super::cloudflare::apply_proxy_env;

const OPENAI_TUNNEL_VERSION: &str = "v0.0.14";

pub struct OpenAiTunnelHandle {
    pub child: Child,
    pub pid: Option<u32>,
}

pub fn resolve_tunnel_client() -> AppResult<PathBuf> {
    cached_tunnel_client_path()
        .filter(|path| path.is_file())
        .or_else(|| which::which(tunnel_client_binary_name()).ok())
        .ok_or_else(|| {
            AppError::Message(
                "未找到 OpenAI tunnel-client。请到「软件管理」安装，或从 openai/tunnel-client Releases 安装后加入 PATH。"
                    .into(),
            )
        })
}

pub(crate) fn cached_tunnel_client_path() -> Option<PathBuf> {
    platform()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("bin").join(tunnel_client_binary_name()))
}

pub(crate) fn tunnel_client_binary_name() -> &'static str {
    #[cfg(windows)]
    {
        "tunnel-client.exe"
    }
    #[cfg(not(windows))]
    {
        "tunnel-client"
    }
}

fn release_asset_name() -> AppResult<String> {
    let platform = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return Err(AppError::Message(
            "当前平台暂不支持自动下载 OpenAI tunnel-client。".into(),
        ));
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        return Err(AppError::Message(
            "当前 CPU 架构暂不支持自动下载 OpenAI tunnel-client。".into(),
        ));
    };
    Ok(format!(
        "tunnel-client-{OPENAI_TUNNEL_VERSION}-{platform}-{arch}.zip"
    ))
}

pub(crate) async fn download_tunnel_client_to_cache() -> AppResult<PathBuf> {
    let settings = crate::settings::AppSettings::load_or_default();
    let asset = release_asset_name()?;
    let url = format!(
        "https://github.com/openai/tunnel-client/releases/download/{OPENAI_TUNNEL_VERSION}/{asset}"
    );
    let dest = cached_tunnel_client_path()
        .ok_or_else(|| AppError::Message("无法解析缓存目录。".into()))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = super::download::download_release_asset(&settings, &url, "OpenAI tunnel-client").await?;
    extract_tunnel_client_zip(&bytes, &dest)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&dest) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&dest, perms);
        }
    }

    if dest.is_file() {
        Ok(dest)
    } else {
        Err(AppError::Message(
            "OpenAI tunnel-client 自动安装失败。".into(),
        ))
    }
}

fn extract_tunnel_client_zip(bytes: &[u8], dest: &Path) -> AppResult<()> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|err| AppError::Message(format!("解压 OpenAI tunnel-client 失败: {err}")))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| AppError::Message(format!("读取 OpenAI tunnel-client 安装包失败: {err}")))?;
        let name = entry.name().replace('\\', "/");
        if name.ends_with(tunnel_client_binary_name()) {
            let mut out = File::create(dest)?;
            std::io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    Err(AppError::Message(
        "OpenAI tunnel-client 安装包中未找到可执行文件。".into(),
    ))
}

pub fn valid_tunnel_id(value: &str) -> bool {
    let Some(suffix) = value.trim().strip_prefix("tunnel_") else {
        return false;
    };
    suffix.len() == 32
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub async fn spawn_openai_tunnel(
    local_port: u16,
    cwd: &Path,
    log_path: &Path,
    tunnel_id: &str,
    api_key: &str,
    use_proxy: bool,
) -> AppResult<OpenAiTunnelHandle> {
    if !valid_tunnel_id(tunnel_id) {
        return Err(AppError::Message(
            "OpenAI Tunnel ID 格式无效，应为 tunnel_ 加 32 位小写十六进制字符。".into(),
        ));
    }
    if api_key.trim().is_empty() {
        return Err(AppError::Message(
            "OpenAI Tunnel 需要 Runtime API Key（CONTROL_PLANE_API_KEY）。".into(),
        ));
    }

    let binary = resolve_tunnel_client()?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let stdout_file = File::create(log_path)?;
    let stderr_file = stdout_file.try_clone()?;

    let mut cmd = Command::new(binary);
    cmd.current_dir(cwd)
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .env("CONTROL_PLANE_API_KEY", api_key.trim())
        .args([
            "run",
            "--control-plane.tunnel-id",
            tunnel_id.trim(),
            "--control-plane.api-key",
            "env:CONTROL_PLANE_API_KEY",
            "--mcp.server-url",
            &format!("http://127.0.0.1:{local_port}/mcp"),
            "--health.listen-addr",
            "127.0.0.1:0",
            "--log.level",
            "info",
            "--log.format",
            "struct-text",
        ]);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    #[cfg(unix)]
    {
        cmd.process_group(0);
    }

    if use_proxy {
        let settings = crate::settings::AppSettings::load_or_default();
        apply_proxy_env(&mut cmd, &settings.proxy);
    }

    let mut child = cmd
        .spawn()
        .map_err(|err| AppError::Message(format!("启动 OpenAI tunnel-client 失败: {err}")))?;
    let pid = child.id();

    // Catch configuration/auth failures that terminate immediately. The daemon
    // otherwise keeps polling in the background under TunnelSupervisor.
    sleep(Duration::from_millis(900)).await;
    if let Some(status) = child
        .try_wait()
        .map_err(|err| AppError::Message(format!("检查 OpenAI tunnel-client 状态失败: {err}")))?
    {
        let detail = read_log_tail(log_path).unwrap_or_default();
        return Err(AppError::Message(format!(
            "OpenAI tunnel-client 启动后立即退出（{status}）。{}{}",
            if detail.is_empty() { "" } else { "\n" },
            detail
        )));
    }

    Ok(OpenAiTunnelHandle { child, pid })
}

fn read_log_tail(path: &Path) -> AppResult<String> {
    let mut file = File::open(path)?;
    let size = file.seek(SeekFrom::End(0))?;
    let start = size.saturating_sub(4096);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::valid_tunnel_id;

    #[test]
    fn validates_openai_tunnel_id() {
        assert!(valid_tunnel_id(
            "tunnel_0123456789abcdef0123456789abcdef"
        ));
        assert!(!valid_tunnel_id("tunnel_ABCDEF"));
        assert!(!valid_tunnel_id("tunnel_0123"));
    }
}
