use std;
use std::collections::HashMap;
use std::vec::Vec;
use std::io;
use std::io::{Read, Write};
use std::sync::{Mutex, Arc, atomic};
use std::thread;

use anyhow::Error;
use console::{Term, style};

use crate::config::Config;
use crate::stack_trace::{StackTrace, Frame};
use crate::version::Version;

pub struct ConsoleViewer {
    #[allow(dead_code)]
    console_config: os_impl::ConsoleConfig,
    version: Option<Version>,
    command: String,
    sampling_rate: f64,
    running: Arc<atomic::AtomicBool>,
    options: Arc<Mutex<Options>>,
    stats: Stats,
    subprocesses: bool,
    config: Config
}

impl ConsoleViewer {
    pub fn new(show_linenumbers: bool,
               python_command: &str,
               version: &Option<Version>,
               config: &Config) -> io::Result<ConsoleViewer> {
        let sampling_rate = 1.0 / (config.sampling_rate as f64);
        let running = Arc::new(atomic::AtomicBool::new(true));
        let options = Arc::new(Mutex::new(Options::new(show_linenumbers)));

        // listen for keyboard events in a separate thread to avoid blocking here
        let input_running = running.clone();
        let input_options = options.clone();
        thread::spawn(move || {
            while input_running.load(atomic::Ordering::Relaxed) {
                // TODO: there isn't a non-blocking version of stdin, so this will capture the
                // next keystroke after the ConsoleViewer object has been destroyed =(
                if let Some(Ok(key)) = std::io::stdin().bytes().next() {
                    let mut options = input_options.lock().unwrap();
                    options.dirty = true;
                    let previous_usage = options.usage;
                    match key as char {
                        'R' | 'r' => options.reset = true,
                        'L' | 'l' => options.show_linenumbers = !options.show_linenumbers,
                        'X' | 'x' => options.usage = false,
                        '?' => options.usage = true,
                        '1' => options.sort_column = 1,
                        '2' => options.sort_column = 2,
                        '3' => options.sort_column = 3,
                        '4' => options.sort_column = 4,
                        _ => {},
                    }

                    options.reset_style = previous_usage != options.usage;
                }
            }
        });

        Ok(ConsoleViewer{console_config: os_impl::ConsoleConfig::new()?,
                         version: version.clone(),
                         command: python_command.to_owned(),
                         running, options, sampling_rate,
                         subprocesses: config.subprocesses,
                         stats: Stats::new(),
                         config: config.clone()})
    }

    pub fn increment(&mut self, traces: &[StackTrace]) -> Result<(), Error> {
        self.maybe_reset();
        self.stats.threads = 0;
        self.stats.processes = 0;
        let mut last_pid = None;
        for trace in traces {
            self.stats.threads += 1;
            if last_pid != Some(trace.pid) {
                self.stats.processes += 1;
                last_pid = Some(trace.pid);
            }

            if !(self.config.include_idle || trace.active) {
                continue;
            }

            if self.config.gil_only && !trace.owns_gil {
                continue;
            }

            if trace.owns_gil {
                self.stats.gil += 1
            }

            if trace.active {
                self.stats.active += 1
            }

            update_function_statistics(&mut self.stats.line_counts, trace, |frame| {
                let filename = match &frame.short_filename { Some(f) => &f, None => &frame.filename };
                if frame.line != 0 {
                    format!("{} ({}:{})", frame.name, filename, frame.line)
                } else {
                    format!("{} ({})", frame.name, filename)
                }
            });

            update_function_statistics(&mut self.stats.function_counts, trace, |frame| {
                let filename = match &frame.short_filename { Some(f) => &f, None => &frame.filename };
                format!("{} ({})", frame.name, filename)
            });
        }
        self.increment_common()?;
        Ok(())
    }

    pub fn display(&self) -> std::io::Result<()> {
        // Get the top aggregate function calls (either by line or by function as )
        let mut options = self.options.lock().unwrap();
        options.dirty = false;
        let counts = if options.show_linenumbers { &self.stats.line_counts } else { &self.stats.function_counts };
        let mut counts:Vec<(&FunctionStatistics, &str)> = counts.iter().map(|(x,y)| (y, x.as_ref())).collect();

        // TODO: subsort ?
        match options.sort_column {
            1 => counts.sort_unstable_by(|a, b| b.0.current_own.cmp(&a.0.current_own)),
            2 => counts.sort_unstable_by(|a, b| b.0.current_total.cmp(&a.0.current_total)),
            3 => counts.sort_unstable_by(|a, b| b.0.overall_own.cmp(&a.0.overall_own)),
            4 => counts.sort_unstable_by(|a, b| b.0.overall_total.cmp(&a.0.overall_total)),
            _ => panic!("unknown sort column. this really shouldn't happen")
        }
        let term = Term::stdout();
        let (height, width) = term.size();
        let width = width as usize;

        // this macro acts like println but also clears the rest of the line if there is already text
        // written there. This is to avoid flickering on redraw, and lets us update just by moving the cursor
        // position to the top left.
        macro_rules! out {
            () => (term.clear_line()?; term.write_line("")?);
            ($($arg:tt)*) => { term.clear_line()?; term.write_line(&format!($($arg)*))?; }
        }

        if options.reset_style {
            #[cfg(windows)]
            self.console_config.reset_styles()?;
            options.reset_style = false;
        }

        self.console_config.reset_cursor()?;
        let mut header_lines = if options.usage { 18 } else { 8 };

        if let Some(delay) = self.stats.last_delay {
            let late_rate = self.stats.late_samples as f64 / self.stats.overall_samples as f64;
            if late_rate > 0.10 && delay > std::time::Duration::from_secs(1) {
                let msg = format!("{:.2?} behind in sampling, results may be inaccurate. Try reducing the sampling rate.", delay);
                out!("{}", style(msg).red());
                header_lines += 1;
            }
        }

        if self.subprocesses {
             out!("Collecting samples from '{}' and subprocesses", style(&self.command).green());
        } else {
            out!("Collecting samples from '{}' (python v{})", style(&self.command).green(), self.version.as_ref().unwrap());
        }

        let error_rate = self.stats.errors as f64 / self.stats.overall_samples as f64;
        if error_rate >= 0.01 && self.stats.overall_samples > 100 {
            let error_string = self.stats.last_error.as_ref().unwrap();
            out!("Total Samples {}, Error Rate {:.2}% ({})",
                 style(self.stats.overall_samples).bold(),
                 style(error_rate * 100.0).bold().red(),
                 style(error_string).bold());
        } else {
             out!("Total Samples {}", style(self.stats.overall_samples).bold());
        }

        out!("GIL: {:.2}%, Active: {:>.2}%, Threads: {}{}",
            style(100.0 * self.stats.gil as f64 / self.stats.current_samples as f64).bold(),
            style(100.0 * self.stats.active as f64 / self.stats.current_samples as f64).bold(),
            style(self.stats.threads).bold(),
            if self.subprocesses {
                format!(", Processes {}", style(self.stats.processes).bold())
            } else {
                "".to_owned()
            });

        out!();

        // Build up the header for the table
        let mut percent_own_header = style("%Own ").reverse();
        let mut percent_total_header = style("%Total").reverse();
        let mut time_own_header = style("OwnTime").reverse();
        let mut time_total_header = style("TotalTime").reverse();
        match options.sort_column {
            1 => percent_own_header = percent_own_header.bold(),
            2 => percent_total_header = percent_total_header.bold(),
            3 => time_own_header = time_own_header.bold(),
            4 => time_total_header = time_total_header.bold(),
            _ => {}
        }

        let function_header = if options.show_linenumbers {
            style("  Function (filename:line)").reverse()
        } else {
            style("  Function (filename)").reverse()
        };

        // If we aren't at least 50 characters wide, lets use two lines per entry
        // Otherwise, truncate the filename so that it doesn't wrap around to the next line
        let header_lines =       if width > 50 { header_lines } else { header_lines + height as usize / 2 };
        let max_function_width = if width > 50 { width as usize - 35 } else { width as usize };

        out!("{:>7}{:>8}{:>9}{:>11}{:width$}", percent_own_header, percent_total_header,
             time_own_header, time_total_header, function_header, width=max_function_width);

        let mut written = 0;
        for (samples, label) in counts.iter().take(height as usize - header_lines) {
            out!("{:>6.2}% {:>6.2}% {:>7}s {:>8}s   {:.width$}",
                100.0 * samples.current_own as f64 / (self.stats.current_samples as f64),
                100.0 * samples.current_total as f64 / (self.stats.current_samples as f64),
                display_time(samples.overall_own as f64 * self.sampling_rate),
                display_time(samples.overall_total as f64 * self.sampling_rate),
                label, width=max_function_width - 2);
                written += 1;
        }
        for _ in written.. height as usize - header_lines {
            out!();
        }

        out!();
        if options.usage {
            out!("{:width$}", style(" Keyboard Shortcuts ").reverse(), width=width as usize);
            out!();
            out!("{:^12}{:<}", style("key").green(), style("action").green());
            out!("{:^12}{:<}", "1", "Sort by %Own (% of time currently spent in the function)");
            out!("{:^12}{:<}", "2", "Sort by %Total (% of time currently in the function and its children)");
            out!("{:^12}{:<}", "3", "Sort by OwnTime (Overall time spent in the function)");
            out!("{:^12}{:<}", "4", "Sort by TotalTime (Overall time spent in the function and its children)");
            out!("{:^12}{:<}", "L,l", "Toggle between aggregating by line number or by function");
            out!("{:^12}{:<}", "R,r", "Reset statistics");
            out!("{:^12}{:<}", "X,x", "Exit this help screen");
            out!();
            //println!("{:^12}{:<}", "Control-C", "Quit py-spy");
        } else {
            out!("Press {} to quit, or {} for help.",
                 style("Control-C").bold().reverse(),
                 style("?").bold().reverse());
        }
        std::io::stdout().flush()?;

        Ok(())
    }

    pub fn increment_error(&mut self, err: &Error) ->  Result<(), Error> {
        self.maybe_reset();
        self.stats.errors += 1;
        self.stats.last_error = Some(format!("{}", err));
        self.increment_common()
    }

    pub fn increment_late_sample(&mut self, delay: std::time::Duration) {
        self.stats.late_samples += 1;
        self.stats.last_delay = Some(delay);
    }

    pub fn should_refresh(&self) -> bool {
        // update faster if we only have a few samples, or if we changed options
        match self.stats.overall_samples {
            10 | 100 | 500 => true,
            _ => self.options.lock().unwrap().dirty ||
                 self.stats.elapsed >= 1.0
        }
    }

    // shared code between increment and increment_error
    fn increment_common(&mut self) -> Result<(), Error> {
        self.stats.current_samples += 1;
        self.stats.overall_samples += 1;
        self.stats.elapsed += self.sampling_rate;

        if self.should_refresh() {
            self.display()?;
            self.stats.reset_current();
        }
        Ok(())
    }

    fn maybe_reset(&mut self) {
        let mut options = self.options.lock().unwrap();
        if options.reset {
            self.stats = Stats::new();
            options.reset = false;
        }
    }
}

impl Drop for ConsoleViewer {
    fn drop(&mut self) {
        self.running.store(false, atomic::Ordering::Relaxed);
    }
}

#[derive(Eq, PartialEq, Ord, PartialOrd, Debug)]
struct FunctionStatistics {
    current_own: u64,
    current_total: u64,
    overall_own: u64,
    overall_total: u64
}

fn update_function_statistics<K>(counts: &mut HashMap<String, FunctionStatistics>, trace: &StackTrace, key_func: K)
    where K: Fn(&Frame) -> String {
    // we need to deduplicate (so we don't overcount cumulative stats with recursive function calls)
    let mut current = HashMap::new();
    for (i, frame) in trace.frames.iter().enumerate() {
        let key = key_func(frame);
        current.entry(key).or_insert(i);
    }

    for (key, order) in current {
        let entry = counts.entry(key).or_insert_with(|| FunctionStatistics{current_own: 0, current_total: 0,
                                                                           overall_own: 0, overall_total: 0});
        entry.current_total += 1;
        entry.overall_total += 1;

        if order == 0 {
            entry.current_own += 1;
            entry.overall_own += 1;
        }
    }
}

struct Options {
    dirty: bool,
    usage: bool,
    reset_style: bool,
    sort_column: i32,
    show_linenumbers: bool,
    reset: bool,
}

struct Stats {
    current_samples: u64,
    overall_samples: u64,
    elapsed: f64,
    errors: u64,
    late_samples: u64,
    threads: u64,
    processes: u64,
    active: u64,
    gil: u64,
    function_counts: HashMap<String, FunctionStatistics>,
    line_counts: HashMap<String, FunctionStatistics>,
    last_error: Option<String>,
    last_delay: Option<std::time::Duration>,
}

impl Options {
    fn new(show_linenumbers: bool) -> Options {
        Options{dirty: false, usage: false, reset: false, sort_column: 3, show_linenumbers, reset_style: false}
    }
}

impl Stats {
    fn new() -> Stats {
        Stats{current_samples: 0, overall_samples: 0, elapsed: 0.,
              errors: 0, late_samples: 0, threads: 0, processes: 0, gil: 0, active: 0,
              line_counts: HashMap::new(), function_counts: HashMap::new(),
              last_error: None, last_delay: None}
    }

    pub fn reset_current(&mut self) {
        // reset current statistics
        for val in self.line_counts.values_mut() {
            val.current_total = 0;
            val.current_own = 0;
        }

        for val in self.function_counts.values_mut() {
            val.current_total = 0;
            val.current_own = 0;
        }
        self.gil = 0;
        self.active = 0;
        self.current_samples = 0;
        self.elapsed = 0.;
    }
}

// helper function for formatting time values (hide decimals for larger values)
fn display_time(val: f64) -> String {
    if val > 1000.0 {
        format!("{:.0}", val)
    } else if val >= 100.0 {
        format!("{:.1}", val)
    } else if val >= 1.0 {
        format!("{:.2}", val)
    } else {
        format!("{:.3}", val)
    }
}

/*
This rest of this code is OS specific functions for setting up keyboard input appropriately
(don't wait for a newline, and disable echo), and clearing the terminal window.

This is all relatively low level, but there doesn't seem to be any great libraries out there
for doing this:
    https://github.com/redox-os/termion doesn't work on windows
    https://github.com/gyscos/Cursive requires ncurses installed
    https://github.com/ihalila/pancurses requires ncurses installed
 */

// operating system specific details on setting up console to receive single characters without displaying
#[cfg(unix)]
mod os_impl {
    use super::*;
    use termios::{Termios, TCSANOW, ECHO, ICANON, tcsetattr};

    pub struct ConsoleConfig {
        termios: Termios,
        stdin: i32
    }

    impl ConsoleConfig {
        pub fn new() -> io::Result<ConsoleConfig> {
            // Set up stdin to not echo the input, and not wait for a return
            let stdin = 0;
            let termios = Termios::from_fd(stdin)?;
            {
                let mut termios = termios;
                termios.c_lflag &= !(ICANON | ECHO);
                tcsetattr(stdin, TCSANOW, &termios)?;
            }

            // flush current screen so that when we clear, we don't overwrite history
            let height = Term::stdout().size().0;
            for _ in 0..=height {
                println!();
            }

            Ok(ConsoleConfig{termios, stdin})
        }

        pub fn reset_cursor(&self) -> io::Result<()> {
            // reset cursor to top left position https://en.wikipedia.org/wiki/ANSI_escape_code
            print!("\x1B[H");
            Ok(())
        }
    }

    impl Drop for ConsoleConfig {
        fn drop(&mut self) {
            tcsetattr(self.stdin, TCSANOW, &self.termios).unwrap();
        }
    }
}

// operating system specific details on setting up console to receive single characters
#[cfg(windows)]
mod os_impl {
    use super::*;
    use winapi::shared::minwindef::{DWORD};
    use winapi::um::winnt::{HANDLE};
    use winapi::um::winbase::{STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    use winapi::um::processenv::GetStdHandle;
    use winapi::um::handleapi::INVALID_HANDLE_VALUE;
    use winapi::um::consoleapi::{GetConsoleMode, SetConsoleMode};
    use winapi::um::wincon::{ENABLE_LINE_INPUT, ENABLE_ECHO_INPUT, CONSOLE_SCREEN_BUFFER_INFO, SetConsoleCursorPosition,
                             GetConsoleScreenBufferInfo, COORD, FillConsoleOutputAttribute};

    pub struct ConsoleConfig {
        stdin: HANDLE,
        mode: DWORD,
        top_left: COORD
    }

    impl ConsoleConfig {
        pub fn new() -> io::Result<ConsoleConfig> {
            unsafe {
                let stdin = GetStdHandle(STD_INPUT_HANDLE);
                if stdin == INVALID_HANDLE_VALUE {
                    return Err(io::Error::last_os_error());
                }

                let mut mode: DWORD = 0;
                if GetConsoleMode(stdin, &mut mode) == 0 {
                    return Err(io::Error::last_os_error());
                }

                if SetConsoleMode(stdin, mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT)) == 0 {
                    return Err(io::Error::last_os_error());
                }

                let stdout = GetStdHandle(STD_OUTPUT_HANDLE);

                // flush current screen so that when we clear, we don't overwrite history
                let height = Term::stdout().size().0 as i16;
                for _ in 0..height + 1 {
                    println!();
                }

                // Get information about the current console (size/background etc)
                let mut csbi = CONSOLE_SCREEN_BUFFER_INFO::default();
                if GetConsoleScreenBufferInfo(stdout, &mut csbi) == 0 {
                    return Err(io::Error::last_os_error());
                }

                // Figure out a consistent spot in the terminal buffer to write output to
                let mut top_left = csbi.dwCursorPosition;
                top_left.X = 0;
                top_left.Y = if top_left.Y > height { top_left.Y - height } else { 0 };

                Ok(ConsoleConfig{stdin, mode, top_left})
            }
        }

        pub fn reset_cursor(&self) -> io::Result<()> {
            unsafe {
                // Set cursor to top-left using the win32 api.
                // (this works better than moving the cursor using ANSI escape codes in the
                // case when the user is scrolling the terminal window)
                let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
                if SetConsoleCursorPosition(stdout, self.top_left) == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            }
        }

        pub fn reset_styles(&self) -> io::Result<()> {
            unsafe {
                let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
                let mut csbi = CONSOLE_SCREEN_BUFFER_INFO::default();
                if GetConsoleScreenBufferInfo(stdout, &mut csbi) == 0 {
                    return Err(io::Error::last_os_error());
                }

                let mut written: DWORD = 0;
                let console_size = ((1 + csbi.srWindow.Bottom - csbi.srWindow.Top) * (csbi.srWindow.Right - csbi.srWindow.Left)) as DWORD;
                if FillConsoleOutputAttribute(stdout, csbi.wAttributes, console_size, self.top_left, &mut written) == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            }
        }
    }

    impl Drop for ConsoleConfig {
        fn drop(&mut self) {
            unsafe { SetConsoleMode(self.stdin, self.mode); }
        }
    }
}
