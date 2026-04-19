//! Panic handler routing panics through tracing.

/// Install a panic hook that routes panics through
/// `tracing::error!` with thread info. Replaces the default
/// "thread 'xxx' panicked at …" so background rayon threads don't
/// die silently.
pub(crate) fn install() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        let location = info.location().map_or_else(
            || "<unknown>".into(),
            |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
        );
        tracing::error!(
            thread = name,
            location = %location,
            payload = ?info.payload(),
            "panic"
        );
        // WHY: delegate to the default hook after logging so that
        // the user still sees the standard backtrace on the tty.
        default_hook(info);
    }));
}
