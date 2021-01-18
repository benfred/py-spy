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
mod python_threading;
mod stack_trace;

mod console_viewer;
mod flamegraph;
mod speedscope;
mod sampler;
mod timer;
mod utils;
mod version;
#[cfg(feature="serve")]
mod web_viewer;

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use console::style;
use log::{info, warn};
use failure::{Error, format_err};

use stack_trace::{StackTrace, Frame};
use console_viewer::ConsoleViewer;
use config::{Config, FileFormat, RecordDuration};

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

fn sample_console(pid: remoteprocess::Pid,
                  config: &Config) -> Result<(), Error> {
    let sampler = sampler::Sampler::new(pid, config)?;

    let display = match remoteprocess::Process::new(pid)?.cmdline() {
        Ok(cmdline) => cmdline.join(" "),
        Err(_) => format!("Pid {}", pid)
    };

    let mut console = ConsoleViewer::new(config.show_line_numbers, &display,
                                         &sampler.version,
                                         config)?;
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

#[cfg(feature="serve")]
fn sample_serve(pid: remoteprocess::Pid, config: &Config) -> Result<(), Error> {
    let sampler = sampler::Sampler::new(pid, config)?;

    let display = match remoteprocess::Process::new(pid)?.cmdline() {
        Ok(cmdline) => cmdline.join(" "),
        Err(_) => format!("Pid {}", pid)
    };

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    }).expect("Error setting Ctrl-C handler");

    let start = std::time::Instant::now();

    let version = match sampler.version.as_ref() {
        Some(version) => format!("{}", version),
        _ => "".to_owned()
    };

    // if we're not showing a progress bar, it's probably because we've spawned the process and
    // are displaying its stderr/stdout. In that case add a prefix to our println messages so
    // that we can distinguish
    let lede = if config.hide_progress {
        format!("{}{} ", style("py-spy").bold().green(), style(">").dim())
    } else {
        "".to_owned()
    };
    let mut last_late_message = std::time::Instant::now();

    let address = config.address.as_ref().expect("need server address for serving results");

    let mut collector = web_viewer::TraceCollector::new(&display, &version, config)?;
    let addr = web_viewer::start_server(address, &collector)?;
    println!("{}Serving requests at {}\n", lede, style(format!("http://{}/", addr)).bold().underlined());


    for sample in sampler {
        if let Some(delay) = sample.late {
            if delay > Duration::from_secs(1) {
                let now = std::time::Instant::now();
                if now - last_late_message > Duration::from_secs(1) {
                    last_late_message = now;
                    println!("{}{:.2?} behind in sampling, results may be inaccurate. Try reducing the sampling rate", lede, delay)
                }
            }
        }

        collector.increment(sample)?;
        if !running.load(Ordering::SeqCst) {
            break;
        }
    }

    if running.load(Ordering::SeqCst) {
        println!("\n{}process {} ended (elapsed: {:?}", lede, pid, std::time::Instant::now() - start);
        println!("{}Press Control-C to stop serving", lede);
        collector.notify_exitted();
        while running.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
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

fn record_samples(pid: remoteprocess::Pid, config: &Config) -> Result<(), Error> {
    let mut output: Box<dyn Recorder> = match config.format {
        Some(FileFormat::flamegraph) => Box::new(flamegraph::Flamegraph::new(config.show_line_numbers)),
        Some(FileFormat::speedscope) =>  Box::new(speedscope::Stats::new(config.show_line_numbers)),
        Some(FileFormat::raw) => Box::new(RawFlamegraph(flamegraph::Flamegraph::new(config.show_line_numbers))),
        None => return Err(format_err!("A file format is required to record samples"))
    };

    let filename = match config.filename.as_ref() {
        Some(filename) => filename,
        None => return Err(format_err!("A filename is required to record samples"))
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

    let max_samples = match &config.duration {
        RecordDuration::Unlimited => {
            println!("{}Sampling process {} times a second. Press Control-C to exit.", lede, config.sampling_rate);
            None
        },
        RecordDuration::Seconds(sec) => {
            println!("{}Sampling process {} times a second for {} seconds. Press Control-C to exit.", lede, config.sampling_rate, sec);
            Some(sec * config.sampling_rate)
        }
    };

    use indicatif::ProgressBar;
    let progress = match (config.hide_progress, &config.duration) {
        (true, _) => ProgressBar::hidden(),
        (false, RecordDuration::Seconds(samples)) => ProgressBar::new(*samples),
        (false, RecordDuration::Unlimited) => {
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

    let mut exit_message = "Stopped sampling because process exitted";
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

        samples += 1;
        if let Some(max_samples) = max_samples {
            if samples >= max_samples {
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
                trace.frames.push(Frame{name: format!("thread ({})", threadid),
                    filename: String::from(""),
                    module: None, short_filename: None, line: 0, locals: None});
            }

            if let Some(process_info) = trace.process_info.as_ref().map(|x| x) {
                trace.frames.push(process_info.to_frame());
                let mut parent = process_info.parent.as_ref();
                while parent.is_some() {
                    if let Some(process_info) = parent {
                        trace.frames.push(process_info.to_frame());
                        parent = process_info.parent.as_ref();
                    }
                }
            }

            output.increment(&trace)?;
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
            progress.set_message(&msg);
        }
        progress.inc(1);
    }
    progress.finish();
    // write out a message here (so as not to interfere with progress bar) if we ended earlier
    if !exit_message.is_empty() {
        println!("\n{}{}", lede, exit_message);
    }

    {
    let mut out_file = std::fs::File::create(filename)?;
    output.write(&mut out_file)?;
    }

    match config.format.as_ref().unwrap() {
        FileFormat::flamegraph => {
            println!("{}Wrote flamegraph data to '{}'. Samples: {} Errors: {}", lede, filename, samples, errors);
            // open generated flame graph in the browser on OSX (theory being that on linux
            // you might be SSH'ed into a server somewhere and this isn't desired, but on
            // that is pretty unlikely for osx) (note to self: xdg-open will open on linux)
            #[cfg(target_os = "macos")]
            std::process::Command::new("open").arg(filename).spawn()?;
        },
        FileFormat::speedscope =>  {
            println!("{}Wrote speedscope file to '{}'. Samples: {} Errors: {}", lede, filename, samples, errors);
            println!("{}Visit https://www.speedscope.app/ to view", lede);
        },
        FileFormat::raw => {
            println!("{}Wrote raw flamegraph data to '{}'. Samples: {} Errors: {}", lede, filename, samples, errors);
            println!("{}You can use the flamegraph.pl script from https://github.com/brendangregg/flamegraph to generate a SVG", lede);
        }
    };

    Ok(())
}

fn run_spy_command(pid: remoteprocess::Pid, config: &config::Config) -> Result<(), Error> {
    match config.command.as_ref() {
        "serve" => {
            #[cfg(feature="serve")]
            sample_serve(pid, config)?;
        },
        "dump" =>  {
            dump::print_traces(pid, config)?;
        },
        "record" => {
            record_samples(pid, config)?;
        },
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

    #[cfg(target_os="macos")]
    {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("This program requires root on OSX.");
            eprintln!("Try running again with elevated permissions by going 'sudo !!'");
            std::process::exit(1)
        }
    }

    if let Some(pid) = config.pid {
        run_spy_command(pid, &config)?;
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

        let mut command = command.args(&subprocess[1..]);

        if config.capture_output {
            command = command.stdin(std::process::Stdio::null())
                .stdout(process_output.reopen()?)
                .stderr(process_output.reopen()?)
        }

        let mut command = command.spawn()
            .map_err(|e| format_err!("Failed to create process '{}': {}", subprocess[0], e))?;

        #[cfg(target_os="macos")]
        {
            // sleep just in case: https://jvns.ca/blog/2018/01/28/mac-freeze/
            std::thread::sleep(Duration::from_millis(50));
        }
        let result = run_spy_command(command.id() as _, &config);

        // check exit code of subprocess
        std::thread::sleep(Duration::from_millis(1));
        let success =  match command.try_wait()? {
            Some(exit) => exit.success(),
            // if process hasn't finished, assume success
            None => true
        };

        // if we failed for any reason, dump out stderr from child process here
        // (could have useful error message)
        if config.capture_output && (!success || result.is_err()) {
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
    env_logger::builder().format_timestamp_nanos().try_init().unwrap();

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
