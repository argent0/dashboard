// Console styling and terminal-size helpers shared by every renderer.

// --------------------------------------------------------------------------
// Styling
// --------------------------------------------------------------------------

pub(crate) mod c {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const GREEN: &str = "\x1b[32m";
    pub const CYAN: &str = "\x1b[36m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const RED: &str = "\x1b[31m";
    pub const GRAY: &str = "\x1b[90m";
    pub const MAGENTA: &str = "\x1b[35m";
    pub const BLUE: &str = "\x1b[34m";
    pub const WHITE: &str = "\x1b[97m";
}

pub(crate) fn style(text: &str, codes: &[&str], enabled: bool) -> String {
    if !enabled {
        return text.to_string();
    }
    format!("{}{}{}", codes.join(""), text, c::RESET)
}

pub(crate) fn bar(fraction: f64, width: usize, enabled: bool) -> String {
    let filled = (fraction * width as f64).round();
    let filled = filled.max(0.0).min(width as f64) as usize;
    let s = "█".repeat(filled) + &"░".repeat(width - filled);
    if !enabled {
        return s;
    }
    let color = if fraction >= 1.0 {
        c::GREEN
    } else if fraction > 0.0 {
        c::CYAN
    } else {
        c::GRAY
    };
    format!("{color}{s}{}", c::RESET)
}

pub(crate) fn truncate(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    if width <= 1 {
        return chars.into_iter().take(width).collect();
    }
    let mut out: String = chars.into_iter().take(width - 1).collect();
    out.push('…');
    out
}

pub(crate) fn center(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.to_string();
    }
    let marg = width - len;
    let extra = if marg % 2 == 1 && width % 2 == 1 {
        1
    } else {
        0
    };
    let left = marg / 2 + extra;
    let right = marg - left;
    format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
}

// --------------------------------------------------------------------------
// Terminal size
// --------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub(crate) mod term {
    use std::os::unix::io::AsRawFd;

    #[repr(C)]
    struct Winsize {
        ws_row: u16,
        ws_col: u16,
        ws_xpixel: u16,
        ws_ypixel: u16,
    }

    const TIOCGWINSZ: u64 = 0x5413;

    unsafe extern "C" {
        fn ioctl(fd: i32, request: u64, argp: *mut Winsize) -> i32;
    }

    fn winsize() -> Option<Winsize> {
        let mut ws = Winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let fd = std::io::stdout().as_raw_fd();
        let ret = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws) };
        (ret == 0).then_some(ws)
    }

    pub fn columns() -> Option<u32> {
        winsize().map(|ws| ws.ws_col as u32).filter(|&c| c > 0)
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) mod term {
    pub fn columns() -> Option<u32> {
        None
    }
}

pub fn detect_width(explicit: Option<u32>) -> usize {
    let width = explicit
        .or_else(|| {
            std::env::var("COLUMNS")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|&v| v > 0)
        })
        .or_else(term::columns)
        .unwrap_or(96);
    width.clamp(60, 140) as usize
}


// --------------------------------------------------------------------------
// Scrolling (`dashboard watch`)
// --------------------------------------------------------------------------

/// A scroll position over a frame taller than the terminal. It survives
/// redraws: the offset is clamped when the content shrinks, and a view the
/// user scrolled to the very end stays pinned there as the content grows.
#[derive(Debug, Default)]
pub struct Viewport {
    offset: usize,
    at_end: bool,
}

/// A scroll request from a key press.
#[derive(Debug, Clone, Copy)]
pub enum Scroll {
    Lines(isize),
    Pages(isize),
    Top,
    Bottom,
}

impl Viewport {
    /// Rows left for content: one goes to the footer when it doesn't fit.
    fn body_rows(lines: usize, rows: usize) -> usize {
        if lines > rows { rows.saturating_sub(1).max(1) } else { rows }
    }

    fn max_offset(lines: usize, rows: usize) -> usize {
        lines.saturating_sub(Self::body_rows(lines, rows))
    }

    /// Moves by a key press, given the current content and terminal height.
    pub fn scroll(&mut self, by: Scroll, lines: usize, rows: usize) {
        let page = Self::body_rows(lines, rows) as isize;
        let max = Self::max_offset(lines, rows);
        let target = match by {
            Scroll::Lines(n) => self.offset as isize + n,
            Scroll::Pages(n) => self.offset as isize + n * page,
            Scroll::Top => 0,
            Scroll::Bottom => max as isize,
        };
        self.offset = target.clamp(0, max as isize) as usize;
        self.at_end = max > 0 && self.offset == max;
    }

    /// The visible slice of `lines` plus, when they don't fit in `rows`, a
    /// footer saying where the view is and which keys scroll it.
    pub fn frame<'a>(
        &mut self,
        lines: &'a [&'a str],
        rows: usize,
        color: bool,
    ) -> (&'a [&'a str], Option<String>) {
        let max = Self::max_offset(lines.len(), rows);
        self.offset = if self.at_end { max } else { self.offset.min(max) };
        if max == 0 {
            self.at_end = false;
            return (&lines[..lines.len().min(rows)], None);
        }
        let end = (self.offset + Self::body_rows(lines.len(), rows)).min(lines.len());
        let footer = format!(
            "── {}–{} of {} · ↑↓ j/k  PgUp/PgDn  g/G  q quit ──",
            self.offset + 1,
            end,
            lines.len()
        );
        let footer = if color {
            format!("{}{}{}", c::DIM, footer, c::RESET)
        } else {
            footer
        };
        (&lines[self.offset..end], Some(footer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("l{i}")).collect()
    }

    fn view<'a>(vp: &mut Viewport, lines: &'a [&'a str], rows: usize) -> (Vec<&'a str>, Option<String>) {
        let (body, footer) = vp.frame(lines, rows, false);
        (body.to_vec(), footer)
    }

    #[test]
    fn content_that_fits_has_no_footer_and_ignores_scrolling() {
        let owned = numbered(3);
        let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let mut vp = Viewport::default();
        vp.scroll(Scroll::Pages(1), lines.len(), 10);
        assert_eq!(view(&mut vp, &lines, 10), (vec!["l1", "l2", "l3"], None));
    }

    #[test]
    fn overflow_reserves_a_footer_row_and_clamps_scrolling() {
        let owned = numbered(10);
        let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let mut vp = Viewport::default();
        let (body, footer) = view(&mut vp, &lines, 4);
        assert_eq!(body, vec!["l1", "l2", "l3"]);
        assert!(footer.unwrap().contains("1–3 of 10"));

        vp.scroll(Scroll::Pages(1), lines.len(), 4);
        assert_eq!(view(&mut vp, &lines, 4).0, vec!["l4", "l5", "l6"]);
        vp.scroll(Scroll::Lines(100), lines.len(), 4);
        assert_eq!(view(&mut vp, &lines, 4).0, vec!["l8", "l9", "l10"]);
        vp.scroll(Scroll::Lines(-100), lines.len(), 4);
        assert_eq!(view(&mut vp, &lines, 4).0, vec!["l1", "l2", "l3"]);
    }

    #[test]
    fn the_end_stays_pinned_as_content_grows_and_offsets_clamp_as_it_shrinks() {
        let owned = numbered(10);
        let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let mut vp = Viewport::default();
        vp.scroll(Scroll::Bottom, lines.len(), 4);
        let grown = numbered(12);
        let grown: Vec<&str> = grown.iter().map(String::as_str).collect();
        assert_eq!(view(&mut vp, &grown, 4).0, vec!["l10", "l11", "l12"]);

        vp.scroll(Scroll::Lines(-1), grown.len(), 4);
        assert_eq!(view(&mut vp, &grown, 4).0, vec!["l9", "l10", "l11"]);
        let shrunk = numbered(6);
        let shrunk: Vec<&str> = shrunk.iter().map(String::as_str).collect();
        assert_eq!(view(&mut vp, &shrunk, 4).0, vec!["l4", "l5", "l6"]);
    }

    #[test]
    fn content_that_once_fit_does_not_jump_to_the_end_when_it_grows() {
        let owned = numbered(3);
        let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let mut vp = Viewport::default();
        vp.scroll(Scroll::Bottom, lines.len(), 4);
        let grown = numbered(10);
        let grown: Vec<&str> = grown.iter().map(String::as_str).collect();
        assert_eq!(view(&mut vp, &grown, 4).0, vec!["l1", "l2", "l3"]);
    }
}
