use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, WebviewWindow};

use crate::error::{AppError, AppResult};

/// When true, CloseRequested may destroy the window / exit the process.
static ALLOW_EXIT: AtomicBool = AtomicBool::new(false);

/// Intercept user close (show confirm / keep running) unless quitting or UI recreate.
pub fn should_intercept_close() -> bool {
    !ALLOW_EXIT.load(Ordering::SeqCst) && !crate::commands::ui_memory::should_prevent_exit()
}

fn main_window(app: &AppHandle) -> AppResult<WebviewWindow> {
    app.get_webview_window("main")
        .or_else(|| {
            app.webview_windows()
                .into_iter()
                .find(|(label, _)| !label.starts_with("__"))
                .map(|(_, w)| w)
        })
        .ok_or_else(|| AppError::Message("找不到主窗口".into()))
}

#[tauri::command]
pub fn hide_to_tray(app: AppHandle) -> AppResult<()> {
    let window = main_window(&app)?;
    window
        .hide()
        .map_err(|err| AppError::Message(format!("隐藏窗口失败: {err}")))?;
    Ok(())
}

#[tauri::command]
pub async fn show_main_window(app: AppHandle) -> AppResult<()> {
    let _lock = crate::commands::ui_memory::UI_WINDOW_LOCK.lock().await;
    let window = match main_window(&app) {
        Ok(window) => window,
        Err(_) => crate::commands::ui_memory::build_main_window(&app, "main")?,
    };
    let _ = window.unminimize();
    window
        .show()
        .map_err(|err| AppError::Message(format!("显示窗口失败: {err}")))?;
    let _ = window.set_focus();
    if let Some(keepalive) = app.get_webview_window(crate::commands::ui_memory::KEEPALIVE_LABEL) {
        let _ = keepalive.destroy();
    }
    Ok(())
}

#[tauri::command]
pub fn quit_app(app: AppHandle) -> AppResult<()> {
    arm_allow_exit();
    app.exit(0);
    Ok(())
}

pub fn arm_allow_exit() {
    ALLOW_EXIT.store(true, Ordering::SeqCst);
}

/// Event handlers cannot await window creation (Windows needs it off the UI thread).
pub fn request_show_main_window(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = show_main_window(app).await {
            eprintln!("恢复主窗口失败: {error}");
        }
    });
}
