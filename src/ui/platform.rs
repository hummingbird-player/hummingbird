//! Platform-dependent layout decisions.

use std::sync::LazyLock;

/// Whether the UI uses the phone layout: a bottom navigation bar instead of the library
/// sidebar.
///
/// Always on for mobile targets. On desktop it can be forced with
/// `HUMMINGBIRD_MOBILE_LAYOUT=1`, which is handy for iterating on the mobile UI without a
/// simulator.
pub fn mobile_layout() -> bool {
    static MOBILE_LAYOUT: LazyLock<bool> = LazyLock::new(|| {
        cfg!(any(target_os = "ios", target_os = "android"))
            || std::env::var_os("HUMMINGBIRD_MOBILE_LAYOUT").is_some_and(|value| value != "0")
    });

    *MOBILE_LAYOUT
}
