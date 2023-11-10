#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate log;

mod binary_parser;
mod chrometrace;
mod config;
mod console_viewer;
#[cfg(target_os = "linux")]
mod coredump;
#[cfg(unwind)]
mod cython;
mod dump;
mod flamegraph;
#[cfg(unwind)]
mod native_stack_trace;
mod python_bindings;
mod python_data_access;
mod python_interpreters;
mod python_process_info;
mod python_spy;
mod python_threading;
mod sampler;
mod speedscope;
mod stack_trace;
mod timer;
mod utils;
mod version;

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Error;
use console::style;

use config::{Config, FileFormat, RecordDuration};
use console_viewer::ConsoleViewer;
use stack_trace::{Frame, StackTrace};

use chrono::{Local, SecondsFormat};

#[cfg(unix)]
fn permission_denied(err: &Error) -> bool {
    err.chain().any(|cause| {
        if let Some(ioerror) = cause.downcast_ref::<std::io::Error>() {
            ioerror.kind() == std::io::ErrorKind::PermissionDenied
        } else if let Some(remoteprocess::Error::IOError(ioerror)) =
            cause.downcast_ref::<remoteprocess::Error>()
        {
            ioerror.kind() == std::io::ErrorKind::PermissionDenied
        } else {
            false
        }
    })
}

fn sample_console(pid: remoteprocess::Pid, config: &Config) -> Result<(), Error> {
    let sampler = sampler::Sampler::new(pid, config)?;

    let display = match remoteprocess::Process::new(pid)?.cmdline() {
        Ok(cmdline) => cmdline.join(" "),
        Err(_) => format!("Pid {}", pid),
    };

    let mut console =
        ConsoleViewer::new(config.show_line_numbers, &display, &sampler.version, config)?;
    for sample in sampler {
        if let Some(elapsed) = sample.late {
            console.increment_late_sample(elapsed);
        }

        if let Some(errors) = sample.sampling_errors {
            for (_, error) in errors {
                console.increment_error(&error)?
            }
        }
        console.increment(&sample.traces)?;
    }

    if !config.subprocesses {
        println!("\nprocess {} ended", pid);
    }
    Ok(())
}

pub trait Recorder {
    fn increment(&mut self, trace: &StackTrace) -> Result<(), Error>;
    fn write(&self, w: &mut dyn Write) -> Result<(), Error>;
}

impl Recorder for speedscope::Stats {
    fn increment(&mut self, trace: &StackTrace) -> Result<(), Error> {
        Ok(self.record(trace)?)
    }
    fn write(&self, w: &mut dyn Write) -> Result<(), Error> {
        self.write(w)
    }
}

impl Recorder for flamegraph::Flamegraph {
    fn increment(&mut self, trace: &StackTrace) -> Result<(), Error> {
        Ok(self.increment(trace)?)
    }
    fn write(&self, w: &mut dyn Write) -> Result<(), Error> {
        self.write(w)
    }
}

impl Recorder for chrometrace::Chrometrace {
    fn increment(&mut self, trace: &StackTrace) -> Result<(), Error> {
        Ok(self.increment(trace)?)
    }
    fn write(&self, w: &mut dyn Write) -> Result<(), Error> {
        self.write(w)
    }
}

pub struct RawFlamegraph(flamegraph::Flamegraph);

impl Recorder for RawFlamegraph {
    fn increment(&mut self, trace: &StackTrace) -> Result<(), Error> {
        Ok(self.0.increment(trace)?)
    }

    fn write(&self, w: &mut dyn Write) -> Result<(), Error> {
        self.0.write_raw(w)
    }
}

fn record_samples(pid: remoteprocess::Pid, config: &Config) -> Result<(), Error> {
    let mut output: Box<dyn Recorder> = match config.format {
        Some(FileFormat::flamegraph) => {
            Box::new(flamegraph::Flamegraph::new(config.show_line_numbers))
        }
        Some(FileFormat::speedscope) => Box::new(speedscope::Stats::new(config)),
        Some(FileFormat::raw) => Box::new(RawFlamegraph(flamegraph::Flamegraph::new(
            config.show_line_numbers,
        ))),
        Some(FileFormat::chrometrace) => {
            Box::new(chrometrace::Chrometrace::new(config.show_line_numbers))
        }
        None => return Err(format_err!("A file format is required to record samples")),
    };

    let filename = match config.filename.clone() {
        Some(filename) => filename,
        None => {
            let ext = match config.format.as_ref() {
                Some(FileFormat::flamegraph) => "svg",
                Some(FileFormat::speedscope) => "json",
                Some(FileFormat::raw) => "txt",
                Some(FileFormat::chrometrace) => "json",
                None => return Err(format_err!("A file format is required to record samples")),
            };
            let local_time = Local::now().to_rfc3339_opts(SecondsFormat::Secs, true);
            let name = match config.python_program.as_ref() {
                Some(prog) => prog[0].to_string(),
                None => match config.pid.as_ref() {
                    Some(pid) => pid.to_string(),
                    None => String::from("unknown"),
                },
            };
            format!("{}-{}.{}", name, local_time, ext)
        }
    };

    let sampler = sampler::Sampler::new(pid, config)?;

    // if we're not showing a progress bar, it's probably because we've spawned the process and
    // are displaying its stderr/stdout. In that case add a prefix to our println messages so
    // that we can distinguish
    let lede = if config.hide_progress {
        format!("{}{} ", style("py-spy").bold().green(), style(">").dim())
    } else {
        "".to_owned()
    };

    let max_intervals = match &config.duration {
        RecordDuration::Unlimited => {
            println!(
                "{}Sampling process {} times a second. Press Control-C to exit.",
                lede, config.sampling_rate
            );
            None
        }
        RecordDuration::Seconds(sec) => {
            println!(
                "{}Sampling process {} times a second for {} seconds. Press Control-C to exit.",
                lede, config.sampling_rate, sec
            );
            Some(sec * config.sampling_rate)
        }
    };

    use indicatif::ProgressBar;
    let progress = match (config.hide_progress, &config.duration) {
        (true, _) => ProgressBar::hidden(),
        (false, RecordDuration::Seconds(samples)) => ProgressBar::new(*samples),
        (false, RecordDuration::Unlimited) => {
            #[allow(clippy::let_and_return)]
            let progress = ProgressBar::new_spinner();

            // The spinner on windows doesn't look great: was replaced by a [?] character at least on
            // my system. Replace unicode spinners with just how many seconds have elapsed
            #[cfg(windows)]
            progress.set_style(
                indicatif::ProgressStyle::default_spinner()
                    .template("[{elapsed}] {msg}")
                    .unwrap(),
            );
            progress
        }
    };

    let mut errors = 0;
    let mut intervals = 0;
    let mut samples = 0;
    println!();

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    let mut exit_message = "Stopped sampling because process exited";
    let mut last_late_message = std::time::Instant::now();

    for mut sample in sampler {
        if let Some(delay) = sample.late {
            if delay > Duration::from_secs(1) {
                if config.hide_progress {
                    // display a message if we're late, but don't spam the log
                    let now = std::time::Instant::now();
                    if now - last_late_message > Duration::from_secs(1) {
                        last_late_message = now;
                        println!("{}{:.2?} behind in sampling, results may be inaccurate. Try reducing the sampling rate", lede, delay)
                    }
                } else {
                    let term = console::Term::stdout();
                    term.move_cursor_up(2)?;
                    println!("{:.2?} behind in sampling, results may be inaccurate. Try reducing the sampling rate.", delay);
                    term.move_cursor_down(1)?;
                }
            }
        }

        if !running.load(Ordering::SeqCst) {
            exit_message = "Stopped sampling because Control-C pressed";
            break;
        }

        intervals += 1;
        if let Some(max_intervals) = max_intervals {
            if intervals >= max_intervals {
                exit_message = "";
                break;
            }
        }

        for trace in sample.traces.iter_mut() {
            if !(config.include_idle || trace.active) {
                continue;
            }

            if config.gil_only && !trace.owns_gil {
                continue;
            }

            if config.include_thread_ids {
                let threadid = trace.format_threadid();
                let thread_fmt = if let Some(thread_name) = &trace.thread_name {
                    format!("thread ({}): {}", threadid, thread_name)
                } else {
                    format!("thread ({})", threadid)
                };
                trace.frames.push(Frame {
                    name: thread_fmt,
                    filename: String::from(""),
                    module: None,
                    short_filename: None,
                    line: 0,
                    locals: None,
                });
            }

            if let Some(process_info) = trace.process_info.as_ref() {
                trace.frames.push(process_info.to_frame());
                let mut parent = process_info.parent.as_ref();
                while parent.is_some() {
                    if let Some(process_info) = parent {
                        trace.frames.push(process_info.to_frame());
                        parent = process_info.parent.as_ref();
                    }
                }
            }

            samples += 1;
            output.increment(trace)?;
        }

        if let Some(sampling_errors) = sample.sampling_errors {
            for (pid, e) in sampling_errors {
                warn!("Failed to get stack trace from {}: {}", pid, e);
                errors += 1;
            }
        }

        if config.duration == RecordDuration::Unlimited {
            let msg = if errors > 0 {
                format!("Collected {} samples ({} errors)", samples, errors)
            } else {
                format!("Collected {} samples", samples)
            };
            progress.set_message(msg);
        }
        progress.inc(1);
    }
    progress.finish();
    // write out a message here (so as not to interfere with progress bar) if we ended earlier
    if !exit_message.is_empty() {
        println!("\n{}{}", lede, exit_message);
    }

    {
        let mut out_file = std::fs::File::create(&filename)?;
        output.write(&mut out_file)?;
    }

    match config.format.as_ref().unwrap() {
        FileFormat::flamegraph => {
            println!(
                "{}Wrote flamegraph data to '{}'. Samples: {} Errors: {}",
                lede, filename, samples, errors
            );
            // open generated flame graph in the browser on OSX (theory being that on linux
            // you might be SSH'ed into a server somewhere and this isn't desired, but on
            // that is pretty unlikely for osx) (note to self: xdg-open will open on linux)
            #[cfg(target_os = "macos")]
            std::process::Command::new("open").arg(&filename).spawn()?;
        }
        FileFormat::speedscope => {
            println!(
                "{}Wrote speedscope file to '{}'. Samples: {} Errors: {}",
                lede, filename, samples, errors
            );
            println!("{}Visit https://www.speedscope.app/ to view", lede);
        }
        FileFormat::raw => {
            println!(
                "{}Wrote raw flamegraph data to '{}'. Samples: {} Errors: {}",
                lede, filename, samples, errors
            );
            println!("{}You can use the flamegraph.pl script from https://github.com/brendangregg/flamegraph to generate a SVG", lede);
        }
        FileFormat::chrometrace => {
            println!(
                "{}Wrote chrome trace to '{}'. Samples: {} Errors: {}",
                lede, filename, samples, errors
            );
            println!("{}Visit chrome://tracing to view", lede);
        }
    };

    Ok(())
}

fn run_spy_command(pid: remoteprocess::Pid, config: &config::Config) -> Result<(), Error> {
    match config.command.as_ref() {
        "dump" => {
            dump::print_traces(pid, config, None)?;
        }
        "record" => {
            record_samples(pid, config)?;
        }
        "top" => {
            sample_console(pid, config)?;
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

    #[cfg(target_os = "macos")]
    {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("This program requires root on OSX.");
            eprintln!("Try running again with elevated permissions by going 'sudo !!'");
            std::process::exit(1)
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(ref core_filename) = config.core_filename {
            let core = coredump::PythonCoreDump::new(std::path::Path::new(&core_filename))?;
            let traces = core.get_stack(&config)?;
            return core.print_traces(&traces, &config);
        }
    }

    if let Some(pid) = config.pid {
        run_spy_command(pid, &config)?;
    } else if let Some(ref subprocess) = config.python_program {
        // Dump out stdout/stderr from the process to a temp file, so we can view it later if needed
        let mut process_output = tempfile::NamedTempFile::new()?;

        let mut command = std::process::Command::new(&subprocess[0]);
        #[cfg(unix)]
        {
            // Drop root permissions if possible: https://github.com/benfred/py-spy/issues/116
            if unsafe { libc::geteuid() } == 0 {
                if let Ok(sudo_uid) = std::env::var("SUDO_UID") {
                    use std::os::unix::process::CommandExt;
                    info!(
                        "Dropping root and running python command as {}",
                        std::env::var("SUDO_USER")?
                    );
                    command.uid(sudo_uid.parse::<u32>()?);
                }
            }
        }

        let mut command = command.args(&subprocess[1..]);

        if config.capture_output {
            command = command
                .stdin(std::process::Stdio::null())
                .stdout(process_output.reopen()?)
                .stderr(process_output.reopen()?)
        }

        let mut command = command
            .spawn()
            .map_err(|e| format_err!("Failed to create process '{}': {}", subprocess[0], e))?;

        #[cfg(target_os = "macos")]
        {
            // sleep just in case: https://jvns.ca/blog/2018/01/28/mac-freeze/
            std::thread::sleep(Duration::from_millis(50));
        }
        let result = run_spy_command(command.id() as _, &config);

        // check exit code of subprocess
        std::thread::sleep(Duration::from_millis(1));
        let success = match command.try_wait()? {
            Some(exit) => exit.success(),
            // if process hasn't finished, assume success
            None => true,
        };

        // if we failed for any reason, dump out stderr from child process here
        // (could have useful error message)
        if config.capture_output && (!success || result.is_err()) {
            let mut buffer = String::new();
            if process_output.read_to_string(&mut buffer).is_ok() {
                eprintln!("{}", buffer);
            }
        }

        // kill it so we don't have dangling processes
        if command.kill().is_err() {
            // I don't actually care if we failed to kill ... most times process is already done
            // eprintln!("Error killing child process {}", e);
        }
        return result;
    }

    Ok(())
}

fn main() {
    env_logger::builder()
        .format_timestamp_nanos()
        .try_init()
        .unwrap();

    if let Err(err) = pyspy_main() {
        #[cfg(unix)]
        {
            if permission_denied(&err) {
                // Got a permission denied error, if we're not running as root - ask to use sudo
                if unsafe { libc::geteuid() } != 0 {
                    eprintln!("Permission Denied: Try running again with elevated permissions by going 'sudo env \"PATH=$PATH\" !!'");
                    std::process::exit(1);
                }

                // We got a permission denied error running as root, check to see if we're running
                // as docker, and if so ask the user to check the SYS_PTRACE capability is added
                // Otherwise, fall through to the generic error handling
                #[cfg(target_os = "linux")]
                if let Ok(cgroups) = std::fs::read_to_string("/proc/self/cgroup") {
                    if cgroups.contains("/docker/") {
                        eprintln!("Permission Denied");
                        eprintln!("\nIt looks like you are running in a docker container. Please make sure \
                        you started your container with the SYS_PTRACE capability. See \
                        https://github.com/benfred/py-spy#how-do-i-run-py-spy-in-docker for \
                        more details");
                        std::process::exit(1);
                    }
                }
            }
        }

        eprintln!("Error: {}", err);
        for (i, suberror) in err.chain().enumerate() {
            if i > 0 {
                eprintln!("Reason: {}", suberror);
            }
        }
        std::process::exit(1);
    }
}
