use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::audit::AuditStore;
use crate::harness::Harness;
use crate::monitor::GoalMonitor;
use crate::tools::policy::PolicySettings;
use crate::tools::session::SessionStore;
use crate::tools::workspace::{relative_display, Workspace};
use crate::workspace::AuthConfig;

pub struct ToolContext {
    pub workspace: Workspace,
    pub auth: AuthConfig,
    pub policy: PolicySettings,
    pub tool_profile: String,
    pub permission_mode: String,
    pub harness: Harness,
    pub monitor: GoalMonitor,
    // 审计附着在现有 ToolContext，避免修改全部工具签名；生产监听器显式启用，测试与内部
    // 构造保持 None。正式 profile_id 绑定前暂用 Harness 的稳定工作区 ID 作为兼容标签。
    audit: Option<AuditStore>,
    audit_workspace_id: String,
    default_cwd: Mutex<PathBuf>,
    pub sessions: Arc<SessionStore>,
}

pub type SharedToolContext = Arc<ToolContext>;

impl ToolContext {
    pub fn new(workspace_path: PathBuf) -> Result<Self, String> {
        let workspace = Workspace::new(workspace_path).map_err(|e| e.message())?;
        let auth = AuthConfig {
            auth_type: "noauth".into(),
            ..AuthConfig::default()
        };
        Ok(Self::from_workspace(
            workspace,
            auth,
            PolicySettings::default(),
            "full".into(),
            "trusted".into(),
        ))
    }

    pub fn from_workspace(
        workspace: Workspace,
        auth: AuthConfig,
        policy: PolicySettings,
        tool_profile: String,
        permission_mode: String,
    ) -> Self {
        let harness_root = Harness::default_root().expect("无法初始化 Harness 数据目录");
        let monitor_root = GoalMonitor::default_root().expect("无法初始化 Goal Monitor 数据目录");
        Self::from_workspace_with_roots(
            workspace,
            auth,
            policy,
            crate::tools::registry::normalize_tool_profile(&tool_profile).into(),
            permission_mode,
            harness_root,
            monitor_root,
        )
    }

    pub fn from_workspace_with_harness_root(
        workspace: Workspace,
        auth: AuthConfig,
        policy: PolicySettings,
        tool_profile: String,
        permission_mode: String,
        harness_root: PathBuf,
    ) -> Self {
        let monitor_root = harness_root.join("monitor-test");
        Self::from_workspace_with_roots(
            workspace,
            auth,
            policy,
            tool_profile,
            permission_mode,
            harness_root,
            monitor_root,
        )
    }

    fn from_workspace_with_roots(
        workspace: Workspace,
        auth: AuthConfig,
        policy: PolicySettings,
        tool_profile: String,
        permission_mode: String,
        harness_root: PathBuf,
        monitor_root: PathBuf,
    ) -> Self {
        let root = workspace.root().to_path_buf();
        let harness = Harness::new(root.clone(), harness_root).expect("无法初始化 Harness");
        let monitor = GoalMonitor::new(
            harness.workspace_id().to_string(),
            None,
            monitor_root,
        )
        .expect("无法初始化 Goal Monitor");
        let audit_workspace_id = harness.workspace_id().to_string();
        Self {
            workspace,
            auth,
            policy,
            tool_profile: crate::tools::registry::normalize_tool_profile(&tool_profile).into(),
            permission_mode,
            harness,
            monitor,
            audit: None,
            audit_workspace_id,
            default_cwd: Mutex::new(root),
            sessions: Arc::new(SessionStore::new()),
        }
    }

    pub fn for_test(workspace_path: PathBuf, harness_root: PathBuf) -> Result<Self, String> {
        let workspace = Workspace::new(workspace_path).map_err(|e| e.message())?;
        Ok(Self::from_workspace_with_harness_root(
            workspace,
            AuthConfig {
                auth_type: "noauth".into(),
                ..AuthConfig::default()
            },
            PolicySettings::default(),
            "full".into(),
            "trusted".into(),
            harness_root,
        ))
    }

    pub fn workspace_path(&self) -> String {
        self.workspace.root_display()
    }

    // 此步骤位于 profile_id 已确定、Context 尚未进入 Arc 的构造末端；开库失败时降级为
    // 无审计，不能影响工具服务可用性。
    pub fn with_audit(mut self, workspace_id: impl Into<String>) -> Self {
        let workspace_id = workspace_id.into();
        if !workspace_id.trim().is_empty() {
            match AuditStore::open_default() {
                Ok(audit) => {
                    self.audit = Some(audit);
                }
                Err(error) => eprintln!("audit store disabled: {error}"),
            }
            self.audit_workspace_id = workspace_id;
            if let Ok(root) = GoalMonitor::default_root() {
                if let Ok(monitor) = GoalMonitor::new(
                    self.harness.workspace_id().to_string(),
                    Some(self.audit_workspace_id.clone()),
                    root,
                ) {
                    self.monitor = monitor;
                }
            }
        }
        self
    }

    pub fn audit_workspace_id(&self) -> &str {
        &self.audit_workspace_id
    }

    pub fn default_cwd_display(&self) -> String {
        let cwd = self.default_cwd.lock().expect("cwd lock");
        relative_display(self.workspace.root(), &cwd)
    }

    pub fn set_default_cwd(&self, path: PathBuf) {
        *self.default_cwd.lock().expect("cwd lock") = path;
    }

    pub fn default_cwd_path(&self) -> PathBuf {
        self.default_cwd.lock().expect("cwd lock").clone()
    }

    pub fn audit_store(&self) -> Option<AuditStore> {
        self.audit.clone()
    }
}
