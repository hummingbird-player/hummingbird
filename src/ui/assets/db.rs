use std::borrow::Cow;

use anyhow::anyhow;
use sqlx::SqlitePool;
use url::Url;

pub fn load(pool: &SqlitePool, url: Url) -> gpui::Result<Option<Cow<'static, [u8]>>> {
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("missing table name"))?;
    match host {
        "album" | "track" => {
            let mut segments = url.path_segments().ok_or_else(|| anyhow!("missing path"))?;
            let id: i64 = segments
                .next()
                .ok_or_else(|| anyhow!("missing id"))?
                .parse()?;
            let image_type = segments
                .next()
                .ok_or_else(|| anyhow!("missing image type"))?;

            let query = match (host, image_type) {
                ("album", "thumb") => include_str!("../../../queries/assets/find_album_thumb.sql"),
                ("album", "full") => include_str!("../../../queries/assets/find_album_art.sql"),
                ("track", "thumb") => {
                    include_str!("../../../queries/assets/find_track_thumb.sql")
                }
                ("track", "full") => include_str!("../../../queries/assets/find_track_art.sql"),
                _ => unimplemented!("invalid image type '{image_type}'"),
            };

            let row: Option<(Option<Vec<u8>>,)> =
                crate::RUNTIME.block_on(sqlx::query_as(query).bind(id).fetch_optional(pool))?;

            match row {
                Some((Some(image),)) if !image.is_empty() => Ok(Some(Cow::Owned(image))),
                _ => Ok(None),
            }
        }
        _ => Ok(None),
    }
}
