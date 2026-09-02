#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

impl SortDirection {
    pub(super) const fn sql(self) -> &'static str {
        match self {
            Self::Ascending => " ASC",
            Self::Descending => " DESC",
        }
    }

    pub const fn reversed(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }
}
