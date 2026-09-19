#![allow(dead_code)]

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const ITALIC: &str = "\x1b[3m";

// Base colors
pub const BLACK: &str = "\x1b[30m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const BLUE: &str = "\x1b[34m";
pub const MAGENTA: &str = "\x1b[35m";
pub const CYAN: &str = "\x1b[36m";
pub const WHITE: &str = "\x1b[37m";

// Bold colors
pub const B_BLACK: &str = "\x1b[1;30m";
pub const B_RED: &str = "\x1b[1;31m";
pub const B_GREEN: &str = "\x1b[1;32m";
pub const B_YELLOW: &str = "\x1b[1;33m";
pub const B_BLUE: &str = "\x1b[1;34m";
pub const B_MAGENTA: &str = "\x1b[1;35m";
pub const B_CYAN: &str = "\x1b[1;36m";
pub const B_WHITE: &str = "\x1b[1;37m";

// Italic Colors
pub const I_BLACK: &str = "\x1b[3;30m";
pub const I_RED: &str = "\x1b[3;31m";
pub const I_GREEN: &str = "\x1b[3;32m";
pub const I_YELLOW: &str = "\x1b[3;33m";
pub const I_BLUE: &str = "\x1b[3;34m";
pub const I_MAGENTA: &str = "\x1b[3;35m";
pub const I_CYAN: &str = "\x1b[3;36m";
pub const I_WHITE: &str = "\x1b[3;37m";

// Base Background Colors
pub const BG_BLACK: &str = "\x1b[40m";
pub const BG_RED: &str = "\x1b[41m";
pub const BG_GREEN: &str = "\x1b[42m";
pub const BG_YELLOW: &str = "\x1b[43m";
pub const BG_BLUE: &str = "\x1b[44m";
pub const BG_MAGENTA: &str = "\x1b[45m";
pub const BG_CYAN: &str = "\x1b[46m";
pub const BG_WHITE: &str = "\x1b[47m";

// Bold Background Colors
pub const BG_B_BLACK: &str = "\x1b[1;40m";
pub const BG_B_RED: &str = "\x1b[1;41m";
pub const BG_B_GREEN: &str = "\x1b[1;42m";
pub const BG_B_YELLOW: &str = "\x1b[1;43m";
pub const BG_B_BLUE: &str = "\x1b[1;44m";
pub const BG_B_MAGENTA: &str = "\x1b[1;45m";
pub const BG_B_CYAN: &str = "\x1b[1;46m";
pub const BG_B_WHITE: &str = "\x1b[1;47m";

// Italic Background Colors
pub const BG_I_BLACK: &str = "\x1b[3;40m";
pub const BG_I_RED: &str = "\x1b[3;41m";
pub const BG_I_GREEN: &str = "\x1b[3;42m";
pub const BG_I_YELLOW: &str = "\x1b[3;43m";
pub const BG_I_BLUE: &str = "\x1b[3;44m";
pub const BG_I_MAGENTA: &str = "\x1b[3;45m";
pub const BG_I_CYAN: &str = "\x1b[3;46m";
pub const BG_I_WHITE: &str = "\x1b[3;47m";
