//! Display-only snapshots. A row clones its label when created or notified;
//! rendering never reads settings, SQLite, credentials, or a remote backend.
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use cntp_i18n::tr;
use gpui::{App, AppContext, Context, Entity, SharedString, div, prelude::*, px};

use crate::{
    settings::SettingsGlobal,
    sources::{SourceId, sync::SourceHost},
    ui::{app::Pool, components::tooltip::build_tooltip, theme::Theme},
};

#[derive(Default)]
pub struct SourceLabels {
    retained: Vec<(SourceId, Option<String>)>,
    configured: BTreeMap<String, String>,
    display: HashMap<SourceId, SharedString>,
}

impl SourceLabels {
    fn rebuild(&mut self) {
        let mut names: BTreeMap<String, String> = self
            .retained
            .iter()
            .filter(|(id, _)| !id.is_local())
            .map(|(id, name)| {
                (
                    id.as_str().to_owned(),
                    name.clone().unwrap_or_else(|| {
                        // Old catalogs and references imported from another installation
                        // have no name. Keep the identity visible instead of guessing.
                        format!("{} · {}", tr!("SOURCE_UNAVAILABLE_LIBRARY"), id)
                    }),
                )
            })
            .collect();
        names.extend(self.configured.clone());
        let mut totals = HashMap::<String, usize>::new();
        for name in names.values() {
            *totals.entry(name.clone()).or_default() += 1;
        }
        let mut ordinals = HashMap::<String, usize>::new();
        self.display.clear();
        if !names.is_empty() {
            self.display.insert(
                SourceId::local(),
                tr!("SOURCE_LOCAL_LIBRARY", "Local files").into(),
            );
        }
        for (id, name) in names {
            let ordinal = ordinals.entry(name.clone()).or_default();
            *ordinal += 1;
            let mut label = if totals[&name] > 1 {
                // Keep the distinction visible when a long name is ellipsized.
                format!("{ordinal} · {name}")
            } else {
                name
            };
            if !self.configured.contains_key(&id) {
                label = format!("{label} ({})", tr!("SOURCE_REMOVED_LABEL", "removed"));
            }
            self.display.insert(SourceId::new(id), label.into());
        }
    }

    fn configure(&mut self, cx: &App) -> bool {
        let configured = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .services
            .libraries
            .iter()
            .filter(|config| !config.id.is_local())
            .map(|config| (config.id.as_str().to_owned(), config.name.clone()))
            .collect();
        if self.configured == configured {
            return false;
        }
        self.configured = configured;
        self.rebuild();
        true
    }

    fn label(&self, source: &SourceId) -> Option<SharedString> {
        self.display.get(source).cloned().or_else(|| {
            (!source.is_local()).then(|| {
                format!(
                    "{} · {}",
                    tr!("SOURCE_UNAVAILABLE_LIBRARY", "Unavailable library"),
                    source
                )
                .into()
            })
        })
    }
}

pub fn initialize(host: &Arc<SourceHost>, cx: &mut App) -> Entity<SourceLabels> {
    let labels = cx.new(|cx| {
        let mut labels = SourceLabels::default();
        labels.configure(cx);
        labels
    });
    let settings = cx.global::<SettingsGlobal>().model.clone();
    let changed_labels = labels.clone();
    cx.observe(&settings, move |_, cx| {
        changed_labels.update(cx, |labels, cx| {
            if labels.configure(cx) {
                cx.notify();
            }
        });
    })
    .detach();
    let pool = cx.global::<Pool>().0.clone();
    let mut changes = host.subscribe_labels();
    let loaded_labels = labels.clone();
    cx.spawn(async move |cx| {
        loop {
            changes.borrow_and_update();
            let pool = pool.clone();
            let result = crate::RUNTIME.spawn(async move {
                sqlx::query_as::<_, (SourceId, Option<String>)>("SELECT id,display_name FROM library_source WHERE id != 'local' ORDER BY id")
                    .fetch_all(&pool).await
            }).await;
            if let Ok(Ok(retained)) = result {
                loaded_labels.update(cx, |labels, cx| {
                    if labels.retained != retained {
                        labels.retained = retained;
                        labels.rebuild();
                        cx.notify();
                    }
                });
            }
            if changes.changed().await.is_err() { break; }
        }
    }).detach();
    labels
}

/// Call only at row creation or on a label snapshot change, never per frame.
pub fn label(source: &SourceId, cx: &App) -> Option<SharedString> {
    cx.try_global::<super::SourceModels>()
        .and_then(|models| models.labels.read(cx).label(source))
}

pub fn observe<T: 'static>(
    cx: &mut Context<T>,
    update: impl Fn(&mut T, &mut Context<T>) + 'static,
) {
    if let Some(models) = cx.try_global::<super::SourceModels>() {
        let labels = models.labels.clone();
        cx.observe(&labels, move |this, _, cx| {
            update(this, cx);
            cx.notify();
        })
        .detach();
    }
}

pub fn badge(label: SharedString, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id("source-label")
        .flex_shrink_0()
        .max_w(px(132.0))
        .px(px(5.0))
        .rounded(px(3.0))
        .text_xs()
        .font_weight(gpui::FontWeight::NORMAL)
        .text_color(theme.text_secondary)
        .bg(theme.elevated_background)
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .tooltip(build_tooltip(label.clone()))
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_distinguish_accounts_and_retained_catalogs_without_changing_local_only_views() {
        let a = SourceId::new("account-a");
        let b = SourceId::new("account-b");
        let mut labels = SourceLabels::default();
        labels.rebuild();
        assert!(labels.label(&SourceId::local()).is_none());
        labels.retained = vec![
            (b.clone(), Some("Music".into())),
            (a.clone(), Some("Old name".into())),
        ];
        labels.configured = [
            ("account-a".into(), "Music".into()),
            ("account-b".into(), "Music".into()),
        ]
        .into();
        labels.rebuild();
        assert_eq!(labels.label(&a).unwrap().as_ref(), "1 · Music");
        assert_eq!(labels.label(&b).unwrap().as_ref(), "2 · Music");
        assert!(labels.label(&SourceId::local()).is_some());
        let first = labels.label(&a).unwrap();
        labels.configured.remove("account-b");
        labels.rebuild();
        assert_eq!(labels.label(&a).unwrap().as_ref(), "1 · Music");
        let removed = labels.label(&b).unwrap();
        assert!(removed.starts_with("2 · Music ("));
        assert_ne!(removed, labels.label(&a).unwrap());
        // Old/missing catalogs never borrow a current account's name.
        let unknown = labels.label(&SourceId::new("purged-account")).unwrap();
        assert!(unknown.ends_with("purged-account"));
        assert_ne!(unknown, first);
        labels.retained.clear();
        labels.configured.clear();
        labels.rebuild();
        assert!(labels.label(&SourceId::local()).is_none());
    }
}
