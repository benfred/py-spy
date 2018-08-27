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
    sampling_rate: f64,
    running: Arc<atomic::AtomicBool>,
    options: Arc<Mutex<Options>>,
    stats: Stats
}

impl ConsoleViewer {
    pub fn new(show_idle: bool,
               python_command: &str,
               version: &str,
               sampling_rate: f64) -> io::Result<ConsoleViewer> {
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
                        'X' | 'x' => options.usage = false,
                        '?' => options.usage = true,
                        '1' => options.sort_column = 1,
                        '2' => options.sort_column = 2,
                        '3' => options.sort_column = 3,
                        '4' => options.sort_column = 4,
                        _ => {},
                    }
                }
            }
        });

        Ok(ConsoleViewer{console_config: os_impl::ConsoleConfig::new()?,
                         version:version.to_owned(),
                         command: python_command.to_owned(),
                         show_idle, running, options, sampling_rate,
                         stats: Stats::new()})
    }

    pub fn increment(&mut self, traces: &[StackTrace]) -> Result<(), Error> {
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
        self.stats.current_samples += 1;
        self.stats.overall_samples += 1;
        self.stats.elapsed += self.sampling_rate;

        if self.should_refresh() {
            self.display()?;
            self.stats.reset_current();
        }
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

        self.console_config.clear_screen()?;

        let term = Term::stdout();
        let (height, width) = term.size();

        // Display aggregate stats about the process
        println!("Collecting samples from '{}' (python v{})", style(&self.command).green(), &self.version);

        let error_rate = self.stats.errors as f64 / self.stats.overall_samples as f64;
        if error_rate >= 0.01 && self.stats.overall_samples > 100 {
            let error_string = self.stats.last_error.as_ref().unwrap();
            println!("Total Samples {}, Error Rate {:.2}% ({})",
                     style(self.stats.overall_samples).bold(),
                     style(error_rate * 100.0).bold().red(),
                     style(error_string).bold());
        } else {
             println!("Total Samples {}", style(self.stats.overall_samples).bold());
        }

        println!("GIL: {:.2}%, Active: {:>.2}%, Threads: {}",
            style(100.0 * self.stats.gil as f64 / self.stats.current_samples as f64).bold(),
            style(100.0 * self.stats.active as f64 / self.stats.current_samples as f64).bold(),
            style(self.stats.threads).bold());

        println!();

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

        let header_lines = if options.usage { 17 } else { 6 };

        // If we aren't at least 50 characters wide, lets use two lines per entry
        // Otherwise, truncate the filename so that it doesn't wrap around to the next line
        let header_lines =       if width > 50 { header_lines } else { header_lines + height as usize / 2 };
        let max_function_width = if width > 50 { width as usize - 35 } else { width as usize };

        println!("{:>7}{:>8}{:>9}{:>11}{:width$}", percent_own_header, percent_total_header,
                 time_own_header, time_total_header, function_header, width=max_function_width);

        let mut written = 0;
        for (samples, label) in counts.iter().take(height as usize - header_lines) {
            println!("{:>6.2}% {:>6.2}% {:>7}s {:>8}s   {:.width$}",
                100.0 * samples.current_own as f64 / (self.stats.current_samples as f64),
                100.0 * samples.current_total as f64 / (self.stats.current_samples as f64),
                display_time(samples.overall_own as f64 * self.sampling_rate),
                display_time(samples.overall_total as f64 * self.sampling_rate),
                label, width=max_function_width - 2);
                written += 1;
        }
        for _ in written.. height as usize - header_lines {
            println!();
        }

        println!();
        if options.usage {
            println!("{:width$}", style(" Keyboard Shortcuts ").reverse(), width=width as usize);
            println!();
            println!("{:^12}{:<}", style("key").bold().green(), style("action").bold().green());
            println!("{:^12}{:<}", "1", "Sort by %Own (% of time currently spent in the function)");
            println!("{:^12}{:<}", "2", "Sort by %Total (% of time currently in the function and its children)");
            println!("{:^12}{:<}", "3", "Sort by OwnTime (Overall time spent in the function)");
            println!("{:^12}{:<}", "4", "Sort by TotalTime (Overall time spent in the function and its children)");
            println!("{:^12}{:<}", "L,l", "Toggle between aggregating by line number or by function");
            println!("{:^12}{:<}", "R,r", "Reset statistics");
            println!("{:^12}{:<}", "X,x", "Exit this help screen");
            println!();
            //println!("{:^12}{:<}", "Control-C", "Quit py-spy");
        } else {
            print!("Press {} to quit, or {} for help.",
                   style("Control-C").bold().reverse(),
                   style("?").bold().reverse());
            use std::io::Write;
            std::io::stdout().flush()?;
        }

        Ok(())
    }

    pub fn increment_error(&mut self, err: &Error) {
        self.maybe_reset();
        self.stats.errors += 1;
        self.stats.overall_samples += 1;
        self.stats.last_error = Some(format!("{}", err));
    }

    pub fn should_refresh(&self) -> bool {
        // update faster if we only have a few samples, or if we changed options
        match self.stats.overall_samples {
            10 | 100 | 500 => true,
            _ => self.options.lock().unwrap().dirty ||
                 self.stats.elapsed >= 1.0
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
    sort_column: i32,
    show_linenumbers: bool,
    reset: bool,
}

struct Stats {
    current_samples: u64,
    overall_samples: u64,
    elapsed: f64,
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
        Options{dirty: false, usage: false, reset: false, sort_column: 1, show_linenumbers: true}
    }
}

impl Stats {
    fn new() -> Stats {
        Stats{current_samples: 0, overall_samples: 0, elapsed: 0.,
              errors: 0, threads: 0, gil: 0, active: 0,
              line_counts: HashMap::new(), function_counts: HashMap::new(),
              last_error: None}
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

// helper function for formating time values (hide decimals for larger values)
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
This rest of this code is OS specific functions for setting up keyboard input appropiately
(don't wait for a newline, and disable echo), and clearing the terminal window.

This is all relatively low level, but there doesn't seem to be any great libraries out there
for doing this:
    https://github.com/redox-os/termion doesn't work on windows
    https://github.com/gyscos/Cursive requires ncurses installed
    https://github.com/ihalila/pancurses requires ncurses installed
 */

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
            for _ in 0..height + 1 {
                println!();
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

        pub fn clear_screen(&self) -> io::Result<()> {
            unsafe {
                // on windows, this handles clearing screen while scrolling slightly better than
                // using ansi clear codes like on unix
                let stdout = GetStdHandle(STD_OUTPUT_HANDLE);

                // Get information about the current console (size/background etc)
                let mut csbi = CONSOLE_SCREEN_BUFFER_INFO::default();
                if GetConsoleScreenBufferInfo(stdout, &mut csbi) == 0 {
                    return Err(io::Error::last_os_error());
                }

                let mut written: DWORD = 0;
                let console_size = ((1 + csbi.srWindow.Bottom - csbi.srWindow.Top) * (csbi.srWindow.Right - csbi.srWindow.Left)) as DWORD;

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
