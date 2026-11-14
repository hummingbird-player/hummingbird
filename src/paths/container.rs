// This handles iOS container paths so they're rewritten when the app is updated.

pub const MARKER: &str = "/Containers/Data/Application/";
const ID_LEN: usize = 36;

/// Whether `id` looks like a container UUID (`XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`).
pub fn is_container_id(id: &str) -> bool {
    id.len() == ID_LEN
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// The UUID of the container that `path` is inside, if any.
pub fn container_id(path: &str) -> Option<&str> {
    let start = path.find(MARKER)? + MARKER.len();
    let id = path.get(start..start + ID_LEN)?;
    is_container_id(id).then_some(id)
}

/// Rewrites every container path in `text` so it points into the container `current_id`.
///
/// Returns `None` when nothing needed changing.
pub fn relocate(text: &str, current_id: &str) -> Option<String> {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    let mut changed = false;

    while let Some(position) = rest.find(MARKER) {
        let id_start = position + MARKER.len();
        output.push_str(&rest[..id_start]);
        rest = &rest[id_start..];

        if let Some(id) = rest.get(..ID_LEN)
            && is_container_id(id)
            && id != current_id
        {
            output.push_str(current_id);
            rest = &rest[ID_LEN..];
            changed = true;
        }
    }

    output.push_str(rest);
    changed.then_some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "DD524E6D-B0D4-4C30-8CBF-467822C668C9";
    const NEW: &str = "0A1B2C3D-4E5F-6071-8293-A4B5C6D7E8F9";

    #[test]
    fn finds_container_id() {
        let home = format!("/private/var/mobile/Containers/Data/Application/{NEW}");
        assert_eq!(container_id(&home), Some(NEW));
        assert_eq!(container_id("/Users/someone"), None);
        assert_eq!(
            container_id("/var/mobile/Containers/Data/Application/not-a-uuid"),
            None
        );
    }

    #[test]
    fn relocates_every_stale_path() {
        let text = format!(
            r#"{{"paths":["/private/var/mobile/Containers/Data/Application/{OLD}/Documents"],"q":"/var/mobile/Containers/Data/Application/{OLD}/Documents/a.ogg"}}"#
        );
        let expected = format!(
            r#"{{"paths":["/private/var/mobile/Containers/Data/Application/{NEW}/Documents"],"q":"/var/mobile/Containers/Data/Application/{NEW}/Documents/a.ogg"}}"#
        );
        assert_eq!(relocate(&text, NEW), Some(expected));
    }

    #[test]
    fn leaves_current_and_unrelated_paths_alone() {
        let current = format!("/var/mobile/Containers/Data/Application/{NEW}/Documents");
        assert_eq!(relocate(&current, NEW), None);
        assert_eq!(relocate("/Users/someone/Music", NEW), None);
        assert_eq!(
            relocate("/var/mobile/Containers/Data/Application/short", NEW),
            None
        );
    }
}
