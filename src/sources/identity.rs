use std::{
    fmt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row, sqlite::SqliteRow};

/// Stable configured account identity, independent of protocol, URL and credentials.
/// The reserved local ID needs no allocation; cloned remote references share storage.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceId(Option<std::sync::Arc<str>>);

impl SourceId {
    pub fn new(value: impl AsRef<str>) -> Self {
        let value = value.as_ref();
        if value == "local" {
            Self::local()
        } else {
            Self(Some(value.into()))
        }
    }
    pub fn local() -> Self {
        Self(None)
    }
    pub fn is_local(&self) -> bool {
        self.0.is_none()
    }
    pub fn as_str(&self) -> &str {
        self.0.as_deref().unwrap_or("local")
    }
}
impl Default for SourceId {
    fn default() -> Self {
        Self::local()
    }
}
impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl Serialize for SourceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for SourceId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::new(String::deserialize(deserializer)?))
    }
}
impl sqlx::Type<sqlx::Sqlite> for SourceId {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
}
impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for SourceId {
    fn decode(value: sqlx::sqlite::SqliteValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        Ok(Self::new(<&str as sqlx::Decode<sqlx::Sqlite>>::decode(
            value,
        )?))
    }
}
impl<'q> sqlx::Encode<'q, sqlx::Sqlite> for SourceId {
    fn encode_by_ref(
        &self,
        buffer: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<sqlx::Sqlite>>::encode_by_ref(&self.as_str(), buffer)
    }
}

/// Remote IDs are opaque, case-sensitive strings. Only local locations expose paths.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TrackLocation {
    Local(PathBuf),
    Remote(String),
}

/// Playable identity. Construction and deserialization keep source and location consistent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TrackRef {
    source: SourceId,
    location: TrackLocation,
}
impl TrackRef {
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self {
            source: SourceId::local(),
            location: TrackLocation::Local(path.into()),
        }
    }
    pub fn from_database(source: SourceId, location: String) -> Self {
        let location = if source.is_local() {
            TrackLocation::Local(location.into())
        } else {
            TrackLocation::Remote(location)
        };
        Self { source, location }
    }
    pub fn source(&self) -> &SourceId {
        &self.source
    }
    pub fn location(&self) -> &TrackLocation {
        &self.location
    }
    pub fn local_path(&self) -> Option<&Path> {
        match &self.location {
            TrackLocation::Local(path) => Some(path),
            TrackLocation::Remote(_) => None,
        }
    }
    pub fn remote_id(&self) -> Option<&str> {
        match &self.location {
            TrackLocation::Remote(id) => Some(id),
            TrackLocation::Local(_) => None,
        }
    }
    /// SQLite retains the existing UTF-8-only path policy; never lossily rewrite an identity.
    pub fn database_location(&self) -> Option<&str> {
        match &self.location {
            TrackLocation::Local(path) => path.to_str(),
            TrackLocation::Remote(id) => Some(id),
        }
    }
}
impl From<PathBuf> for TrackRef {
    fn from(path: PathBuf) -> Self {
        Self::local(path)
    }
}
impl From<&Path> for TrackRef {
    fn from(path: &Path) -> Self {
        Self::local(path)
    }
}
impl fmt::Display for TrackRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.location {
            TrackLocation::Local(path) => path.display().fmt(f),
            TrackLocation::Remote(id) => write!(f, "{}:{id}", self.source),
        }
    }
}
impl<'r> FromRow<'r, SqliteRow> for TrackRef {
    fn from_row(row: &'r SqliteRow) -> sqlx::Result<Self> {
        Ok(Self::from_database(
            row.try_get("source")?,
            row.try_get("location")?,
        ))
    }
}
impl Serialize for TrackRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeStruct};
        let mut value = serializer.serialize_struct("TrackRef", 2)?;
        value.serialize_field("source", &self.source)?;
        value.serialize_field(
            "location",
            self.database_location()
                .ok_or_else(|| S::Error::custom("track path is not UTF-8"))?,
        )?;
        value.end()
    }
}
impl<'de> Deserialize<'de> for TrackRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Reference {
                #[serde(default)]
                source: SourceId,
                location: String,
            },
            Legacy(PathBuf),
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Reference { source, location } => Self::from_database(source, location),
            Wire::Legacy(path) => Self::local(path),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_ids_are_opaque_and_account_scoped() {
        let a = TrackRef::from_database(SourceId::new("a"), "/Music/../Song\\A%2F".into());
        let b = TrackRef::from_database(SourceId::new("b"), a.remote_id().unwrap().into());
        assert_ne!(a, b);
        assert!(a.local_path().is_none());
        assert_eq!(a.remote_id(), Some("/Music/../Song\\A%2F"));
        assert_eq!(
            serde_json::from_str::<TrackRef>(&serde_json::to_string(&a).unwrap()).unwrap(),
            a
        );
    }
    #[test]
    fn legacy_identity_is_local() {
        let reference: TrackRef = serde_json::from_str(r#""/Music/song.flac""#).unwrap();
        assert!(reference.source().is_local());
        assert_eq!(reference.local_path(), Some(Path::new("/Music/song.flac")));
    }
}
