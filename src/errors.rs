use cntp_i18n::I18nString;

pub trait EmittableError {
    /// Retrieve the user-friendly string for this error. **Must be an `I18nString`.**
    fn friendly(&self) -> Option<I18nString> {
        None
    }

    /// Log the error. You must implement this.
    fn log(&self);
}

macro_rules! define_error {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }, None, $log:expr) => {
        #[derive(Debug)]
        struct $name {
            $($field: $ty,)*
        }

        impl EmittableError for $name {
            fn log(&self) {
                let Self { $($field),* } = self;

                tracing::error!($log)
            }
        }
    };
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }, $friendly:expr, $log:expr) => {
        #[derive(Debug)]
        struct $name {
            $($field: $ty,)*
        }

        impl EmittableError for $name {
            fn log(&self) {
                let Self { $($field),* } = self;

                tracing::error!($log)
            }

            fn friendly(&self) -> Option<I18nString> {
                Some($friendly)
            }
        }
    };
}

pub fn emit_error(err: impl EmittableError) {
    err.log();

    // TODO: send error toasts
}
