use crate::library::types::DBString;

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Genre {
    #[allow(dead_code)]
    pub id: i64,
    pub name: DBString,
    #[allow(dead_code)]
    pub normalized_name: DBString,
}
