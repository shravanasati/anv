/// Returns `true` if `ANV_DEBUG` is set in the environment.
pub fn is_debug() -> bool {
    std::env::var("ANV_DEBUG").is_ok()
}

/// Emit a debug line to stderr only when `ANV_DEBUG` is set in the environment.
///
/// Example: `dbg_log!("aniskip", "Fetching skip times for key {}", key);`
#[macro_export]
macro_rules! dbg_log {
    ($module:expr, $($arg:tt)*) => {
        if $crate::logger::is_debug() {
            eprintln!("[{}] {}", $module, format!($($arg)*));
        }
    };
}

