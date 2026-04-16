//! Shared formatting helpers for CLI output.

/// Format a byte count as a human-readable string.
///
/// WHY `#[allow(cast_precision_loss)]`: display formatting for human-readable
/// byte sizes inherently sacrifices precision for readability. A 1-decimal-
/// place GB figure that is off by a few bytes is correct enough for the UI.
#[allow(clippy::cast_precision_loss)]
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
    }

    #[test]
    fn format_size_kb() {
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(2048), "2.0 KB");
    }

    #[test]
    fn format_size_mb() {
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn format_size_gb() {
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
    }
}
