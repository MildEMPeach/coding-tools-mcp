mod access_logs;
mod app_info;
mod audit;
mod frp_profiles;
mod health;
mod harness;
mod logs;
mod monitor;
pub(crate) mod runtime;
mod secrets;
mod software;
mod tunnel;
pub(crate) mod ui_memory;
pub(crate) mod window_chrome;
mod workspace;

pub use app_info::{check_app_update, open_url};
pub use access_logs::{open_http_access_log_directory, query_http_access_logs};
pub use audit::{
    clear_all_logs, get_audit_config, get_audit_record, get_audit_stats, query_audit_records,
    set_audit_config,
};
pub use ui_memory::{get_webview_memory_sample, recreate_ui_webview};
pub use window_chrome::{hide_to_tray, quit_app, show_main_window};
pub use frp_profiles::{
    delete_frp_profile, get_app_settings, get_last_workspace_id, get_proxy, list_frp_profiles,
    save_frp_profile, set_last_workspace, set_proxy,
};
pub use health::run_health_checks;
pub use harness::get_harness_dashboard;
pub use logs::read_workspace_logs;
pub use monitor::{
    clear_workspace_goal, create_workspace_goal, list_goal_dashboard, pause_workspace_goal,
    resume_workspace_goal,
};
pub use runtime::{
    get_actions_runtime_status, get_runtime_status, restart_actions_runtime, restart_runtime,
    start_actions_runtime, start_runtime, stop_actions_runtime, stop_runtime,
};
pub use secrets::{
    get_shared_secret, get_workspace_secret, regenerate_shared_secret,
    regenerate_workspace_secret, set_shared_secret, set_workspace_secret,
};
pub use software::{
    get_download_config, install_software, list_software, set_download_config,
    uninstall_software,
};
pub use tunnel::{get_frp_snippet, restart_tunnel, start_tunnel, stop_tunnel, test_tunnel};
pub use workspace::{
    create_workspace, delete_workspace, discover_upstream_tools, list_workspaces,
    open_workspace_directory, update_workspace,
};
