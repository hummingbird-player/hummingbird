use super::schema::{MainRegion, OuterBand, ShellLayout};

pub fn default_shell_layout() -> ShellLayout {
    ShellLayout::default()
}

pub fn stage_shell_layout() -> ShellLayout {
    ShellLayout {
        outer_order: [OuterBand::Header, OuterBand::Controls, OuterBand::Main],
        main_order: [
            MainRegion::LibrarySidebar,
            MainRegion::LibraryContent,
            MainRegion::RightSidebar,
        ],
    }
}
