use chrono::{DateTime, Utc};

use crate::library::types::DBString;

#[derive(sqlx::FromRow)]
pub struct Artist {
    #[allow(dead_code)]
    pub id: i64,
    pub name: DBString,
    #[allow(dead_code)]
    pub name_sortable: String,
    #[allow(dead_code)]
    pub created_at: DateTime<Utc>,
}
