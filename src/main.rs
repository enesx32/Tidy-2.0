//! tidy — a small Lua-scriptable TUI text editor.
//!
//! Controls:
//!   EDITOR mode (default): type to insert text, Enter for a new line,
//!     Backspace to delete/merge lines, arrow keys to move,
//!     Ctrl+S to save, Ctrl+Q to quit, Esc to drop into COMMAND mode.
//!   COMMAND mode: i = insert, a = append (move right, then insert),
//!     t = integrated terminal, s = save, q = quit, arrow keys to move.
//!   Unsaved-changes prompt: q = quit without saving, s = save & quit,
//!     Esc = cancel.
//!
//! A toolbar pinned to the top row always spells out the keys available
//! in the current mode, so the shortcuts don't have to be memorized.
//!
//! Lua extensions live at `extentions/main.lua` and can set the globals
//! `char` / `color` (via `change_placeholder` / `change_placeholder_color`)
//! and call `tidy_save()` / `tidy_get_cursor()`.

#[path = "../util/colors.rs"]
mod colors;

use crossterm::event::{poll, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use mlua::prelude::*;
use std::cell::RefCell;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

const GUTTER: usize = 4;
const TOOLBAR_ROWS: usize = 1;
const STATUS_ROWS: usize = 1;

const COLOR_KEYWORD: &str = "\x1B[38;2;198;120;221m";
const COLOR_TYPE: &str = "\x1B[38;2;86;182;194m";
const COLOR_LITERAL: &str = "\x1B[38;2;209;154;102m";
const COLOR_STRING: &str = "\x1B[38;2;152;195;121m";
const COLOR_COMMENT: &str = "\x1B[38;2;92;99;112m";
const COLOR_NUMBER: &str = "\x1B[38;2;209;154;102m";
const COLOR_DEFAULT: &str = "\x1B[38;2;171;178;191m";
const RESET_CODE: &str = "\x1B[0m";

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Edit,
    Command,
    Terminal,
    PromptFilename,
    ConfirmQuit,
    View,
}

struct Editor {
    lines: Vec<Vec<char>>,
    cursor_x: usize,
    cursor_y: usize,
    scroll_y: usize,
    mode: Mode,
    filename: String,
    input_buffer: String,
    dirty: bool,
    read_only: bool,
    placeholder: char,
    placeholder_color: String,
    term_cols: usize,
    term_rows: usize,
    terminal: Option<TerminalState>,
}

impl Editor {
    fn new(
        term_cols: usize,
        term_rows: usize,
        placeholder: char,
        placeholder_color: String,
        read_only: bool,
    ) -> Self {
        Editor {
            lines: vec![Vec::new()],
            cursor_x: 0,
            cursor_y: 0,
            scroll_y: 0,
            mode: if read_only { Mode::View } else { Mode::Edit },
            filename: String::new(),
            input_buffer: String::new(),
            dirty: false,
            read_only,
            placeholder,
            placeholder_color,
            term_cols,
            term_rows,
            terminal: None,
        }
    }

    fn edit_rows(&self) -> usize {
        self.term_rows
            .saturating_sub(TOOLBAR_ROWS + STATUS_ROWS)
    }

    fn current_line_len(&self) -> usize {
        self.lines
            .get(self.cursor_y)
            .map(|l| l.len())
            .unwrap_or(0)
    }

    fn clamp_cursor_x(&mut self) {
        let len = self.current_line_len();

        if self.cursor_x > len {
            self.cursor_x = len;
        }
    }

    fn ensure_visible(&mut self) {
        let rows = self.edit_rows().max(1);

        if self.cursor_y < self.scroll_y {
            self.scroll_y = self.cursor_y;
        } else if self.cursor_y >= self.scroll_y + rows {
            self.scroll_y = self.cursor_y + 1 - rows;
        }
    }

    fn insert_char(&mut self, c: char) {
        if self.read_only {
            return;
        }

        let max_text_cols = self.term_cols.saturating_sub(GUTTER);

        if self.cursor_x >= max_text_cols {
            return;
        }

        let line = &mut self.lines[self.cursor_y];

        if self.cursor_x > line.len() {
            self.cursor_x = line.len();
        }

        line.insert(self.cursor_x, c);
        self.cursor_x += 1;
        self.dirty = true;
    }

    fn insert_newline(&mut self) {
        if self.read_only {
            return;
        }

        let line = &mut self.lines[self.cursor_y];
        let at = self.cursor_x.min(line.len());
        let rest = line.split_off(at);

        self.lines.insert(self.cursor_y + 1, rest);
        self.cursor_y += 1;
        self.cursor_x = 0;
        self.dirty = true;
        self.ensure_visible();
    }

    fn backspace(&mut self) {
        if self.read_only {
            return;
        }

        if self.cursor_x > 0 {
            self.lines[self.cursor_y].remove(self.cursor_x - 1);
            self.cursor_x -= 1;
            self.dirty = true;
        } else if self.cursor_y > 0 {
            let current = self.lines.remove(self.cursor_y);

            self.cursor_y -= 1;

            let prev_len = self.lines[self.cursor_y].len();
            self.lines[self.cursor_y].extend(current);

            self.cursor_x = prev_len;
            self.dirty = true;
            self.ensure_visible();
        }
    }

    fn move_left(&mut self) {
        if self.cursor_x > 0 {
            self.cursor_x -= 1;
        } else if self.cursor_y > 0 {
            self.cursor_y -= 1;
            self.cursor_x = self.current_line_len();
            self.ensure_visible();
        }
    }

    fn move_right(&mut self) {
        let len = self.current_line_len();

        if self.cursor_x < len {
            self.cursor_x += 1;
        } else if self.cursor_y + 1 < self.lines.len() {
            self.cursor_y += 1;
            self.cursor_x = 0;
            self.ensure_visible();
        }
    }

    fn move_up(&mut self) {
        if self.cursor_y > 0 {
            self.cursor_y -= 1;
            self.clamp_cursor_x();
            self.ensure_visible();
        }
    }

    fn move_down(&mut self) {
        if self.cursor_y + 1 < self.lines.len() {
            self.cursor_y += 1;
            self.clamp_cursor_x();
            self.ensure_visible();
        }
    }

    fn line_text(&self, y: usize) -> String {
        self.lines
            .get(y)
            .map(|l| l.iter().collect())
            .unwrap_or_default()
    }

    fn serialize(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }
}

fn move_cursor(x: usize, y: usize) {
    print!("\x1B[{};{}H", y + 1, x + 1);
    io::stdout().flush().unwrap();
}

fn classify_word(word: &str) -> &'static str {
    match word {
        "fn"
        | "let"
        | "mut"
        | "struct"
        | "enum"
        | "pub"
        | "use"
        | "mod"
        | "match"
        | "if"
        | "else"
        | "return"
        | "def"
        | "class"
        | "import"
        | "from"
        | "for"
        | "in"
        | "while"
        | "loop"
        | "break"
        | "continue"
        | "impl"
        | "trait"
        | "as"
        | "const"
        | "static"
        | "async"
        | "await"
        | "unsafe" => COLOR_KEYWORD,

        "u8"
        | "u16"
        | "u32"
        | "u64"
        | "usize"
        | "i8"
        | "i16"
        | "i32"
        | "i64"
        | "isize"
        | "f32"
        | "f64"
        | "bool"
        | "char"
        | "String"
        | "str"
        | "Vec"
        | "Option"
        | "Result"
        | "int"
        | "float"
        | "self"
        | "Self" => COLOR_TYPE,

        "true" | "false" | "None" | "Some" | "Ok" | "Err" | "null" | "nil" => {
            COLOR_LITERAL
        }

        _ => COLOR_DEFAULT,
    }
}

fn highlight_line(text: &str, bg: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();

    let mut out = String::new();
    let mut i = 0;

    while i < len {
        let c = chars[i];

        if (c == '/' && i + 1 < len && chars[i + 1] == '/') || c == '#' {
            let rest: String = chars[i..].iter().collect();

            out.push_str(bg);
            out.push_str(COLOR_COMMENT);
            out.push_str(&rest);
            out.push_str(RESET_CODE);

            break;
        }

        if c == '"' || c == '\'' {
            let quote = c;
            let start = i;

            i += 1;

            while i < len && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < len {
                    i += 1;
                }

                i += 1;
            }

            if i < len {
                i += 1;
            }

            let s: String = chars[start..i].iter().collect();

            out.push_str(bg);
            out.push_str(COLOR_STRING);
            out.push_str(&s);
            out.push_str(RESET_CODE);

            continue;
        }

        if c.is_ascii_digit() {
            let start = i;

            while i < len
                && (chars[i].is_ascii_alphanumeric()
                    || chars[i] == '.'
                    || chars[i] == '_')
            {
                i += 1;
            }

            let s: String = chars[start..i].iter().collect();

            out.push_str(bg);
            out.push_str(COLOR_NUMBER);
            out.push_str(&s);
            out.push_str(RESET_CODE);

            continue;
        }

        if c.is_alphabetic() || c == '_' {
            let start = i;

            while i < len
                && (chars[i].is_alphanumeric() || chars[i] == '_')
            {
                i += 1;
            }

            let word: String = chars[start..i].iter().collect();

            out.push_str(bg);
            out.push_str(classify_word(&word));
            out.push_str(&word);
            out.push_str(RESET_CODE);

            continue;
        }

        out.push_str(bg);
        out.push(c);
        out.push_str(RESET_CODE);

        i += 1;
    }

    out
}

fn render_toolbar(ed: &Editor) {
    print!("\x1B[1;1H\x1B[2K");

    let hints: &str = match ed.mode {
        Mode::Edit => {
            "  ^S Save    ^Q Quit    Esc Command Mode  "
        }

        Mode::Command => {
            "  [i] Insert    [a] Append    [t] Terminal    [s] Save    [q] Quit   [ ↑↓←→ ] Move  "
        }

        Mode::Terminal => {
            "  Integrated Terminal    type 'exit' to return to Tidy  "
        }

        Mode::PromptFilename => {
            "  [Enter] Confirm    [Esc] Cancel  "
        }

        Mode::ConfirmQuit => {
            "  [q] Quit Without Saving    [s]Save & Quit    [Esc] Cancel  "
        }

        Mode::View => {
            "  [q] Quit    [ ↑↓←→ ] Scroll    Read-only  "
        }
    };

    let cols = ed.term_cols;
    let pad_len = cols.saturating_sub(hints.len());

    print!(
        "\x1B[48;2;33;37;43m\x1B[38;2;209;213;219m{}{:pad_len$}\x1B[0m",
        hints,
        "",
        pad_len = pad_len
    );

    io::stdout().flush().unwrap();
}

fn render_chrome(ed: &Editor) {
    render_toolbar(ed);
    render_status_bar(ed);
}

fn render_status_bar(ed: &Editor) {
    let rows = ed.term_rows;
    let cols = ed.term_cols;

    print!("\x1B[{};1H\x1B[2K", rows);

    if ed.mode == Mode::PromptFilename {
        let prompt = format!(" Save File As: {}", ed.input_buffer);
        let pad_len = cols.saturating_sub(prompt.len() + 1);

        print!(
            "\x1B[48;2;40;44;52m\x1B[38;2;229;192;123m{}{:pad_len$}\x1B[0m",
            prompt,
            "",
            pad_len = pad_len
        );

        io::stdout().flush().unwrap();
        return;
    }

    if ed.mode == Mode::ConfirmQuit {
        let prompt =
            " Unsaved changes!  [q] quit without saving   [s] save & quit   [Esc] cancel ";

        let pad_len = cols.saturating_sub(prompt.len() + 1);

        print!(
            "\x1B[48;2;224;108;117m\x1B[38;2;40;44;52m{}{:pad_len$}\x1B[0m",
            prompt,
            "",
            pad_len = pad_len
        );

        io::stdout().flush().unwrap();
        return;
    }

    let mode_block = match ed.mode {
        Mode::Edit => {
            "\x1B[48;2;152;195;121m\x1B[38;2;40;44;52m EDITOR \x1B[0m"
        }

        Mode::Command => {
            "\x1B[48;2;97;175;239m\x1B[38;2;40;44;52m COMMAND \x1B[0m"
        }

        Mode::Terminal => {
            "\x1B[48;2;97;175;239m\x1B[38;2;40;44;52m TERMINAL \x1B[0m"
        }

        Mode::View => {
            "\x1B[48;2;229;192;123m\x1B[38;2;40;44;52m VIEW \x1B[0m"
        }

        _ => "",
    };

    let file_str = if ed.filename.is_empty() {
        "[New File]"
    } else {
        &ed.filename
    };

    let dirty_marker = if ed.dirty { " [+]" } else { "" };

    let right_info =
        format!("Ln {}, Col {} ", ed.cursor_y + 1, ed.cursor_x + 1);

    let left_len = 9 + file_str.len() + dirty_marker.len();

    let pad_len =
        cols.saturating_sub(left_len + right_info.len() + 1);

    print!(
        "\x1B[48;2;40;44;52m\x1B[38;2;171;178;191m"
    );

    print!(
        "{}  {}{}{:pad_len$}{}\x1B[0m",
        mode_block,
        file_str,
        dirty_marker,
        "",
        right_info,
        pad_len = pad_len
    );

    io::stdout().flush().unwrap();
}

const CURRENT_LINE_BG: &str = "\x1B[48;2;44;49;58m";
const GUTTER_FG: &str = "\x1B[38;2;92;99;112m";
const GUTTER_FG_ACTIVE: &str = "\x1B[38;2;229;192;123m";

fn render_line(ed: &Editor, y: usize) {
    let screen_row = TOOLBAR_ROWS + (y - ed.scroll_y);

    print!("\x1B[{};1H\x1B[2K", screen_row + 1);

    let is_current = y == ed.cursor_y;

    let line_bg = if is_current {
        CURRENT_LINE_BG
    } else {
        ""
    };

    let gutter_fg = if is_current {
        GUTTER_FG_ACTIVE
    } else {
        GUTTER_FG
    };

    if y < ed.lines.len() {
        print!(
            "{}{}{:>3} \x1B[0m",
            line_bg,
            gutter_fg,
            y + 1
        );

        let text = ed.line_text(y);

        print!("{}", highlight_line(&text, line_bg));

        if !line_bg.is_empty() {
            let max_text_cols =
                ed.term_cols.saturating_sub(GUTTER);

            let pad =
                max_text_cols.saturating_sub(text.chars().count());

            if pad > 0 {
                print!(
                    "{}{:pad$}\x1B[0m",
                    line_bg,
                    "",
                    pad = pad
                );
            }
        }
    } else {
        print!(
            "  {}{}\x1B[0m",
            ed.placeholder_color,
            ed.placeholder
        );
    }

    io::stdout().flush().unwrap();
}

fn render_all(ed: &Editor) {
    print!("\x1B[2J");

    let rows = ed.edit_rows();

    for i in 0..rows {
        render_line(ed, ed.scroll_y + i);
    }

    render_chrome(ed);
}

fn save_file(ed: &mut Editor) {
    if ed.read_only {
        return;
    }

    if ed.filename.is_empty() {
        ed.mode = Mode::PromptFilename;
        ed.input_buffer.clear();
        return;
    }

    if fs::write(&ed.filename, ed.serialize()).is_ok() {
        ed.dirty = false;
    }
}

const TERMINAL_PWD_MARKER: &str = "__TIDY_PWD__";
const TERMINAL_MAX_LINES: usize = 1000;

const TERM_FG: &str = "\x1B[38;2;214;219;229m";
const TERM_MUTED: &str = "\x1B[38;2;125;133;147m";
const TERM_CYAN: &str = "\x1B[38;2;97;175;239m";
const TERM_GREEN: &str = "\x1B[38;2;152;195;121m";
const TERM_YELLOW: &str = "\x1B[38;2;229;192;123m";
const TERM_RED: &str = "\x1B[38;2;224;108;117m";
const TERM_BAR_BG: &str = "\x1B[48;2;33;37;43m";

struct TerminalState {
    output: Vec<String>,
    current_output: String,
    input: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    scroll: usize,
    cwd: PathBuf,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    rx: Receiver<String>,
    tx: Sender<String>,
    session_started: bool,
}

impl TerminalState {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();

        let cwd = env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."));

        let mut terminal = TerminalState {
            output: Vec::new(),
            current_output: String::new(),
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
            scroll: 0,
            cwd,
            child: None,
            stdin: None,
            rx,
            tx,
            session_started: false,
        };

        terminal.start_shell();
        terminal
    }

    fn start_shell(&mut self) {
        if self.child.is_some() {
            return;
        }

        let script = r#"
$ErrorActionPreference = "Continue"
function __tidy_prompt {
    [Console]::Out.WriteLine("__TIDY_PWD__" + (Get-Location).Path)
}
__tidy_prompt
while ($true) {
    $line = [Console]::In.ReadLine()
    if ($null -eq $line) {
        break
    }

    try {
        Invoke-Expression $line 2>&1 | Out-String -Stream | ForEach-Object { [Console]::Out.WriteLine($_) }
    }
    catch {
        $_ | Out-String | Write-Error
    }

    __tidy_prompt
}
"#;

        #[cfg(target_os = "windows")]
        let mut command = {
            use std::os::windows::process::CommandExt;

            let mut command = Command::new("powershell.exe");
            command.creation_flags(0x08000000);
            command.args([
                "-NoLogo",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                script,
            ]);
            command
        };

        #[cfg(not(target_os = "windows"))]
        let mut command = {
            let mut command = Command::new("pwsh");
            command.args([
                "-NoLogo",
                "-NoProfile",
                "-Command",
                script,
            ]);
            command
        };

        let spawned = command
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                self.push_system_line(&format!(
                    "Could not start PowerShell: {}",
                    error
                ));
                self.session_started = true;
                return;
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();

        if let Some(stdout) = stdout {
            spawn_terminal_reader(stdout, self.tx.clone());
        }

        if let Some(stderr) = stderr {
            spawn_terminal_reader(stderr, self.tx.clone());
        }

        self.stdin = stdin;
        self.child = Some(child);
        self.session_started = true;

        self.push_system_line("PowerShell session started.");
    }

    fn stop_shell(&mut self) {
        self.session_started = false;
        self.stdin.take();

        if let Some(mut child) = self.child.take() {
            let still_running = child
                .try_wait()
                .ok()
                .flatten()
                .is_none();

            if still_running {
                #[cfg(target_os = "windows")]
                {
                    let pid = child.id().to_string();

                    let _ = Command::new("taskkill")
                        .args(["/PID", &pid, "/T", "/F"])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }

                #[cfg(not(target_os = "windows"))]
                {
                    let _ = child.kill();
                }
            }

            let _ = child.wait();
        }
    }

    fn restart_shell(&mut self) {
        self.stop_shell();
        self.push_system_line("Restarting PowerShell...");
        self.start_shell();
    }

    fn send_command(&mut self, command: &str) {
        let result = match self.stdin.as_mut() {
            Some(stdin) => {
                stdin
                    .write_all(command.as_bytes())
                    .and_then(|_| stdin.write_all(b"\n"))
                    .and_then(|_| stdin.flush())
            }
            None => {
                self.push_system_line("PowerShell is not running.");
                return;
            }
        };

        if let Err(error) = result {
            self.push_system_line(&format!(
                "Terminal input error: {}",
                error
            ));
        }
    }

    fn submit_input(&mut self) {
        let command = self.input.clone();

        if command.trim().is_empty() {
            self.output.push(String::new());
            self.scroll = 0;
            self.input.clear();
            self.cursor = 0;
            self.history_index = None;
            return;
        }

        self.output.push(format!("❯ {}", command));

        if self.output.len() > TERMINAL_MAX_LINES {
            let remove_count = self.output.len() - TERMINAL_MAX_LINES;
            self.output.drain(0..remove_count);
        }

        if self.history.last() != Some(&command) {
            self.history.push(command.clone());
        }

        self.send_command(&command);

        self.input.clear();
        self.cursor = 0;
        self.history_index = None;
        self.scroll = 0;
    }

    fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += 1;
        self.scroll = 0;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.input.remove(self.cursor);
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.input.len() {
            self.input.remove(self.cursor);
        }
    }

    fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }

        let new_index = match self.history_index {
            None => self.history.len() - 1,
            Some(index) => index.saturating_sub(1),
        };

        self.history_index = Some(new_index);
        self.input = self.history[new_index].clone();
        self.cursor = self.input.len();
    }

    fn history_down(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };

        if index + 1 >= self.history.len() {
            self.history_index = None;
            self.input.clear();
            self.cursor = 0;
            return;
        }

        let new_index = index + 1;

        self.history_index = Some(new_index);
        self.input = self.history[new_index].clone();
        self.cursor = self.input.len();
    }

    fn push_system_line(&mut self, line: &str) {
        self.output.push(format!("• {}", line));

        if self.output.len() > TERMINAL_MAX_LINES {
            let remove_count = self.output.len() - TERMINAL_MAX_LINES;
            self.output.drain(0..remove_count);
        }

        self.scroll = 0;
    }

    fn push_output_chunk(&mut self, chunk: &str) {
        for c in chunk.chars() {
            match c {
                '\n' => self.finish_output_line(),
                '\r' => {} // Ignore \r so Windows CRLF line endings don't clear current_output
                '\u{0008}' => {
                    self.current_output.pop();
                }
                '\0' => {}
                c if c.is_control() => {}
                c => self.current_output.push(c),
            }
        }

        self.scroll = 0;
    }

    fn finish_output_line(&mut self) {
        let line = strip_ansi(&self.current_output);
        self.current_output.clear();

        if let Some(path) = line.strip_prefix(TERMINAL_PWD_MARKER) {
            if !path.is_empty() {
                self.cwd = PathBuf::from(path);
            }
            return;
        }

        self.output.push(line);

        if self.output.len() > TERMINAL_MAX_LINES {
            let remove_count = self.output.len() - TERMINAL_MAX_LINES;
            self.output.drain(0..remove_count);
        }
    }

    fn total_display_lines(&self) -> usize {
        self.output.len()
            + usize::from(!self.current_output.is_empty())
    }

    fn scroll_up(&mut self) {
        let max_scroll = self
            .total_display_lines()
            .saturating_sub(self.output_height());

        self.scroll = (self.scroll + 3).min(max_scroll);
    }

    fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_sub(3);
    }

    fn output_height(&self) -> usize {
        let (_, rows) = terminal::size().unwrap_or((80, 24));
        (rows as usize).saturating_sub(3).max(1)
    }
}

impl Drop for TerminalState {
    fn drop(&mut self) {
        self.stop_shell();
    }
}

fn spawn_terminal_reader<R>(mut reader: R, tx: Sender<String>)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0u8; 4096];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let text =
                        String::from_utf8_lossy(&buffer[..count])
                            .into_owned();

                    if tx.send(text).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut escape = false;
    let mut csi = false;

    for c in text.chars() {
        if escape {
            if c == '[' {
                csi = true;
                escape = false;
            } else {
                escape = false;
            }
            continue;
        }

        if csi {
            if c.is_ascii_alphabetic() {
                csi = false;
            }
            continue;
        }

        if c == '\x1B' {
            escape = true;
            continue;
        }

        out.push(c);
    }

    out
}

fn drain_terminal_output(ed: &mut Editor) -> bool {
    let Some(term) = ed.terminal.as_mut() else {
        return false;
    };

    let mut changed = false;

    while let Ok(chunk) = term.rx.try_recv() {
        term.push_output_chunk(&chunk);
        changed = true;
    }

    let exit_status = match term.child.as_mut() {
        Some(child) => match child.try_wait() {
            Ok(status) => status,
            Err(_) => None,
        },
        None => None,
    };

    if let Some(status) = exit_status {
        term.child = None;
        term.stdin = None;

        let code = status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        term.push_system_line(&format!(
            "PowerShell exited with code {}.",
            code
        ));

        changed = true;
    }

    changed
}

fn terminal_line_color(line: &str) -> &'static str {
    let lower = line.to_ascii_lowercase();

    if lower.contains("error")
        || lower.contains("failed")
        || lower.contains("panic")
        || lower.contains("traceback")
    {
        TERM_RED
    } else if lower.contains("warning") {
        TERM_YELLOW
    } else if lower.contains("finished")
        || lower.contains("success")
        || lower.contains("ok")
    {
        TERM_GREEN
    } else if line.starts_with('•') {
        TERM_MUTED
    } else if line.starts_with('❯') {
        TERM_CYAN
    } else {
        TERM_FG
    }
}

fn truncate_text(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

fn terminal_working_dir(term: &TerminalState, width: usize) -> String {
    let path = term.cwd.to_string_lossy();

    if path.chars().count() <= width {
        return path.to_string();
    }

    if width <= 3 {
        return "...".chars().take(width).collect();
    }

    let tail: String = path
        .chars()
        .rev()
        .take(width - 3)
        .collect::<String>()
        .chars()
        .rev()
        .collect();

    format!("...{}", tail)
}

fn render_terminal(ed: &Editor) {
    let Some(term) = ed.terminal.as_ref() else {
        return;
    };

    let cols = ed.term_cols.max(20);
    let rows = ed.term_rows.max(5);

    print!("\x1B[2J\x1B[H");

    let cwd_width = cols.saturating_sub(45).max(8);
    let cwd = terminal_working_dir(term, cwd_width);

    let title = format!(
        "  ●  TIDY TERMINAL   •   PowerShell   •   {}",
        cwd
    );

    let title = truncate_text(&title, cols);

    print!(
        "{}{}{}{}{:pad$}\x1B[0m",
        TERM_BAR_BG,
        TERM_CYAN,
        title,
        "",
        "",
        pad = cols.saturating_sub(title.chars().count())
    );

    let output_height = rows.saturating_sub(3).max(1);
    let total = term.total_display_lines();

    let start = total.saturating_sub(
        output_height + term.scroll
    );

    for row in 0..output_height {
        let screen_row = row + 2;

        print!("\x1B[{};1H\x1B[2K", screen_row);

        let index = start + row;

        if index >= total {
            print!("{} ", TERM_MUTED);
            print!("{:pad$}", "", pad = cols.saturating_sub(1));
            continue;
        }

        let line = if index < term.output.len() {
            term.output[index].as_str()
        } else {
            term.current_output.as_str()
        };

        let max_width = cols.saturating_sub(3);
        let text = truncate_text(line, max_width);

        print!(
            "{}│ {}{:pad$}\x1B[0m",
            TERM_MUTED,
            terminal_line_color(line),
            text,
            pad = max_width.saturating_sub(text.chars().count())
        );
    }

    let input_row = rows.saturating_sub(1);

    print!("\x1B[{};1H\x1B[2K", input_row);

    let prompt = "  ❯ ";
    let available = cols.saturating_sub(prompt.chars().count() + 1);
    let input = truncate_text(&term.input, available);

    print!(
        "{}{}{}{}{}",
        TERM_BAR_BG,
        TERM_CYAN,
        prompt,
        TERM_FG,
        input
    );

    let input_used =
        prompt.chars().count() + input.chars().count();

    print!(
        "{:pad$}\x1B[0m",
        "",
        pad = cols.saturating_sub(input_used)
    );

    print!("\x1B[{};1H\x1B[2K", rows);

    let status = if term.scroll > 0 {
        "  [Enter] Run   [↑↓] History   [PgUp/PgDn] Scroll   [Ctrl+C] Stop   [Esc] Close"
    } else {
        "  [Enter] Run   [↑↓] History   [Ctrl+C] Stop   [Esc] Close"
    };

    let status = truncate_text(status, cols);

    print!(
        "{}{}{:pad$}\x1B[0m",
        TERM_BAR_BG,
        TERM_MUTED,
        status,
        pad = cols.saturating_sub(status.chars().count())
    );

    io::stdout().flush().unwrap();
}

fn terminal_cursor_position(ed: &Editor) -> (usize, usize) {
    let term = ed.terminal.as_ref();

    let cursor = term.map(|term| term.cursor).unwrap_or(0);

    let x = 4 + cursor;
    let y = ed.term_rows.saturating_sub(2);

    (x, y)
}

fn load_file(ed: &mut Editor, filename: &str) {
    ed.filename = filename.to_string();

    if let Ok(content) = fs::read_to_string(filename) {
        let max_cols = ed.term_cols.saturating_sub(GUTTER);

        ed.lines = content
            .lines()
            .map(|l| l.chars().take(max_cols).collect())
            .collect();

        if ed.lines.is_empty() {
            ed.lines.push(Vec::new());
        }

        ed.dirty = false;
    }
}

const EXTENSIONS_FILE: &str = "extentions/main.lua";

struct LaunchConfig {
    file: Option<String>,
    read_only: bool,
    show_help: bool,
}

fn parse_args(args: &[String]) -> LaunchConfig {
    let mut flags: Vec<String> = Vec::new();
    let mut positional: Vec<String> = Vec::new();

    for arg in args.iter().skip(1) {
        if let Some(name) = arg.strip_prefix('-') {
            let name = name.trim_start_matches('-').to_lowercase();

            if !name.is_empty() {
                flags.push(name);
            }
        } else {
            positional.push(arg.clone());
        }
    }

    let has = |name: &str| flags.iter().any(|f| f == name);

    let show_help = has("help") || has("h");
    let wants_extensions = has("extentions") || has("extensions");

    let file = if wants_extensions {
        Some(EXTENSIONS_FILE.to_string())
    } else {
        positional.into_iter().next()
    };

    let read_only = if wants_extensions {
        !has("edit")
    } else {
        has("view")
    };

    LaunchConfig {
        file,
        read_only,
        show_help,
    }
}

fn print_help() {
    println!("tidy — a small Lua-scriptable TUI text editor\n");

    println!("USAGE:");
    println!("    tidy [FILE] [OPTIONS]");
    println!(
        "    cargo run -- [FILE] [OPTIONS]   (note the -- before your own args)\n"
    );

    println!("OPTIONS:");
    println!(
        "    --edit          Open FILE for editing (default when FILE is given)"
    );
    println!("    --view          Open FILE read-only");
    println!(
        "    --extentions    Open the Lua extensions file instead of FILE"
    );
    println!(
        "                    (read-only unless --edit is also given)"
    );
    println!("    --help, -h      Show this message\n");

    println!("EXAMPLES:");
    println!("    cargo run -- main.rs --edit");
    println!("    cargo run -- --extentions --view");
    println!("    cargo run -- --extentions --edit");
}

fn register_lua_hooks(
    lua: &Lua,
    editor: Rc<RefCell<Editor>>,
) {
    let globals = lua.globals();

    let ed_save = editor.clone();

    let save_fn = lua
        .create_function(move |_, (): ()| {
            save_file(&mut ed_save.borrow_mut());
            Ok(())
        })
        .unwrap();

    let ed_cursor = editor.clone();

    let get_cursor_fn = lua
        .create_function(move |_, (): ()| {
            let ed = ed_cursor.borrow();
            Ok((ed.cursor_x, ed.cursor_y))
        })
        .unwrap();

    globals.set("tidy_save", save_fn).unwrap();
    globals
        .set("tidy_get_cursor", get_cursor_fn)
        .unwrap();
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let config = parse_args(&args);

    if config.show_help {
        print_help();
        return;
    }

    let lua = Lua::new();
    let globals = lua.globals();

    globals.set("RESET", colors::RESET).unwrap();
    globals.set("B_CYAN", colors::B_CYAN).unwrap();
    globals.set("BG_BLACK", colors::BG_BLACK).unwrap();
    globals.set("WHITE", colors::WHITE).unwrap();

    let change_placeholder = lua
        .create_function(|_, ch: Option<String>| {
            Ok(ch.unwrap_or_else(|| "#".to_string()))
        })
        .unwrap();

    let change_placeholder_color = lua
        .create_function(|_, color: Option<String>| {
            Ok(color.unwrap_or_else(|| {
                colors::B_CYAN.to_string()
            }))
        })
        .unwrap();

    globals
        .set("change_placeholder", change_placeholder)
        .unwrap();

    globals
        .set(
            "change_placeholder_color",
            change_placeholder_color,
        )
        .unwrap();

    let lua_code =
        fs::read_to_string(EXTENSIONS_FILE).unwrap_or_default();

    let _ = lua.load(&lua_code).exec();

    let placeholder_str: String = globals
        .get("char")
        .unwrap_or_else(|_| "#".to_string());

    let color_code: String = globals
        .get("color")
        .unwrap_or_else(|_| colors::B_CYAN.to_string());

    let placeholder =
        placeholder_str.chars().next().unwrap_or('#');

    let (cols, rows) =
        terminal::size().unwrap_or((80, 24));

    let editor = Rc::new(RefCell::new(Editor::new(
        cols as usize,
        rows as usize,
        placeholder,
        color_code,
        config.read_only,
    )));

    register_lua_hooks(&lua, editor.clone());

    if let Some(path) = &config.file {
        load_file(
            &mut editor.borrow_mut(),
            path,
        );
    }

    terminal::enable_raw_mode().unwrap();

    print!(
        "\x1B[?1049h\x1B[?25l\x1B[?7l\x1B[2J\x1B[H"
    );

    render_all(&editor.borrow());

    print!("\x1B[?25h");

    {
        let ed = editor.borrow();

        move_cursor(
            GUTTER + ed.cursor_x,
            TOOLBAR_ROWS + ed.cursor_y - ed.scroll_y,
        );
    }

    'main_loop: loop {
        let terminal_mode =
            editor.borrow().mode == Mode::Terminal;

        if terminal_mode {
            let mut ed = editor.borrow_mut();

            if ed.terminal.is_none() {
                ed.terminal = Some(TerminalState::new());
            } else if let Some(term) = ed.terminal.as_mut() {
                if !term.session_started {
                    term.start_shell();
                }
            }

            if drain_terminal_output(&mut ed) {
                render_terminal(&ed);

                let (x, y) =
                    terminal_cursor_position(&ed);

                drop(ed);

                move_cursor(x, y);
            } else {
                drop(ed);
            }

            if !poll(Duration::from_millis(16)).unwrap() {
                continue;
            }
        }

        if let Ok(Event::Key(key_event)) =
            crossterm::event::read()
        {
            if key_event.kind != KeyEventKind::Press {
                continue;
            }

            let mut ed = editor.borrow_mut();

            let old_scroll = ed.scroll_y;
            let old_line_count = ed.lines.len();
            let old_cursor_y = ed.cursor_y;
            let mode = ed.mode;

            match mode {
                Mode::Terminal => {
                    let control =
                        key_event.modifiers.contains(KeyModifiers::CONTROL);

                    match key_event.code {
                        KeyCode::Esc => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.stop_shell();
                            }

                            ed.mode = Mode::Command;
                            render_all(&ed);
                        }

                        KeyCode::Char('q')
                            if control =>
                        {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.stop_shell();
                            }

                            ed.mode = Mode::Command;
                            render_all(&ed);
                        }

                        KeyCode::Char('c')
                            if control =>
                        {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.push_system_line(
                                    "^C  Process stopped.",
                                );
                                term.restart_shell();
                            }
                        }

                        KeyCode::Char('l')
                            if control =>
                        {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.output.clear();
                                term.current_output.clear();
                                term.scroll = 0;
                            }
                        }

                        KeyCode::Char('u')
                            if control =>
                        {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.input.clear();
                                term.cursor = 0;
                            }
                        }

                        KeyCode::Enter => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.submit_input();
                            }
                        }

                        KeyCode::Backspace => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.backspace();
                            }
                        }

                        KeyCode::Delete => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.delete();
                            }
                        }

                        KeyCode::Left => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.cursor =
                                    term.cursor.saturating_sub(1);
                            }
                        }

                        KeyCode::Right => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.cursor =
                                    (term.cursor + 1).min(term.input.len());
                            }
                        }

                        KeyCode::Home => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.cursor = 0;
                            }
                        }

                        KeyCode::End => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.cursor =
                                    term.input.len();
                            }
                        }

                        KeyCode::Up => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.history_up();
                            }
                        }

                        KeyCode::Down => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.history_down();
                            }
                        }

                        KeyCode::PageUp => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.scroll_up();
                            }
                        }

                        KeyCode::PageDown => {
                            if let Some(term) =
                                ed.terminal.as_mut()
                            {
                                term.scroll_down();
                            }
                        }

                        KeyCode::Char(c) => {
                            if !control {
                                if let Some(term) =
                                    ed.terminal.as_mut()
                                {
                                    term.insert_char(c);
                                }
                            }
                        }

                        _ => {}
                    }
                }

                Mode::PromptFilename => match key_event.code {
                    KeyCode::Enter => {
                        if !ed.input_buffer.is_empty() {
                            ed.filename =
                                ed.input_buffer.clone();

                            ed.mode = Mode::Edit;

                            save_file(&mut ed);
                        } else {
                            ed.mode = Mode::Edit;
                        }

                        render_chrome(&ed);
                    }

                    KeyCode::Esc => {
                        ed.mode = Mode::Edit;
                        render_chrome(&ed);
                    }

                    KeyCode::Backspace => {
                        ed.input_buffer.pop();
                        render_status_bar(&ed);
                    }

                    KeyCode::Char(c) => {
                        ed.input_buffer.push(c);
                        render_status_bar(&ed);
                    }

                    _ => {}
                },

                Mode::ConfirmQuit => match key_event.code {
                    KeyCode::Char('q')
                    | KeyCode::Char('Q') => {
                        drop(ed);
                        break 'main_loop;
                    }

                    KeyCode::Char('s')
                    | KeyCode::Char('S') => {
                        save_file(&mut ed);
                        drop(ed);
                        break 'main_loop;
                    }

                    KeyCode::Esc => {
                        ed.mode = Mode::Command;
                        render_chrome(&ed);
                    }

                    _ => {}
                },

                Mode::Edit => match key_event.code {
                    KeyCode::Esc => {
                        ed.mode = Mode::Command;
                        render_chrome(&ed);
                    }

                    KeyCode::Enter => {
                        ed.insert_newline();
                    }

                    KeyCode::Left => {
                        ed.move_left();
                    }

                    KeyCode::Right => {
                        ed.move_right();
                    }

                    KeyCode::Up => {
                        ed.move_up();
                    }

                    KeyCode::Down => {
                        ed.move_down();
                    }

                    KeyCode::Backspace => {
                        ed.backspace();
                    }

                    KeyCode::Tab => {
                        for _ in 0..4 {
                            ed.insert_char(' ');
                        }
                    }

                    KeyCode::Char('s')
                        if key_event
                            .modifiers
                            .contains(KeyModifiers::CONTROL) =>
                    {
                        save_file(&mut ed);
                    }

                    KeyCode::Char('q')
                        if key_event
                            .modifiers
                            .contains(KeyModifiers::CONTROL) =>
                    {
                        if ed.dirty {
                            ed.mode = Mode::ConfirmQuit;
                            render_chrome(&ed);
                        } else {
                            drop(ed);
                            break 'main_loop;
                        }
                    }

                    KeyCode::Char(c) => {
                        ed.insert_char(c);
                    }

                    _ => {}
                },

                Mode::Command => match key_event.code {
                    KeyCode::Char('i')
                    | KeyCode::Char('I') => {
                        ed.mode = Mode::Edit;
                    }

                    KeyCode::Char('a')
                    | KeyCode::Char('A') => {
                        ed.move_right();
                        ed.mode = Mode::Edit;
                    }

                    KeyCode::Char('t')
                    | KeyCode::Char('T') => {
                        if ed.terminal.is_none() {
                            ed.terminal = Some(TerminalState::new());
                        } else if let Some(term) = ed.terminal.as_mut() {
                            if !term.session_started {
                                term.start_shell();
                            }
                        }

                        ed.mode = Mode::Terminal;
                    }

                    KeyCode::Char('s')
                    | KeyCode::Char('S') => {
                        save_file(&mut ed);
                    }

                    KeyCode::Char('q')
                    | KeyCode::Char('Q') => {
                        if ed.dirty {
                            ed.mode = Mode::ConfirmQuit;
                        } else {
                            drop(ed);
                            break 'main_loop;
                        }
                    }

                    KeyCode::Left => {
                        ed.move_left();
                    }

                    KeyCode::Right => {
                        ed.move_right();
                    }

                    KeyCode::Up => {
                        ed.move_up();
                    }

                    KeyCode::Down => {
                        ed.move_down();
                    }

                    _ => {}
                },

                Mode::View => match key_event.code {
                    KeyCode::Esc
                    | KeyCode::Char('q')
                    | KeyCode::Char('Q') => {
                        drop(ed);
                        break 'main_loop;
                    }

                    KeyCode::Left => {
                        ed.move_left();
                    }

                    KeyCode::Right => {
                        ed.move_right();
                    }

                    KeyCode::Up => {
                        ed.move_up();
                    }

                    KeyCode::Down => {
                        ed.move_down();
                    }

                    _ => {}
                },
            }

            match ed.mode {
                Mode::Terminal => {
                    render_terminal(&ed);

                    let (x, y) =
                        terminal_cursor_position(&ed);

                    drop(ed);

                    move_cursor(x, y);
                }

                Mode::PromptFilename => {
                    let x =
                        15 + ed.input_buffer.len();

                    let y = ed.term_rows - 1;

                    drop(ed);

                    move_cursor(x, y);
                }

                Mode::ConfirmQuit => {
                    render_status_bar(&ed);
                }

                _ => {
                    let structural_change =
                        ed.lines.len() != old_line_count;

                    if ed.scroll_y != old_scroll
                        || structural_change
                    {
                        render_all(&ed);
                    } else {
                        if ed.cursor_y != old_cursor_y {
                            render_line(
                                &ed,
                                old_cursor_y,
                            );
                        }

                        render_line(
                            &ed,
                            ed.cursor_y,
                        );

                        render_chrome(&ed);
                    }

                    let (cx, cy, sy) =
                        (ed.cursor_x, ed.cursor_y, ed.scroll_y);

                    drop(ed);

                    move_cursor(
                        GUTTER + cx,
                        TOOLBAR_ROWS + cy - sy,
                    );
                }
            }
        }
    }

    if let Ok(mut ed) = editor.try_borrow_mut() {
        if let Some(term) = ed.terminal.as_mut() {
            term.stop_shell();
        }
    }

    print!("\x1B[?7h\x1B[?1049l");

    io::stdout().flush().unwrap();

    terminal::disable_raw_mode().unwrap();
}