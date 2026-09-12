use crate::library::types::DBString;

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Genre {
    pub id: i64,
    pub name: DBString,
    pub normalized_name: DBString,
}
