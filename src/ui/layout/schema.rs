use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OuterBand {
    Header,
    Main,
    Controls,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MainRegion {
    LibrarySidebar,
    LibraryContent,
    RightSidebar,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ShellLayout {
    pub outer_order: [OuterBand; 3],
    pub main_order: [MainRegion; 3],
}

impl Default for ShellLayout {
    fn default() -> Self {
        Self {
            outer_order: [OuterBand::Header, OuterBand::Main, OuterBand::Controls],
            main_order: [
                MainRegion::LibrarySidebar,
                MainRegion::LibraryContent,
                MainRegion::RightSidebar,
            ],
        }
    }
}

impl ShellLayout {
    pub fn is_valid(&self) -> bool {
        is_outer_permutation(self.outer_order) && is_main_permutation(self.main_order)
    }

    pub fn validated(self) -> Option<Self> {
        self.is_valid().then_some(self)
    }
}

fn is_outer_permutation(order: [OuterBand; 3]) -> bool {
    let mut seen_header = false;
    let mut seen_main = false;
    let mut seen_controls = false;

    for band in order {
        match band {
            OuterBand::Header if !seen_header => seen_header = true,
            OuterBand::Main if !seen_main => seen_main = true,
            OuterBand::Controls if !seen_controls => seen_controls = true,
            _ => return false,
        }
    }

    seen_header && seen_main && seen_controls
}

fn is_main_permutation(order: [MainRegion; 3]) -> bool {
    let mut seen_sidebar = false;
    let mut seen_content = false;
    let mut seen_right_sidebar = false;

    for region in order {
        match region {
            MainRegion::LibrarySidebar if !seen_sidebar => seen_sidebar = true,
            MainRegion::LibraryContent if !seen_content => seen_content = true,
            MainRegion::RightSidebar if !seen_right_sidebar => seen_right_sidebar = true,
            _ => return false,
        }
    }

    seen_sidebar && seen_content && seen_right_sidebar
}
