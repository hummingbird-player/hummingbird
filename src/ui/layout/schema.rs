use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OuterBand {
    Main,
    Controls,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MainRegion {
    LibrarySidebar,
    LibraryContent,
    SidePanel,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UiLayout {
    pub outer_order: [OuterBand; 2],
    pub main_order: [MainRegion; 3],
    pub library: LibraryLayout,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TwoColumnPane {
    Browse,
    Detail,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LibraryLayout {
    pub two_column_order: [TwoColumnPane; 2],
}

impl Default for UiLayout {
    fn default() -> Self {
        Self {
            outer_order: [OuterBand::Main, OuterBand::Controls],
            main_order: [
                MainRegion::LibrarySidebar,
                MainRegion::LibraryContent,
                MainRegion::SidePanel,
            ],
            library: LibraryLayout::default(),
        }
    }
}

impl Default for LibraryLayout {
    fn default() -> Self {
        Self {
            two_column_order: [TwoColumnPane::Browse, TwoColumnPane::Detail],
        }
    }
}

impl UiLayout {
    pub fn is_valid(&self) -> bool {
        is_outer_permutation(self.outer_order)
            && is_main_permutation(self.main_order)
            && is_two_column_permutation(self.library.two_column_order)
    }

    pub fn validated(self) -> Option<Self> {
        self.is_valid().then_some(self)
    }
}

fn is_outer_permutation(order: [OuterBand; 2]) -> bool {
    let mut seen_main = false;
    let mut seen_controls = false;

    for band in order {
        match band {
            OuterBand::Main if !seen_main => seen_main = true,
            OuterBand::Controls if !seen_controls => seen_controls = true,
            _ => return false,
        }
    }

    seen_main && seen_controls
}

fn is_main_permutation(order: [MainRegion; 3]) -> bool {
    let mut seen_sidebar = false;
    let mut seen_content = false;
    let mut seen_side_panel = false;

    for region in order {
        match region {
            MainRegion::LibrarySidebar if !seen_sidebar => seen_sidebar = true,
            MainRegion::LibraryContent if !seen_content => seen_content = true,
            MainRegion::SidePanel if !seen_side_panel => seen_side_panel = true,
            _ => return false,
        }
    }

    seen_sidebar && seen_content && seen_side_panel
}

fn is_two_column_permutation(order: [TwoColumnPane; 2]) -> bool {
    let mut seen_browse = false;
    let mut seen_detail = false;

    for pane in order {
        match pane {
            TwoColumnPane::Browse if !seen_browse => seen_browse = true,
            TwoColumnPane::Detail if !seen_detail => seen_detail = true,
            _ => return false,
        }
    }

    seen_browse && seen_detail
}

#[cfg(test)]
mod tests {
    use super::{LibraryLayout, MainRegion, OuterBand, TwoColumnPane, UiLayout};

    #[test]
    fn ui_layout_accepts_reordered_bands_and_panes() {
        let layout = UiLayout {
            outer_order: [OuterBand::Controls, OuterBand::Main],
            main_order: [
                MainRegion::SidePanel,
                MainRegion::LibraryContent,
                MainRegion::LibrarySidebar,
            ],
            library: LibraryLayout {
                two_column_order: [TwoColumnPane::Detail, TwoColumnPane::Browse],
            },
        };

        assert!(layout.is_valid());
    }

    #[test]
    fn duplicate_outer_band_is_invalid() {
        let layout = UiLayout {
            outer_order: [OuterBand::Main, OuterBand::Main],
            ..UiLayout::default()
        };

        assert!(!layout.is_valid());
    }

    #[test]
    fn duplicate_two_column_pane_is_invalid() {
        let layout = UiLayout {
            library: LibraryLayout {
                two_column_order: [TwoColumnPane::Browse, TwoColumnPane::Browse],
            },
            ..UiLayout::default()
        };

        assert!(!layout.is_valid());
    }

    #[test]
    fn legacy_header_and_right_sidebar_strings_fail_to_deserialize() {
        let raw = r#"{
          "outer_order": ["header", "main"],
          "main_order": ["library_sidebar", "library_content", "right_sidebar"],
          "library": { "two_column_order": ["browse", "detail"] }
        }"#;

        assert!(serde_json::from_str::<UiLayout>(raw).is_err());
    }
}
