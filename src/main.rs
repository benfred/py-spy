#[macro_use]
extern crate clap;
extern crate console;
extern crate ctrlc;
extern crate env_logger;
#[macro_use]
extern crate failure;
extern crate goblin;
extern crate indicatif;
extern crate inferno;
#[macro_use]
extern crate lazy_static;
extern crate libc;
#[macro_use]
extern crate log;
#[cfg(unwind)]
extern crate lru;
extern crate memmap;
extern crate proc_maps;
extern crate regex;
extern crate tempfile;
#[cfg(unix)]
extern crate termios;
#[cfg(windows)]
extern crate winapi;
extern crate cpp_demangle;
extern crate rand;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;

extern crate remoteprocess;

mod config;
mod binary_parser;
#[cfg(unwind)]
mod cython;
#[cfg(unwind)]
mod native_stack_trace;
mod python_bindings;
mod python_interpreters;
mod python_spy;
mod stack_trace;
mod console_viewer;
mod flamegraph;
mod speedscope;
mod timer;
mod utils;
mod version;

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use failure::Error;

use python_spy::PythonSpy;
use stack_trace::{StackTrace, Frame};
use console_viewer::ConsoleViewer;
use config::{Config, FileFormat, RecordDuration};

fn format_trace_threadid(trace: &StackTrace) -> String {
    // native threadids in osx are kinda useless, use the pthread id instead
    #[cfg(target_os="macos")]
    return format!("{:#X}", trace.thread_id);

    // otherwise use the native threadid if given
    #[cfg(not(target_os="macos"))]
    match trace.os_thread_id {
        Some(tid) => format!("{}", tid),
        None => format!("{:#X}", trace.thread_id)
    }
}

fn print_traces(process: &mut PythonSpy, config: &Config) -> Result<(), Error> {
    if config.dump_json {
        let traces = process.get_stack_traces()?;
        println!("{}", serde_json::to_string_pretty(&traces)?);
        return Ok(())
    }

    use console::style;
    println!("Process {}: {}",
        style(process.pid).bold().yellow(),
        process.process.cmdline()?.join(" "));

    println!("Python v{} ({})\n",
        style(&process.version).bold(),
        style(process.process.exe()?).dim());

    let traces = process.get_stack_traces()?;

    for trace in traces.iter().rev() {
        let thread_id = format_trace_threadid(&trace);
        println!("Thread {} ({})", style(thread_id).bold().yellow(), trace.status_str());
        for frame in &trace.frames {
            let filename = match &frame.short_filename { Some(f) => &f, None => &frame.filename };
            if frame.line != 0 {
                println!("\t {} ({}:{})", style(&frame.name).green(), style(&filename).cyan(), style(frame.line).dim());
            } else {
                println!("\t {} ({})", style(&frame.name).green(), style(&filename).cyan());
            }
        }
    }
    Ok(())
}

fn process_exitted(process: &remoteprocess::Process) -> bool {
    process.exe().is_err()
}

#[cfg(unix)]
fn permission_denied(err: &Error) -> bool {
    err.iter_chain().any(|cause| {
        if let Some(ioerror) = cause.downcast_ref::<std::io::Error>() {
            ioerror.kind() == std::io::ErrorKind::PermissionDenied
        } else if let Some(remoteprocess::Error::IOError(ioerror)) = cause.downcast_ref::<remoteprocess::Error>() {
            ioerror.kind() == std::io::ErrorKind::PermissionDenied
        }else {
            false
        }
    })
}

fn sample_console(process: &mut PythonSpy,
                  config: &Config) -> Result<(), Error> {
    let rate = config.sampling_rate;
    let display = match process.process.cmdline() {
        Ok(cmdline) => cmdline.join(" "),
        Err(_) => format!("Pid {}", process.process.pid)
    };

    let mut console = ConsoleViewer::new(config.show_line_numbers, &display,
                                         &format!("{}", process.version),
                                         1.0 / rate as f64)?;

    for sleep in timer::Timer::new(rate as f64) {
        if let Err(elapsed) = sleep {
            console.increment_late_sample(elapsed);
        }

        match process.get_stack_traces() {
            Ok(traces) => {
                console.increment(&traces)?;
            },
            Err(err) => {
                if process_exitted(&process.process) {
                    println!("\nprocess {} ended", process.pid);
                    break;
                } else {
                    console.increment_error(&err)?;
                }
            }
        }

    }
    Ok(())
}

pub trait Recorder {
    fn increment(&mut self, trace: &StackTrace) -> Result<(), Error>;
    fn write(&self, w: &mut std::fs::File) -> Result<(), Error>;
}

impl Recorder for speedscope::Stats {
    fn increment(&mut self, trace: &StackTrace) -> Result<(), Error> {
        Ok(self.record(trace)?)
    }
    fn write(&self, w: &mut std::fs::File) -> Result<(), Error> {
        self.write(w)
    }
}

impl Recorder for flamegraph::Flamegraph {
    fn increment(&mut self, trace: &StackTrace) -> Result<(), Error> {
        Ok(self.increment(trace)?)
    }
    fn write(&self, w: &mut std::fs::File) -> Result<(), Error> {
        self.write(w)
    }
}

pub struct RawFlamegraph(flamegraph::Flamegraph);

impl Recorder for RawFlamegraph {
    fn increment(&mut self, trace: &StackTrace) -> Result<(), Error> {
        Ok(self.0.increment(trace)?)
    }

    fn write(&self, w: &mut std::fs::File) -> Result<(), Error> {
        self.0.write_raw(w)
    }
}

fn record_samples(process: &mut PythonSpy, config: &Config) -> Result<(), Error> {
    let mut output: Box<dyn Recorder> = match config.format {
        Some(FileFormat::flamegraph) => Box::new(flamegraph::Flamegraph::new(config.show_line_numbers)),
        Some(FileFormat::speedscope) =>  Box::new(speedscope::Stats::new()),
        Some(FileFormat::raw) => Box::new(RawFlamegraph(flamegraph::Flamegraph::new(config.show_line_numbers))),
        None => return Err(format_err!("A file format is required to record samples"))
    };

    let filename = match config.filename.as_ref() {
        Some(filename) => filename,
        None => return Err(format_err!("A filename is required to record samples"))
    };

    let mut max_samples = None;
    use indicatif::ProgressBar;

    let progress = match (config.hide_progess, &config.duration) {
        (true, _) => ProgressBar::hidden(),
        (false, RecordDuration::Seconds(sec)) => {
            max_samples = Some(sec * config.sampling_rate);
            println!("Sampling process {} times a second for {} seconds. Press Control-C to exit.",
                config.sampling_rate, sec);
            ProgressBar::new(max_samples.unwrap())
        }
        (false, RecordDuration::Unlimited) => {
            println!("Sampling process {} times a second. Press Control-C to exit.",
                config.sampling_rate);
            let progress = ProgressBar::new_spinner();

            // The spinner on windows doesn't look great: was replaced by a [?] character at least on
            // my system. Replace unicode spinners with just how many seconds have elapsed
            #[cfg(windows)]
            progress.set_style(indicatif::ProgressStyle::default_spinner().template("[{elapsed}] {msg}"));
            progress
        }
    };

    let mut errors = 0;
    let mut samples = 0;
    println!();

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    let mut exit_message = "";

    for sleep in timer::Timer::new(config.sampling_rate as f64) {
        if let Err(delay) = sleep {
            if delay > Duration::from_secs(1) && !config.hide_progess {
                let term = console::Term::stdout();
                term.move_cursor_up(2)?;
                println!("{:.2?} behind in sampling, results may be inaccurate. Try reducing the sampling rate.", delay);
                term.move_cursor_down(1)?;
            }
        }

        if !running.load(Ordering::SeqCst) {
            exit_message = "Stopped sampling because Control-C pressed";
            break;
        }

        match process.get_stack_traces() {
            Ok(traces) => {
                for mut trace in traces {
                    if !(config.include_idle || trace.active) {
                        continue;
                    }

                    if config.gil_only && !trace.owns_gil {
                        continue;
                    }

                    if config.include_thread_ids {
                        let threadid = format_trace_threadid(&trace);
                        trace.frames.push(Frame{name: format!("thread {}", threadid),
                            filename: String::from(""),
                            module: None, short_filename: None, line: 0});
                    }

                    output.increment(&trace)?;
                }

                samples += 1;
                if let Some(max_samples) = max_samples {
                    if samples >= max_samples {
                        break;
                    }
                }
            },
            Err(e) => {
                if process_exitted(&process.process) {
                    exit_message = "Stopped sampling because the process ended";
                    break;
                } else {
                    warn!("Failed to get stack trace {:?}", e);
                    errors += 1;
                }
            }
        }
        if config.duration == RecordDuration::Unlimited {
            let msg = if errors > 0 {
                format!("Collected {} samples ({} errors)", samples, errors)
            } else {
                format!("Collected {} samples", samples)
            };
            progress.set_message(&msg);
        }

        progress.inc(1);
    }
    progress.finish();
    // write out a message here (so as not to interfere with progress bar) if we ended earlier
    if !exit_message.is_empty() {
        println!("{}", exit_message);
    }

    {
    let mut out_file = std::fs::File::create(filename)?;
    output.write(&mut out_file)?;
    }

    match config.format.as_ref().unwrap() {
        FileFormat::flamegraph => {
            println!("Wrote flamegraph data to '{}'. Samples: {} Errors: {}", filename, samples, errors);
            // open generated flame graph in the browser on OSX (theory being that on linux
            // you might be SSH'ed into a server somewhere and this isn't desired, but on
            // that is pretty unlikely for osx) (note to self: xdg-open will open on linux)
            #[cfg(target_os = "macos")]
            std::process::Command::new("open").arg(filename).spawn()?;
        },
        FileFormat::speedscope =>  {
            println!("Wrote speedscope file to '{}'. Samples: {} Errors: {}", filename, samples, errors);
            println!("Visit https://www.speedscope.app/ to view");
        },
        FileFormat::raw => {
            println!("Wrote raw flamegraph data to '{}'. Samples: {} Errors: {}", filename, samples, errors);
            println!("You can use the flamegraph.pl script from https://github.com/brendangregg/flamegraph to generate a SVG");
        }
    };

    Ok(())
}

fn run_spy_command(process: &mut PythonSpy, config: &config::Config) -> Result<(), Error> {
    match config.command.as_ref() {
        "dump" =>  {
            print_traces(process, config)?;
        },
        "record" => {
            record_samples(process, config)?;
        },
        "top" => {
            sample_console(process, config)?;
        }
        _ => {
            // shouldn't happen
            return Err(format_err!("Unknown command {}", config.command));
        }
    }
    Ok(())
}

fn pyspy_main() -> Result<(), Error> {
    let config = config::Config::from_commandline();

    #[cfg(target_os="macos")]
    {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("This program requires root on OSX.");
            eprintln!("Try running again with elevated permissions by going 'sudo !!'");
            std::process::exit(1)
        }
    }

    if let Some(pid) = config.pid {
        let mut process = PythonSpy::retry_new(pid, &config, 3)?;
        run_spy_command(&mut process, &config)?;
    }

    else if let Some(ref subprocess) = config.python_program {
        // Dump out stdout/stderr from the process to a temp file, so we can view it later if needed
        let mut process_output = tempfile::NamedTempFile::new()?;

        let mut command = std::process::Command::new(&subprocess[0]);
        #[cfg(unix)]
        {
            // Drop root permissions if possible: https://github.com/benfred/py-spy/issues/116
            if unsafe { libc::geteuid() } == 0 {
                if let Ok(sudo_uid) = std::env::var("SUDO_UID") {
                    use std::os::unix::process::CommandExt;
                    info!("Dropping root and running python command as {}", std::env::var("SUDO_USER")?);
                    command.uid(sudo_uid.parse::<u32>()?);
                }
            }
        }

        let mut command = command.args(&subprocess[1..])
            .stdin(std::process::Stdio::null())
            .stdout(process_output.reopen()?)
            .stderr(process_output.reopen()?)
            .spawn()
            .map_err(|e| format_err!("Failed to create process '{}': {}", subprocess[0], e))?;

        #[cfg(target_os="macos")]
        {
            // sleep just in case: https://jvns.ca/blog/2018/01/28/mac-freeze/
            std::thread::sleep(Duration::from_millis(50));
        }
        let result = match PythonSpy::retry_new(command.id() as remoteprocess::Pid, &config, 8) {
            Ok(mut process) => {
                run_spy_command(&mut process, &config)
            },
            Err(e) => Err(e)
        };

        // check exit code of subprocess
        std::thread::sleep(Duration::from_millis(1));
        let success =  match command.try_wait()? {
            Some(exit) => exit.success(),
            // if process hasn't finished, assume success
            None => true
        };

        // if we failed for any reason, dump out stderr from child process here
        // (could have useful error message)
        if !success || result.is_err() {
            let mut buffer = String::new();
            if process_output.read_to_string(&mut buffer).is_ok() {
                eprintln!("{}", buffer);
            }
        }

        // kill it so we don't have dangling processess
        if command.kill().is_err() {
            // I don't actually care if we failed to kill ... most times process is already done
            // eprintln!("Error killing child process {}", e);
        }
        return result;
    }

    Ok(())
}

fn main() {
    env_logger::init();

    if let Err(err) = pyspy_main() {
        #[cfg(unix)]
        {
        if permission_denied(&err) {
            eprintln!("Permission Denied: Try running again with elevated permissions by going 'sudo env \"PATH=$PATH\" !!'");
            std::process::exit(1);
        }
        }

        eprintln!("Error: {}", err);
        for (i, suberror) in err.iter_chain().enumerate() {
            if i > 0 {
                eprintln!("Reason: {}", suberror);
            }
        }
        eprintln!("{}", err.backtrace());
        std::process::exit(1);
    }
}
