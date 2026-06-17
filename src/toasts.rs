use std::{sync::OnceLock, time::Duration};

use cntp_i18n::I18nString;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

static TOAST_SENDER: OnceLock<UnboundedSender<Toast>> = OnceLock::new();

const DEFAULT_DURATION: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Success,
    Warning,
    Error,
}

pub struct ToastAction {
    pub label: I18nString,
    pub callback: Box<dyn FnOnce(&mut gpui::App) + Send + 'static>,
}

pub struct Toast {
    pub severity: Severity,
    pub message: I18nString,
    // we don't actually change this ever but if we need to it's just better to leave it like this
    pub duration: Option<Duration>,
    pub actions: Vec<ToastAction>,
}

impl Toast {
    pub fn new(severity: Severity, message: I18nString) -> Self {
        Self {
            severity,
            message,
            duration: Some(DEFAULT_DURATION),
            actions: Vec::new(),
        }
    }

    pub fn info(message: I18nString) -> Self {
        Self::new(Severity::Info, message)
    }

    pub fn success(message: I18nString) -> Self {
        Self::new(Severity::Success, message)
    }

    pub fn warning(message: I18nString) -> Self {
        Self::new(Severity::Warning, message)
    }

    pub fn error(message: I18nString) -> Self {
        Self::new(Severity::Error, message)
    }

    pub fn with_action(
        mut self,
        label: I18nString,
        callback: impl FnOnce(&mut gpui::App) + Send + 'static,
    ) -> Self {
        self.actions.push(ToastAction {
            label,
            callback: Box::new(callback),
        });
        self
    }
}

/// Queue a toast for display. Silently dropped if the sender hasn't been
/// installed yet (e.g. before `init` is called during very early startup).
pub fn emit_toast(toast: Toast) {
    if let Some(sender) = TOAST_SENDER.get() {
        let _ = sender.send(toast);
    }
}

pub fn init() -> UnboundedReceiver<Toast> {
    let (tx, rx) = unbounded_channel();
    TOAST_SENDER
        .set(tx)
        .expect("toasts::init called more than once");
    rx
}
