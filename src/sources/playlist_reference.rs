//! Credential-free references for Hummingbird M3U round trips. Other players need
//! completed downloads for portable export; these URIs are identities, not streams.
use super::{SourceId, TrackRef};

pub fn encode(reference: &TrackRef) -> anyhow::Result<String> {
    if reference.source().is_local() {
        let path = reference
            .database_location()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path"))?;
        anyhow::ensure!(
            !path.contains(['\r', '\n']),
            "playlist paths cannot contain line breaks"
        );
        return Ok(path.to_owned());
    }
    let mut uri = url::Url::parse("hummingbird://track")?;
    uri.query_pairs_mut()
        .append_pair("source", reference.source().as_str())
        .append_pair("location", reference.remote_id().unwrap());
    Ok(uri.to_string())
}

pub fn decode(value: &str) -> anyhow::Result<TrackRef> {
    if !value.starts_with("hummingbird:") {
        return Ok(TrackRef::local(value));
    }
    let uri = url::Url::parse(value)?;
    anyhow::ensure!(
        uri.host_str() == Some("track")
            && uri.username().is_empty()
            && uri.password().is_none()
            && uri.port().is_none()
            && uri.path().is_empty()
            && uri.fragment().is_none(),
        "invalid Hummingbird track reference"
    );
    let mut source = None;
    let mut location = None;
    for (key, value) in uri.query_pairs() {
        match key.as_ref() {
            "source" if source.is_none() => source = Some(SourceId::new(value.into_owned())),
            "location" if location.is_none() => location = Some(value.into_owned()),
            _ => anyhow::bail!("invalid Hummingbird track reference fields"),
        }
    }
    let source = source.ok_or_else(|| anyhow::anyhow!("missing source"))?;
    anyhow::ensure!(
        !source.is_local() && !source.as_str().is_empty(),
        "invalid remote source"
    );
    let location = location.ok_or_else(|| anyhow::anyhow!("missing location"))?;
    Ok(TrackRef::from_database(source, location))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_preserves_opaque_ids_without_path_normalization() {
        let reference =
            TrackRef::from_database(SourceId::new("account-a"), "../song?x=1&y=2#雪\n%".into());
        let encoded = encode(&reference).unwrap();
        assert!(!encoded.contains('\n'));
        assert_eq!(decode(&encoded).unwrap(), reference);
        assert_eq!(
            decode("/Music/a.flac").unwrap(),
            TrackRef::local("/Music/a.flac")
        );
        assert!(decode("hummingbird://password@track?source=a&location=b").is_err());
        assert!(decode("hummingbird://track?source=a&source=b&location=x").is_err());
    }
}
