use anstyle::{AnsiColor, Color, Style};

pub(crate) const ERROR: Style =
    Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightRed))).bold();
pub(crate) const INFO: Style =
    Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightCyan))).bold();
pub(crate) const MUTED: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightBlack)));
pub(crate) const OK: Style =
    Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightGreen))).bold();
pub(crate) const WARN: Style =
    Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightYellow))).bold();

#[inline]
pub(crate) const fn should_print_progress(index: usize, total: usize) -> bool {
    total <= 10 || index == 1 || index == total || index.is_multiple_of(10)
}
