#![allow(dead_code)]
pub mod table;

use std::sync::Arc;

use gpui::{IntoElement, RenderImage, SharedString};
use image::{Frame, RgbaImage};
use smallvec::SmallVec;
use sqlx::{Database, Decode, Sqlite, Type, encode::IsNull, error::BoxDynError};

use crate::util::rgb_to_bgr;

pub use crate::library::model::{
    Album, Artist, ArtistWithCounts, Playlist, PlaylistItem, PlaylistType, Track, TrackStats,
};

#[derive(Clone)]
pub struct Thumbnail(pub Arc<RenderImage>);

impl Thumbnail {
    pub fn new(image: Arc<RenderImage>) -> Self {
        Self(image)
    }
}

impl From<Box<[u8]>> for Thumbnail {
    fn from(data: Box<[u8]>) -> Self {
        let mut image = image::load_from_memory(&data)
            .unwrap()
            .as_rgba8()
            .map(|image| image.to_owned())
            .unwrap_or_else(|| {
                let mut image = RgbaImage::new(1, 1);
                image.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
                image
            });

        rgb_to_bgr(&mut image);

        Self(Arc::new(RenderImage::new(SmallVec::from_vec(vec![
            Frame::new(image),
        ]))))
    }
}

impl<'r, DB: Database> Decode<'r, DB> for Thumbnail
where
    Box<[u8]>: Decode<'r, DB>,
{
    fn decode(value: <DB as Database>::ValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let data = <Box<[u8]>>::decode(value)?;
        Ok(Self::from(data))
    }
}

impl<'q, DB: Database> sqlx::Encode<'q, DB> for Thumbnail
where
    Box<[u8]>: sqlx::Encode<'q, DB>,
{
    fn encode_by_ref(
        &self,
        _: &mut <DB as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        panic!("Thumbnail is write-only")
    }
}

impl sqlx::Type<sqlx::Sqlite> for Thumbnail {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <Box<[u8]> as Type<Sqlite>>::type_info()
    }
}

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
