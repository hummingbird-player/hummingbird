use cntp_i18n::trn;
use gpui::SharedString;

pub fn format_collection_summary(track_count: i64, total_duration: i64) -> SharedString {
    let track_label = format!(
        "{}",
        trn!(
            "COLLECTION_SUMMARY_TRACKS",
            "{{count}} track",
            "{{count}} tracks",
            count = track_count
        )
    );

    SharedString::from(format!(
        "{track_label} • {}",
        format_total_duration(total_duration)
    ))
}

fn format_total_duration(total_duration: i64) -> String {
    let total_duration = total_duration.max(0);
    let hours = total_duration / 3_600;
    let minutes = (total_duration % 3_600) / 60;
    let seconds = total_duration % 60;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}
