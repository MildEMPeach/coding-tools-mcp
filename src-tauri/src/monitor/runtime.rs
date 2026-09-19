use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::time::{sleep, Duration};

use crate::data::DataStore;
use crate::harness::Harness;

use super::GoalMonitor;

static MONITOR_LOOP_STARTED: AtomicBool = AtomicBool::new(false);
const MONITOR_INTERVAL: Duration = Duration::from_secs(15);

pub fn ensure_monitor_loop() {
    if MONITOR_LOOP_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async {
        loop {
            sleep(MONITOR_INTERVAL).await;
            if let Err(error) = refresh_all_goals() {
                eprintln!("goal monitor refresh failed: {error}");
            }
        }
    });
}

fn refresh_all_goals() -> Result<(), String> {
    let profiles = DataStore::read_file(|data| Ok(data.profiles.clone()))
        .map_err(|error| error.to_string())?;
    let harness_root = Harness::default_root().map_err(|error| error.to_string())?;
    let monitor_root = GoalMonitor::default_root().map_err(|error| error.to_string())?;
    for profile in profiles {
        let Ok(harness) = Harness::new(PathBuf::from(&profile.path), harness_root.clone()) else {
            continue;
        };
        let Ok(monitor) = GoalMonitor::new(
            harness.workspace_id().to_string(),
            Some(profile.id.clone()),
            monitor_root.clone(),
        ) else {
            continue;
        };
        let _ = monitor.refresh_current(&harness);
    }
    Ok(())
}

