use std::fmt;
use std::io::{self, IsTerminal};
use std::sync::Once;

use time::macros::format_description;
use time::OffsetDateTime;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::EnvFilter;

pub struct RuntimeLogging;

#[derive(Clone, Copy, Debug)]
struct TimestampedColorFormatter {
    ansi: bool,
}

impl TimestampedColorFormatter {
    const RESET: &'static str = "\x1b[0m";
    const ERROR: &'static str = "\x1b[31m";
    const WARN: &'static str = "\x1b[33m";
    const DEBUG: &'static str = "\x1b[90m";

    fn format_timestamp() -> String {
        let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        now.format(format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]"
        ))
        .unwrap_or_else(|_| "0000-00-00 00:00:00.000".to_string())
    }

    fn level_style(self, level: &Level) -> &'static str {
        match *level {
            Level::ERROR => Self::ERROR,
            Level::WARN => Self::WARN,
            Level::INFO => "",
            Level::DEBUG | Level::TRACE => Self::DEBUG,
        }
    }
}

impl<S, N> FormatEvent<S, N> for TimestampedColorFormatter
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let timestamp = Self::format_timestamp();
        let level = metadata.level().as_str();
        let style = self.level_style(metadata.level());

        write!(writer, "[{timestamp}] ")?;
        if self.ansi && !style.is_empty() {
            write!(writer, "{style}{level:<5}{}", Self::RESET)?;
        } else {
            write!(writer, "{level:<5}")?;
        }
        write!(writer, " ")?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

impl RuntimeLogging {
    pub fn init(verbose: bool) -> Option<tracing_appender::non_blocking::WorkerGuard> {
        static INIT: Once = Once::new();
        let mut guard_out = None;
        INIT.call_once(|| {
            let default_level = if verbose { "debug" } else { "warn" };
            let filter = EnvFilter::try_from_env("FORMATTER_LOG")
                .or_else(|_| EnvFilter::try_from_env("RUST_LOG"))
                .unwrap_or_else(|_| EnvFilter::new(default_level));
            let ansi = io::stderr().is_terminal();

            let (non_blocking, guard) = tracing_appender::non_blocking(io::stderr());
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .event_format(TimestampedColorFormatter { ansi })
                .with_writer(non_blocking)
                .try_init();
            guard_out = Some(guard);
        });
        guard_out
    }
}
