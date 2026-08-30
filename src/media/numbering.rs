use std::sync::LazyLock;

use regex::Regex;

const MAX_STORED_NUMBER: u64 = i32::MAX as u64;
// Bound vinyl-single labels, which repeat the side letter once per track.
const MAX_VINYL_SINGLE_TRACK: i32 = 64;

#[derive(sqlx::Type, Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
pub enum NumberDisplayMode {
    #[default]
    Standard = 0,
    Vinyl = 1,
    VinylSingle = 2,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ParsedTrackNumber {
    pub disc: Option<u64>,
    pub track: u64,
    pub track_max: Option<u64>,
    pub section: Option<u64>,
    pub mode: NumberDisplayMode,
}

static TRACK_POSITION_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^([A-Z]*)(\d*)(?:[.\-](\d+))?(?:/(\d+))?$").unwrap());

fn parse_number(value: &str) -> Option<u64> {
    value
        .parse()
        .ok()
        .filter(|number| *number <= MAX_STORED_NUMBER)
}

pub fn parse_track_number(value: &str) -> Option<ParsedTrackNumber> {
    let captures = TRACK_POSITION_REGEX.captures(value.trim())?;
    let letters = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
    let digits = captures.get(2).map(|m| m.as_str()).unwrap_or_default();
    let section = match captures.get(3) {
        Some(value) => Some(parse_number(value.as_str())?),
        None => None,
    };
    let track_max = match captures.get(4) {
        Some(value) => Some(parse_number(value.as_str())?),
        None => None,
    };

    match (letters.is_empty(), digits.is_empty()) {
        (true, false) => Some(ParsedTrackNumber {
            track: parse_number(digits)?,
            track_max,
            section,
            ..ParsedTrackNumber::default()
        }),
        (false, true) => {
            let (disc, track) = decode_side_run(letters)?;
            Some(ParsedTrackNumber {
                disc: Some(disc),
                track,
                section,
                mode: NumberDisplayMode::VinylSingle,
                ..ParsedTrackNumber::default()
            })
        }
        (false, false) => Some(ParsedTrackNumber {
            disc: Some(side_number(letters)?),
            track: parse_number(digits)?,
            section,
            mode: NumberDisplayMode::Vinyl,
            ..ParsedTrackNumber::default()
        }),
        (true, true) => None,
    }
}

fn side_number(letters: &str) -> Option<u64> {
    let letter = letters.chars().next()?;
    if letters.len() != 1 || !letter.is_ascii_alphabetic() {
        return None;
    }
    Some((letter.to_ascii_uppercase() as u64) - ('A' as u64) + 1)
}

fn decode_side_run(letters: &str) -> Option<(u64, u64)> {
    let letter = letters.chars().next()?.to_ascii_uppercase();
    if !letter.is_ascii_uppercase()
        || letters.len() > MAX_VINYL_SINGLE_TRACK as usize
        || !letters
            .chars()
            .all(|candidate| candidate.to_ascii_uppercase() == letter)
    {
        return None;
    }
    Some(((letter as u64) - ('A' as u64) + 1, letters.len() as u64))
}

pub fn side_letter(disc: i32) -> Option<String> {
    let offset = u8::try_from(disc.checked_sub(1)?).ok()?;
    (offset < 26).then(|| ((b'A' + offset) as char).to_string())
}

fn numeric_position(disc: Option<i32>, track: i32) -> String {
    match disc.filter(|disc| *disc > 0) {
        Some(disc) => format!("{disc}-{track}"),
        None => track.to_string(),
    }
}

pub fn format_track_position(
    mode: NumberDisplayMode,
    disc: Option<i32>,
    track: Option<i32>,
    section: Option<i32>,
) -> Option<String> {
    let track = track.filter(|track| *track >= 0)?;
    let section = match section {
        Some(section) if section >= 0 => Some(section),
        Some(_) => return None,
        None => None,
    };
    let suffix = section
        .map(|section| format!(".{section}"))
        .unwrap_or_default();

    let position = match mode {
        NumberDisplayMode::Standard => track.to_string(),
        NumberDisplayMode::Vinyl => side_letter(disc.unwrap_or(1))
            .map(|side| format!("{side}{track}"))
            .unwrap_or_else(|| numeric_position(disc, track)),
        NumberDisplayMode::VinylSingle if track <= MAX_VINYL_SINGLE_TRACK => {
            side_letter(disc.unwrap_or(1))
                .map(|side| {
                    if track == 0 {
                        format!("{side}0")
                    } else {
                        side.repeat(track as usize)
                    }
                })
                .unwrap_or_else(|| numeric_position(disc, track))
        }
        NumberDisplayMode::VinylSingle => numeric_position(disc, track),
    };

    Some(format!("{position}{suffix}"))
}

pub fn format_track_table_position(
    mode: NumberDisplayMode,
    disc: Option<i32>,
    track: Option<i32>,
    section: Option<i32>,
) -> Option<String> {
    let label = format_track_position(mode, disc, track, section)?;
    match mode {
        NumberDisplayMode::Standard => Some(match disc {
            Some(disc) => format!("{disc}-{label}"),
            None => label,
        }),
        _ => Some(label),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_positions() {
        let cases = [
            ("1", None, 1, None, None, NumberDisplayMode::Standard),
            ("1/12", None, 1, None, Some(12), NumberDisplayMode::Standard),
            ("1-2", None, 1, Some(2), None, NumberDisplayMode::Standard),
            ("A1.2", Some(1), 1, Some(2), None, NumberDisplayMode::Vinyl),
            ("B3", Some(2), 3, None, None, NumberDisplayMode::Vinyl),
            ("A", Some(1), 1, None, None, NumberDisplayMode::VinylSingle),
            ("AA", Some(1), 2, None, None, NumberDisplayMode::VinylSingle),
            (
                "BBB.1",
                Some(2),
                3,
                Some(1),
                None,
                NumberDisplayMode::VinylSingle,
            ),
        ];

        for (value, disc, track, section, track_max, mode) in cases {
            assert_eq!(
                parse_track_number(value),
                Some(ParsedTrackNumber {
                    disc,
                    track,
                    track_max,
                    section,
                    mode,
                }),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_unsupported_positions() {
        for value in [
            "",
            "Intro",
            "Side A",
            "AB",
            "AA1",
            "1-A",
            "1.1.1",
            "2147483648",
        ] {
            assert_eq!(parse_track_number(value), None, "{value}");
        }
        assert_eq!(
            parse_track_number(&"A".repeat(MAX_VINYL_SINGLE_TRACK as usize + 1)),
            None
        );
    }

    #[test]
    fn formats_positions_and_bounds_repetition() {
        let cases = [
            (
                NumberDisplayMode::Standard,
                Some(1),
                Some(3),
                Some(1),
                "3.1",
            ),
            (NumberDisplayMode::Vinyl, Some(2), Some(3), Some(2), "B3.2"),
            (NumberDisplayMode::VinylSingle, Some(1), Some(2), None, "AA"),
            (
                NumberDisplayMode::VinylSingle,
                Some(2),
                Some(3),
                Some(1),
                "BBB.1",
            ),
            (NumberDisplayMode::Vinyl, Some(27), Some(3), None, "27-3"),
            (
                NumberDisplayMode::VinylSingle,
                Some(1),
                Some(65),
                None,
                "1-65",
            ),
        ];

        for (mode, disc, track, section, expected) in cases {
            assert_eq!(
                format_track_position(mode, disc, track, section).as_deref(),
                Some(expected)
            );
        }
        assert_eq!(
            format_track_table_position(NumberDisplayMode::Standard, Some(2), Some(3), Some(1))
                .as_deref(),
            Some("2-3.1")
        );
    }
}
