use log::{Level, LevelFilter, Metadata, Record, SetLoggerError};

struct SimpleLogger;

fn level2color(level: Level) -> u8 {
    match level {
        Level::Error => 31, // 31 Red
        Level::Warn => 93,  // 93 BrightYellow
        _ => 0,
        // Level::Info => 34,   // 34 Blue
        // Level::Debug => 32,  // 32 Green
        // Level::Trace => 90,  // 90 BrightBlack
    }
}

macro_rules! with_color {
    ($color: expr, $($arg:tt)*) => {
        format_args!("\u{1B}[{}m{}\u{1B}[0m", $color as u8, format_args!($($arg)*))
    };
}

impl log::Log for SimpleLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let time = crate::kernel::timer::now();
            let sec = time.as_secs();
            let ms = time.subsec_millis();

            let level = match record.level() {
                Level::Error => "E>",
                Level::Warn => "W>",
                Level::Info => "I>",
                Level::Debug => "D>",
                Level::Trace => "T>",
            };
            println!(
                "{}",
                with_color!(
                    level2color(record.level()),
                    "[{sec:04}.{ms:03}]{}{}[{}:{}] {}",
                    level,
                    crate::kernel::current_cpu().id,
                    record.file().unwrap_or("<unnamed>"),
                    record.line().unwrap_or(0),
                    record.args()
                )
            );
        }
    }

    fn flush(&self) {}
}

static LOGGER: SimpleLogger = SimpleLogger;

pub fn logger_init() -> Result<(), SetLoggerError> {
    log::set_logger(&LOGGER).map(|()| log::set_max_level(LevelFilter::Trace))
}
