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
    let extra = if marg % 2 == 1 && width % 2 == 1 { 1 } else { 0 };
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

    pub fn columns() -> Option<u32> {
        let mut ws = Winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
        let fd = std::io::stdout().as_raw_fd();
        let ret = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws) };
        if ret == 0 && ws.ws_col > 0 {
            Some(ws.ws_col as u32)
        } else {
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) mod term {
    pub fn columns() -> Option<u32> {
        None
    }
}

pub(crate) fn detect_width(explicit: Option<u32>) -> usize {
    let width = explicit
        .or_else(|| std::env::var("COLUMNS").ok().and_then(|v| v.parse::<u32>().ok()).filter(|&v| v > 0))
        .or_else(term::columns)
        .unwrap_or(96);
    width.clamp(60, 140) as usize
}
