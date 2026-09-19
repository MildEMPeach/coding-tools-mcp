pub mod model;
pub mod runtime;
pub mod state;
pub mod store;
pub mod tools;
pub mod ui;

pub use model::{GoalHealth, GoalRecord, GoalStatus};
pub use runtime::ensure_monitor_loop;
pub use state::GoalMonitor;
pub use ui::{goal_widget_html, GOAL_WIDGET_URI};

