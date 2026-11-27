//! One tracing setup for every binary.
//!
//! Filter with `VICTORY_LOG` (falls back to `RUST_LOG`), e.g.
//! `VICTORY_LOG=victory_loader=trace,info`. Debug and trace output exist
//! only in builds with the `diagnostics` feature; a plain release build
//! logs info and above whatever the filter asks for.
//!
//! On a terminal the lines name their module without the crates' common
//! `victory_` (`gpu::host` for `victory_gpu::host`); redirected, and in the
//! log file [`init_with_file`] writes, they keep the full path.

use std::io::IsTerminal;
use std::path::Path;

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

/// The most verbose level this build can log.
const CAP: LevelFilter = if cfg!(feature = "diagnostics") {
    LevelFilter::TRACE
} else {
    LevelFilter::INFO
};

pub fn init(default_level: &str) {
    init_with_file(default_level, None);
}

/// As [`init`], and every line also goes to `file` (created anew, without
/// colours, modules by their full path) when it can be made.
pub fn init_with_file(default_level: &str, file: Option<&Path>) {
    let filter = std::env::var("VICTORY_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .and_then(|v| EnvFilter::try_new(v).ok())
        .unwrap_or_else(|| EnvFilter::new(default_level));
    let short = std::io::stderr().is_terminal();
    let file = file.and_then(|path| {
        path.parent().map(std::fs::create_dir_all);
        std::fs::File::create(path).ok()
    });
    let _ = tracing_subscriber::registry()
        .with(CAP)
        .with(filter)
        .with(short.then(|| {
            tracing_subscriber::fmt::layer()
                .event_format(Short)
                .with_writer(std::io::stderr)
        }))
        .with((!short).then(|| {
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_writer(std::io::stderr)
        }))
        .with(file.map(|file| {
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
        }))
        .try_init();
}

/// A module path as the console shows it: without the crates' `victory_`.
pub fn short_target(target: &str) -> &str {
    target.strip_prefix("victory_").unwrap_or(target)
}

/// The default line (time, level, spans, module, fields), the module named
/// by [`short_target`].
struct Short;

impl<S, N> FormatEvent<S, N> for Short
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> std::fmt::Result {
        let ansi = writer.has_ansi_escapes();
        let (dim, reset) = if ansi { ("\x1b[2m", "\x1b[0m") } else { ("", "") };
        write!(writer, "{dim}")?;
        SystemTime.format_time(&mut writer)?;
        let level = *event.metadata().level();
        let colour = match level {
            Level::TRACE => "\x1b[35m",
            Level::DEBUG => "\x1b[34m",
            Level::INFO => "\x1b[32m",
            Level::WARN => "\x1b[33m",
            Level::ERROR => "\x1b[31m",
        };
        if ansi {
            write!(writer, "{reset} {colour}{level:>5}{reset} ")?;
        } else {
            write!(writer, " {level:>5} ")?;
        }
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                write!(writer, "{}", span.name())?;
                let ext = span.extensions();
                if let Some(fields) = ext.get::<tracing_subscriber::fmt::FormattedFields<N>>()
                    && !fields.is_empty()
                {
                    write!(writer, "{{{fields}}}")?;
                }
                write!(writer, ":")?;
            }
            write!(writer, " ")?;
        }
        write!(writer, "{dim}{}:{reset} ", short_target(event.metadata().target()))?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn modules_lose_the_common_prefix() {
        assert_eq!(super::short_target("victory_gpu::host"), "gpu::host");
        assert_eq!(super::short_target("wgpu::thing"), "wgpu::thing");
    }
}
