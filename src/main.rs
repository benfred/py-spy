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
mod dump;
mod binary_parser;
#[cfg(unwind)]
mod cython;
#[cfg(unwind)]
mod native_stack_trace;
mod python_bindings;
mod python_interpreters;
mod python_spy;
mod python_data_access;
mod stack_trace;
mod console_viewer;
mod flamegraph;
mod speedscope;
mod timer;
mod utils;
mod version;

use std::collections::HashSet;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;
use std::thread;

use failure::Error;

use python_spy::PythonSpy;
use stack_trace::{StackTrace, Frame};
use console_viewer::ConsoleViewer;
use config::{Config, FileFormat, RecordDuration};

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
        } else {
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

    let running = Arc::new(AtomicBool::new(true));
    let (trace_sender, trace_receiver) = channel();
    let (result_sender, result_receiver) = channel();

    // Spawn threads to profile process and its subprocesses in case
    // if the --subprocesses config flag was supplied.
    spawn_recorder_threads(
        result_sender,
        trace_sender,
        process.process,
        running.clone(),
        &config.clone()
    );

    for event in trace_receiver.iter() {
        match event {
            TraceEvent::Trace((traces, _)) => {
                console.increment(&traces)?;
            },
            TraceEvent::TimingErr(delay) => {
                console.increment_late_sample(delay);
            },
            TraceEvent::Err(error) => {
                console.increment_error(&error)?;
            },
        }
    }

    // There is a variety of situtations when profiling subprocesses
    // may fail. For instance, a child process may end up being not a
    // python process.
    //
    // Nonetheless, if we were able to profile something, we're fine.
    let mut last_err = Ok(());

    for result in result_receiver {
        if result.is_ok() {
            return result;
        }

        last_err = result;
    }

    last_err
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

#[derive(Debug)]
pub enum TraceEvent {
    Trace((Vec<StackTrace>, u64)),
    Err(Error),
    TimingErr(Duration),
}

/// Records traces, sends them over via an mpsc sender.
///
/// This function is meant to be used in both console and
/// record reporters.
fn do_record_samples(trace_event_sender: Sender<TraceEvent>,
                     pid: remoteprocess::Pid,
                     running: Arc<AtomicBool>,
                     config: &Config,
) -> Result<(), Error> {
    // Each thread maintains its own samples counter.
    let mut samples = 0;
    let sampling_rate = config.sampling_rate as f64;
    let mut process = PythonSpy::retry_new(pid, &config, 3)?;

    for sleep in timer::Timer::new(sampling_rate) {
        if let Err(delay) = sleep {
            trace_event_sender.send(TraceEvent::TimingErr(delay))?;
        }

        if !running.load(Ordering::SeqCst) {
            // We have been told to shut ourselves down.
            break;
        }

        match process.get_stack_traces() {
            Ok(traces) => {
                samples = samples + 1;
                trace_event_sender.send(TraceEvent::Trace((traces, samples)))?;
            },
            Err(e) => {
                if process_exitted(&process.process) {
                    // Process has exited, tell other threads to exit.
                    running.store(false, Ordering::SeqCst);
                    break;
                } else {
                    trace_event_sender.send(TraceEvent::Err(e))?;
                }
            }
        }
    }

    Ok(())
}

/// Spawn recorder threads
///
/// This function is meant to be used in both console and
/// record reporters.
///
/// If --subprocesses configuration option is supplied, the function
/// continuously monitors process' children and spawns new recorder
/// threads, if needed
fn spawn_recorder_threads(result_sender: Sender<Result<(), Error>>,
                          trace_event_sender: Sender<TraceEvent>,
                          remoteprocess: remoteprocess::Process,
                          running: Arc<AtomicBool>,
                          config: &Config,
) {
    let config_clone = config.clone();

    thread::spawn(move || {
        if !config_clone.subprocesses {
            let result = do_record_samples(
                trace_event_sender,
                remoteprocess.pid,
                running,
                &config_clone,
            );

            result_sender.send(result)
                .expect("Couldn't send result");

            return;
        }

        let mut pids: HashSet<remoteprocess::Pid> = HashSet::new();

        while running.load(Ordering::SeqCst) {
            let children = remoteprocess.children()
                .expect("Error retrieving children of process");

            for pid in children {
                if !pids.insert(pid) {
                    continue;
                }

                let trace_event_sender = trace_event_sender.clone();
                let running = running.clone();
                let config_clone = config_clone.clone();
                let result_sender = result_sender.clone();

                thread::spawn(move || {
                    let result = do_record_samples(
                        trace_event_sender,
                        pid,
                        running,
                        &config_clone,
                    );

                    result_sender.send(result)
                        .expect("Couldn't send result");
                });
            }
            thread::sleep(Duration::from_secs(1));
        }
    });

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

    let running = Arc::new(AtomicBool::new(true));
    let interrupted = Arc::new(AtomicBool::new(false));

    let interrupted_clone_ctrlc_handler = interrupted.clone();
    let running_clone_ctrlc_handler = running.clone();

    ctrlc::set_handler(move || {
        interrupted_clone_ctrlc_handler.store(true, Ordering::Relaxed);
        running_clone_ctrlc_handler.store(false, Ordering::SeqCst);
    })?;

    let (trace_sender, trace_receiver) = channel();
    let (result_sender, result_receiver) = channel();

    spawn_recorder_threads(
        result_sender,
        trace_sender,
        process.process,
        running.clone(),
        &config.clone()
    );

    let mut samples = 0;
    let mut errors = 0;

    for event in trace_receiver.iter() {
        match event {
            TraceEvent::Trace((traces, count)) => {
                for mut trace in traces {
                    if !(config.include_idle || trace.active) {
                        continue;
                    }

                    if config.gil_only && !trace.owns_gil {
                        continue;
                    }

                    if config.include_thread_ids {
                        let threadid = trace.format_threadid();
                        trace.frames.push(
                            Frame{name: format!("thread {}", threadid),
                                  filename: String::from(""),
                                  module: None, short_filename: None,
                                  line: 0, locals: None});
                    }

                    output.increment(&trace)?;

                    samples += 1;

                    if let Some(max_samples) = max_samples {
                        if count >= max_samples {
                            running.store(false, Ordering::SeqCst);
                            break;
                        }
                    }
                }
            },
            TraceEvent::TimingErr(delay) => {
                if delay > Duration::from_secs(1) && !config.hide_progess {
                    let term = console::Term::stdout();
                    term.move_cursor_up(2)?;
                    println!("{:.2?} behind in sampling, results may be inaccurate. Try reducing the sampling rate.", delay);
                    term.move_cursor_down(1)?;
                }
            },
            TraceEvent::Err(error) => {
                errors += 1;
                warn!("Failed to get stack trace {}", error);
            },
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
    if interrupted.load(Ordering::SeqCst) {
        println!("{}", "Stopped sampling because Control-C pressed");
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

    let mut last_err = Ok(());

    for result in result_receiver {
        if result.is_ok() {
            return result;
        }

        last_err = result;
    }

    last_err
}

fn run_spy_command(process: &mut PythonSpy, config: &config::Config) -> Result<(), Error> {
    match config.command.as_ref() {
        "dump" =>  {
            dump::print_traces(process, config)?;
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
    env_logger::builder().default_format_timestamp_nanos(true).try_init().unwrap();

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
