//! A tiny logging facade that is structured under the `otel` feature and a
//! plain stderr `eprintln!` otherwise.
//!
//! The macros [`log_info!`](crate::log_info), [`log_warn!`](crate::log_warn)
//! and [`log_error!`](crate::log_error) share one call grammar:
//!
//! ```ignore
//! log_warn!("message literal", field = expr, other = expr);
//! ```
//!
//! - With `--features otel` they forward to `tracing::{info,warn,error}!`, so
//!   the event reaches the stderr fmt layer *and* the OpenTelemetry log bridge
//!   (which ships it to Loki, tagged with the active span's trace context). All
//!   field values are recorded via [`Display`](std::fmt::Display).
//! - Without the feature they expand to an `eprintln!` carrying the same
//!   `borzoi:` prefix as the code they replaced, with fields flattened
//!   as ` key=value`. No subscriber, no telemetry dependencies — the default
//!   build's behaviour is unchanged.
//!
//! This keeps a single call site per log point: the transport is chosen at
//! compile time, so there is no duplication and the default build never loses
//! its diagnostics. Field values must implement `Display` in both builds; for a
//! `Debug`-only value, format it to a `String` at the call site.

/// Internal: render a log line as a `String` in the non-`otel` build. Split out
/// from the `eprintln!` so the `concat!`/`stringify!` field-flattening can be
/// unit-tested. Not part of the public API.
#[cfg(not(feature = "otel"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __lsp_log_line {
    ($msg:literal $(, $k:ident = $v:expr)* $(,)?) => {
        ::std::format!(
            ::core::concat!("borzoi: ", $msg $(, " ", ::core::stringify!($k), "={}")*),
            $($v),*
        )
    };
}

/// Internal: the level-dispatching core. The public `log_*` macros delegate
/// here. Not part of the public API.
#[cfg(feature = "otel")]
#[doc(hidden)]
#[macro_export]
macro_rules! __lsp_log {
    ($lvl:ident, $msg:literal $(, $k:ident = $v:expr)* $(,)?) => {
        ::tracing::$lvl!($($k = %$v,)* $msg)
    };
}

/// Internal: the level-dispatching core. In the non-`otel` build the level is
/// ignored — every line goes to stderr, as it did before. Not part of the
/// public API.
#[cfg(not(feature = "otel"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __lsp_log {
    ($lvl:ident, $($t:tt)*) => {
        ::std::eprintln!("{}", $crate::__lsp_log_line!($($t)*))
    };
}

/// Emit an info-level structured log. See the [module docs](self).
#[macro_export]
macro_rules! log_info {
    ($($t:tt)*) => { $crate::__lsp_log!(info, $($t)*) };
}

/// Emit a warn-level structured log. See the [module docs](self).
#[macro_export]
macro_rules! log_warn {
    ($($t:tt)*) => { $crate::__lsp_log!(warn, $($t)*) };
}

/// Emit an error-level structured log. See the [module docs](self).
#[macro_export]
macro_rules! log_error {
    ($($t:tt)*) => { $crate::__lsp_log!(error, $($t)*) };
}

#[cfg(all(test, not(feature = "otel")))]
mod tests {
    #[test]
    fn message_only_keeps_the_prefix() {
        assert_eq!(
            crate::__lsp_log_line!("starting on stdio"),
            "borzoi: starting on stdio"
        );
    }

    #[test]
    fn fields_are_flattened_in_order() {
        let path = std::path::Path::new("/tmp/App.fsproj");
        assert_eq!(
            crate::__lsp_log_line!("discovery failed", project = path.display(), error = "boom"),
            "borzoi: discovery failed project=/tmp/App.fsproj error=boom"
        );
    }

    #[test]
    fn trailing_comma_is_accepted() {
        assert_eq!(crate::__lsp_log_line!("x", a = 1,), "borzoi: x a=1");
    }
}
