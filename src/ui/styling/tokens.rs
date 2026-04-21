use std::collections::HashMap;

use gpui::{App, FontWeight, Global, Pixels, SharedString, px};
use tracing::warn;

use crate::settings::interface::UiDensity;

#[derive(Clone, Debug)]
pub struct Tokens {
    pub density: UiDensity,
    pub fonts: FontStacks,
    pub text: HashMap<&'static str, TextStyleSpec>,
    pub spacing: HashMap<&'static str, Pixels>,
    pub radius: HashMap<&'static str, Pixels>,
    pub border: HashMap<&'static str, Pixels>,
}

impl Global for Tokens {}

#[derive(Clone, Debug)]
pub struct FontStacks {
    pub body: SharedString,
    pub display: SharedString,
    pub mono: SharedString,
}

#[derive(Clone, Copy, Debug)]
pub struct TextStyleSpec {
    pub size: Pixels,
    pub line_height: Pixels,
    pub weight: FontWeight,
}

impl Default for Tokens {
    fn default() -> Self {
        Self::for_density(UiDensity::Default)
    }
}

impl Tokens {
    pub fn for_density(density: UiDensity) -> Self {
        let spacing = match density {
            UiDensity::Compact => [
                ("none", px(0.0)),
                ("xs", px(2.0)),
                ("sm", px(4.0)),
                ("md", px(6.0)),
                ("lg", px(10.0)),
                ("xl", px(14.0)),
                ("2xl", px(20.0)),
                ("3xl", px(28.0)),
            ],
            UiDensity::Default => [
                ("none", px(0.0)),
                ("xs", px(2.0)),
                ("sm", px(4.0)),
                ("md", px(8.0)),
                ("lg", px(12.0)),
                ("xl", px(16.0)),
                ("2xl", px(24.0)),
                ("3xl", px(32.0)),
            ],
            UiDensity::Comfortable => [
                ("none", px(0.0)),
                ("xs", px(3.0)),
                ("sm", px(6.0)),
                ("md", px(10.0)),
                ("lg", px(14.0)),
                ("xl", px(20.0)),
                ("2xl", px(28.0)),
                ("3xl", px(36.0)),
            ],
        }
        .into_iter()
        .collect();

        let radius = match density {
            UiDensity::Compact => [
                ("none", px(0.0)),
                ("sm", px(2.0)),
                ("md", px(5.0)),
                ("lg", px(8.0)),
                ("full", px(9999.0)),
            ],
            UiDensity::Default => [
                ("none", px(0.0)),
                ("sm", px(3.0)),
                ("md", px(6.0)),
                ("lg", px(10.0)),
                ("full", px(9999.0)),
            ],
            UiDensity::Comfortable => [
                ("none", px(0.0)),
                ("sm", px(4.0)),
                ("md", px(8.0)),
                ("lg", px(12.0)),
                ("full", px(9999.0)),
            ],
        }
        .into_iter()
        .collect();

        let border = [("none", px(0.0)), ("hairline", px(1.0)), ("thick", px(2.0))]
            .into_iter()
            .collect();

        let text = match density {
            UiDensity::Compact => [
                (
                    "xs",
                    TextStyleSpec {
                        size: px(10.0),
                        line_height: px(13.0),
                        weight: FontWeight::NORMAL,
                    },
                ),
                (
                    "sm",
                    TextStyleSpec {
                        size: px(12.0),
                        line_height: px(16.0),
                        weight: FontWeight::NORMAL,
                    },
                ),
                (
                    "md",
                    TextStyleSpec {
                        size: px(13.0),
                        line_height: px(18.0),
                        weight: FontWeight::NORMAL,
                    },
                ),
                (
                    "lg",
                    TextStyleSpec {
                        size: px(15.0),
                        line_height: px(20.0),
                        weight: FontWeight::MEDIUM,
                    },
                ),
                (
                    "xl",
                    TextStyleSpec {
                        size: px(18.0),
                        line_height: px(24.0),
                        weight: FontWeight::MEDIUM,
                    },
                ),
                (
                    "2xl",
                    TextStyleSpec {
                        size: px(22.0),
                        line_height: px(28.0),
                        weight: FontWeight::BOLD,
                    },
                ),
            ],
            UiDensity::Default => [
                (
                    "xs",
                    TextStyleSpec {
                        size: px(11.0),
                        line_height: px(14.0),
                        weight: FontWeight::NORMAL,
                    },
                ),
                (
                    "sm",
                    TextStyleSpec {
                        size: px(13.0),
                        line_height: px(18.0),
                        weight: FontWeight::NORMAL,
                    },
                ),
                (
                    "md",
                    TextStyleSpec {
                        size: px(14.0),
                        line_height: px(20.0),
                        weight: FontWeight::NORMAL,
                    },
                ),
                (
                    "lg",
                    TextStyleSpec {
                        size: px(16.0),
                        line_height: px(22.0),
                        weight: FontWeight::MEDIUM,
                    },
                ),
                (
                    "xl",
                    TextStyleSpec {
                        size: px(20.0),
                        line_height: px(26.0),
                        weight: FontWeight::MEDIUM,
                    },
                ),
                (
                    "2xl",
                    TextStyleSpec {
                        size: px(24.0),
                        line_height: px(30.0),
                        weight: FontWeight::BOLD,
                    },
                ),
            ],
            UiDensity::Comfortable => [
                (
                    "xs",
                    TextStyleSpec {
                        size: px(12.0),
                        line_height: px(16.0),
                        weight: FontWeight::NORMAL,
                    },
                ),
                (
                    "sm",
                    TextStyleSpec {
                        size: px(14.0),
                        line_height: px(20.0),
                        weight: FontWeight::NORMAL,
                    },
                ),
                (
                    "md",
                    TextStyleSpec {
                        size: px(15.0),
                        line_height: px(22.0),
                        weight: FontWeight::NORMAL,
                    },
                ),
                (
                    "lg",
                    TextStyleSpec {
                        size: px(18.0),
                        line_height: px(24.0),
                        weight: FontWeight::MEDIUM,
                    },
                ),
                (
                    "xl",
                    TextStyleSpec {
                        size: px(22.0),
                        line_height: px(30.0),
                        weight: FontWeight::MEDIUM,
                    },
                ),
                (
                    "2xl",
                    TextStyleSpec {
                        size: px(28.0),
                        line_height: px(34.0),
                        weight: FontWeight::BOLD,
                    },
                ),
            ],
        }
        .into_iter()
        .collect();

        Self {
            density,
            fonts: FontStacks {
                body: SharedString::new_static("Inter"),
                display: SharedString::new_static("Inter"),
                mono: SharedString::new_static("JetBrains Mono"),
            },
            text,
            spacing,
            radius,
            border,
        }
    }

    pub fn space(&self, name: &str) -> Pixels {
        self.spacing.get(name).copied().unwrap_or_else(|| {
            warn!("unknown spacing token: {name}");
            px(0.0)
        })
    }

    pub fn radius(&self, name: &str) -> Pixels {
        self.radius.get(name).copied().unwrap_or_else(|| {
            warn!("unknown radius token: {name}");
            px(0.0)
        })
    }

    pub fn border_width(&self, name: &str) -> Pixels {
        self.border.get(name).copied().unwrap_or_else(|| {
            warn!("unknown border token: {name}");
            px(0.0)
        })
    }

    pub fn text_style(&self, name: &str) -> TextStyleSpec {
        self.text.get(name).copied().unwrap_or_else(|| {
            warn!("unknown text token: {name}");
            self.text["md"]
        })
    }

    pub fn choose_px(&self, compact: f32, default: f32, comfortable: f32) -> Pixels {
        px(match self.density {
            UiDensity::Compact => compact,
            UiDensity::Default => default,
            UiDensity::Comfortable => comfortable,
        })
    }

    pub fn choose_f32(&self, compact: f32, default: f32, comfortable: f32) -> f32 {
        match self.density {
            UiDensity::Compact => compact,
            UiDensity::Default => default,
            UiDensity::Comfortable => comfortable,
        }
    }
}

pub trait ActiveTokens {
    fn tokens(&self) -> &Tokens;
}

impl ActiveTokens for App {
    fn tokens(&self) -> &Tokens {
        self.global::<Tokens>()
    }
}
