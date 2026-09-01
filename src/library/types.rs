#![allow(dead_code)]
pub mod table;

use gpui::{IntoElement, SharedString};
use sqlx::{Database, Decode, Sqlite, Type, encode::IsNull, error::BoxDynError};

pub use crate::library::model::{
    Album, Artist, ArtistWithCounts, Playlist, PlaylistItem, PlaylistType, Track, TrackStats,
};

#[derive(Clone, Default, Debug)]
pub struct DBString(pub SharedString);

impl From<String> for DBString {
    fn from(data: String) -> Self {
        Self(SharedString::from(data))
    }
}

impl From<&str> for DBString {
    fn from(data: &str) -> Self {
        Self(SharedString::from(data.to_string()))
    }
}

impl From<DBString> for SharedString {
    fn from(data: DBString) -> Self {
        data.0
    }
}

impl From<DBString> for String {
    fn from(data: DBString) -> Self {
        data.0.to_string()
    }
}

impl std::fmt::Display for DBString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl PartialEq for DBString {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl PartialEq<String> for DBString {
    fn eq(&self, other: &String) -> bool {
        self.0.as_ref() == other
    }
}

impl PartialEq<DBString> for String {
    fn eq(&self, other: &DBString) -> bool {
        self == other.0.as_ref()
    }
}

impl PartialEq<&str> for DBString {
    fn eq(&self, other: &&str) -> bool {
        self.0.as_ref() == *other
    }
}

impl PartialEq<DBString> for &str {
    fn eq(&self, other: &DBString) -> bool {
        *self == other.0.as_ref()
    }
}

impl IntoElement for DBString {
    type Element = <SharedString as IntoElement>::Element;

    fn into_element(self) -> Self::Element {
        self.0.into_element()
    }
}

impl<'q, DB: Database> sqlx::Encode<'q, DB> for DBString
where
    String: sqlx::Encode<'q, DB>,
{
    fn encode_by_ref(
        &self,
        out: &mut <DB as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        let string = self.0.to_string();
        <String>::encode_by_ref(&string, out)
    }
}

impl<'r, DB: Database> Decode<'r, DB> for DBString
where
    String: Decode<'r, DB>,
{
    fn decode(value: <DB as Database>::ValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let data = String::decode(value)?;
        Ok(Self::from(data))
    }
}

impl sqlx::Type<sqlx::Sqlite> for DBString {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as Type<Sqlite>>::type_info()
    }
}

pub const DATE_PRECISION_YEAR: i32 = 0;
pub const DATE_PRECISION_FULL_DATE: i32 = 1;
pub const DATE_PRECISION_YEAR_MONTH: i32 = 2;
