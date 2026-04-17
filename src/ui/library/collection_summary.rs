use cntp_i18n::trn;
use gpui::SharedString;

use crate::ui::util::format_duration;

pub fn format_collection_summary(track_count: i64, total_duration: i64) -> SharedString {
    let track_label = trn!(
        "COLLECTION_SUMMARY_TRACKS",
        "{{count}} track",
        "{{count}} tracks",
        count = track_count
    );

    SharedString::from(format!(
        "{track_label} • {}",
        format_duration(total_duration, false)
    ))
}
