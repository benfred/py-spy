use std;
use std::collections::HashMap;
use std::vec::Vec;
use std::io;
use std::io::Read;
use std::sync::{Mutex, Arc, atomic};
use std::thread;

use console::{Term, style};
use failure::Error;

use stack_trace::{StackTrace, Frame};

pub struct ConsoleViewer {
    #[allow(dead_code)]
    console_config: os_impl::ConsoleConfig,
    show_idle: bool,
    version: String,
    command: String,
    running: Arc<atomic::AtomicBool>,
    options: Arc<Mutex<Options>>,
    stats: Stats
}

impl ConsoleViewer {
    pub fn new(show_idle: bool, python_command: &str, version: &str) -> io::Result<ConsoleViewer> {
        let running = Arc::new(atomic::AtomicBool::new(true));
        let options = Arc::new(Mutex::new(Options::new()));

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
                    match key as char {
                        'R' | 'r' => options.reset = true,
                        'L' | 'l' => options.show_linenumbers = !options.show_linenumbers,
                        'T' | 't' => options.show_total = true,
                        'C' | 'c' => options.show_total = false,
                        _ => options.usage = true,
                    }
                }
            }
        });
        let console_config = os_impl::ConsoleConfig::new()?;

        // flush current screen so that when we clear, we don't overwrite history
        let height = Term::stdout().size().0;
        for _ in 0..height + 1 {
            println!();
        }

        Ok(ConsoleViewer{console_config,
                      version:version.to_owned(),
                      command: python_command.to_owned(),
                      show_idle, running, options,
                      stats: Stats::new()})
    }

    pub fn increment(&mut self, traces: &[StackTrace]) {
        self.maybe_reset();
        self.stats.threads = 0;
        for trace in traces {
            self.stats.threads += 1;

            if !(self.show_idle || trace.active) {
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
                format!("{} ({}:{})", frame.name, filename, frame.line)
            });

            update_function_statistics(&mut self.stats.function_counts, trace, |frame| {
                let filename = match &frame.short_filename { Some(f) => &f, None => &frame.filename };
                format!("{} ({})", frame.name, filename)
            });
        }
        self.stats.total += 1;
    }

    pub fn display(&self) -> std::io::Result<()> {
        // Get the top aggregate function calls (either by line or by function as )
        let mut options = self.options.lock().unwrap();
        options.dirty = false;
        let counts = if options.show_linenumbers { &self.stats.line_counts } else { &self.stats.function_counts };
        let mut counts:Vec<(&FunctionStatistics, &str)> = counts.iter().map(|(x,y)| (y, x.as_ref())).collect();

        if options.show_total {
            counts.sort_unstable_by(|a, b| b.0.total.cmp(&a.0.total));
        } else {
            counts.sort_unstable_by(|a, b| b.0.cumulative.cmp(&a.0.cumulative));
        }

        self.console_config.clear_screen()?;

        let header_lines = 6;
        let term = Term::stdout();
        let (height, width) = term.size();

        // Display aggregate stats about the process
        println!("Collecting samples from '{}' (python v{})", style(&self.command).green(), &self.version);

        let error_rate = self.stats.errors as f64 / self.stats.total as f64;
        if error_rate >= 0.01 && self.stats.total > 100 {
            let error_string = self.stats.last_error.as_ref().unwrap();
            println!("Total Samples {}, Error Rate {:.2}% ({})",
                     style(self.stats.total).bold(),
                     style(error_rate * 100.0).bold().red(),
                     style(error_string).bold());
        } else {
             println!("Total Samples {}", style(self.stats.total).bold());
        }

        println!("GIL: {:.2}%, Active: {:>.2}%, Threads: {}",
            style(100.0 * self.stats.gil as f64 / self.stats.total as f64).bold(),
            style(100.0 * self.stats.active as f64 / self.stats.total as f64).bold(),
            style(self.stats.threads).bold());

        if !options.usage {
            println!();
        } else if options.show_total {
            println!("Press {} to quit, {} to toggle line numbers, {} to switch to cumulative view.",
                style("Control-C").bold(), style("L").bold(), style("C").bold());
        } else {
            println!("Press {} to quit, {} to toggle line numbers, {} to switch to total view",
                style("Control-C").bold(), style("L").bold(), style("T").bold());
        }
        options.usage = false;

        // Build up the header for the table
        let mut total_header = style("Total").reverse();
        let mut cumulative_header = style(" Cumulative").reverse();
        if options.show_total {
            total_header = total_header.bold();
        } else {
            cumulative_header = cumulative_header.bold();
        }

        let function_header = if options.show_linenumbers {
            style("   Function (filename:line)").reverse()
        } else {
            style("   Function (filename)").reverse()
        };
        println!("{:>8}{:>13}{:width$}", total_header, cumulative_header, function_header, width=width as usize - 21);

        for (samples, label) in counts.iter().take(height as usize - header_lines) {
            println!("{:>7.2}%{:>10.2}%     {}",
                100.0 * samples.total as f64 / (self.stats.total as f64),
                100.0 * samples.cumulative as f64 / (self.stats.total as f64),
                label);
        }
        Ok(())
    }

    pub fn increment_error(&mut self, err: &Error) {
        self.maybe_reset();
        self.stats.errors += 1;
        self.stats.total += 1;
        self.stats.last_error = Some(format!("{}", err));
    }

    pub fn should_refresh(&self) -> bool {
        // update faster if we only have a few samples, or if we changed options
        match self.stats.total {
            10 | 100 | 500 => true,
            _ => self.options.lock().unwrap().dirty
        }
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
    total: u64,
    cumulative: u64
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
        let entry = counts.entry(key).or_insert_with(|| FunctionStatistics{total: 0, cumulative: 0});
        entry.cumulative += 1;
        if order == 0 {
            entry.total += 1;
        }
    }
}

struct Options {
    dirty: bool,
    usage: bool,
    show_total: bool,
    show_linenumbers: bool,
    reset: bool,
}

struct Stats {
    total: u64,
    errors: u64,
    threads: u64,
    active: u64,
    gil: u64,
    function_counts: HashMap<String, FunctionStatistics>,
    line_counts: HashMap<String, FunctionStatistics>,
    last_error: Option<String>
}

impl Options {
    fn new() -> Options {
        Options{dirty: false, usage: false, reset: false, show_total: true, show_linenumbers: true}
    }
}

impl Stats {
    fn new() -> Stats {
        Stats{total: 0, errors: 0, threads: 0, gil: 0, active: 0, line_counts: HashMap::new(), function_counts: HashMap::new(), last_error: None}
    }
}

// operating system specific details on setting up console to recieve single characters without displaying
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
            let stdin = 0;
            let termios = Termios::from_fd(stdin)?;
            {
                let mut termios = termios;
                termios.c_lflag &= !(ICANON | ECHO);
                tcsetattr(stdin, TCSANOW, &termios)?;
            }

            Ok(ConsoleConfig{termios, stdin})
        }

        pub fn clear_screen(&self) -> io::Result<()> {
            // reset cursor + clear screen: https://en.wikipedia.org/wiki/ANSI_escape_code
            print!("\x1B[0f\x1B[J");
            Ok(())
        }
    }

    impl Drop for ConsoleConfig {
        fn drop(&mut self) {
            tcsetattr(self.stdin, TCSANOW, &self.termios).unwrap();
        }
    }
}

// operating system specific details on setting up console to recieve single characters
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
                            GetConsoleScreenBufferInfo, FillConsoleOutputCharacterA, COORD, FillConsoleOutputAttribute};

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

                // Get information about the current console (size/background etc)
                let mut csbi = CONSOLE_SCREEN_BUFFER_INFO{..Default::default()};
                if GetConsoleScreenBufferInfo(stdout, &mut csbi) == 0 {
                    return Err(io::Error::last_os_error());
                }

                csbi.dwCursorPosition.X = 0;

                Ok(ConsoleConfig{stdin, mode, top_left: csbi.dwCursorPosition})
            }
        }

        pub fn clear_screen(&self) -> io::Result<()> {
            unsafe {
                // on windows, this handles clearing screen while scrolling slightly better than
                // using ansi clear codes like on unix
                let stdout = GetStdHandle(STD_OUTPUT_HANDLE);

                // Get information about the current console (size/background etc)
                let mut csbi = CONSOLE_SCREEN_BUFFER_INFO{..Default::default()};
                if GetConsoleScreenBufferInfo(stdout, &mut csbi) == 0 {
                    return Err(io::Error::last_os_error());
                }

                let mut written: DWORD = 0;
                let console_size = ((csbi.srWindow.Bottom - csbi.srWindow.Top) * (csbi.srWindow.Right - csbi.srWindow.Left)) as DWORD;

                // Set the entire buffer to whitespace
                if FillConsoleOutputCharacterA(stdout, ' ' as i8, console_size, self.top_left, &mut written) == 0 {
                    return Err(io::Error::last_os_error());
                }

                // And set the entire buffer to the background colour
                if FillConsoleOutputAttribute(stdout, csbi.wAttributes, console_size, self.top_left, &mut written) == 0 {
                    return Err(io::Error::last_os_error());
                }

                // Set cursor to top-left
                if SetConsoleCursorPosition(stdout, self.top_left) == 0 {
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
