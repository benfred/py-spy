#[macro_use]
extern crate clap;
extern crate console;
extern crate ctrlc;
extern crate env_logger;
#[macro_use]
extern crate failure;
extern crate goblin;
extern crate indicatif;
#[macro_use]
extern crate lazy_static;
extern crate libc;
#[cfg(target_os = "macos")]
extern crate libproc;
#[cfg(target_os = "macos")]
extern crate mach;
#[cfg(target_os = "linux")]
extern crate nix;
#[macro_use]
extern crate log;
extern crate memmap;
extern crate proc_maps;
extern crate benfred_read_process_memory as read_process_memory;
extern crate regex;
extern crate tempdir;
extern crate tempfile;
#[cfg(unix)]
extern crate termios;
#[cfg(windows)]
extern crate winapi;

mod config;
mod binary_parser;
mod python_bindings;
mod python_interpreters;
mod python_spy;
mod stack_trace;
mod console_viewer;
mod flamegraph;
mod process;
mod utils;

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use failure::Error;

use python_spy::PythonSpy;
use stack_trace::StackTrace;
use console_viewer::ConsoleViewer;

fn print_traces(traces: &[StackTrace], show_idle: bool) {
    for trace in traces {
        if !show_idle && !trace.active {
            continue;
        }

        println!("Thread {:#X} ({})", trace.thread_id, trace.status_str());
        for frame in &trace.frames {
            let filename = match &frame.short_filename { Some(f) => &f, None => &frame.filename };
            println!("\t {} ({}:{})", frame.name, filename, frame.line);
        }
    }
}

// Given a failure::Error, tries to see if it is because the process exitted
fn process_exitted(err: &Error) -> bool {
    err.iter_chain().any(|cause| {
        if let Some(ioerror) = cause.downcast_ref::<std::io::Error>() {
            if let Some(err_code) = ioerror.raw_os_error() {
                if err_code == 3 || err_code == 60 || err_code == 299 {
                    return true;
                }
            }
        }
        false
    })
}

#[cfg(unix)]
fn permission_denied(err: &Error) -> bool {
    err.iter_chain().any(|cause| {
        if let Some(ioerror) = cause.downcast_ref::<std::io::Error>() {
            ioerror.kind() == std::io::ErrorKind::PermissionDenied
        } else {
            false
        }
    })
}

fn sample_console(process: &PythonSpy,
                  display: &str,
                  config: &config::Config) -> Result<(), Error> {
    let rate = config.sampling_rate;
    let mut console = ConsoleViewer::new(config.show_line_numbers, display,
                                         &format!("{}", process.version),
                                         1.0 / rate as f64)?;
    let mut exitted_count = 0;

    for sleep in utils::Timer::new(Duration::from_nanos(1_000_000_000 / rate)) {
        if let Err(elapsed) = sleep {
            console.increment_late_sample(elapsed);
        }

        match process.get_stack_traces() {
            Ok(traces) => {
                console.increment(&traces)?;
            },
            Err(err) => {
                if process_exitted(&err) {
                    exitted_count += 1;
                    if exitted_count > 5 {
                        println!("\nprocess {} ended", process.pid);
                        break;
                    }
                } else {
                    console.increment_error(&err);
                }
            }
        }

    }
    Ok(())
}


fn sample_flame(process: &PythonSpy, filename: &str, stacks: bool,
                config: &config::Config) -> Result<(), Error> {
    let max_samples = config.duration * config.sampling_rate;

    let mut flame = flamegraph::Flamegraph::new(config.show_line_numbers);
    use indicatif::ProgressBar;
    let progress = ProgressBar::new(max_samples);

    println!("Sampling process {} times a second for {} seconds. Press Control-C to exit.",
             config.sampling_rate, config.duration);

    let mut errors = 0;
    let mut samples = 0;
    let mut exitted_count = 0;
    println!();

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    let mut exit_message = "";

    for sleep in utils::Timer::new(Duration::from_nanos(1_000_000_000 / config.sampling_rate)) {
        if let Err(delay) = sleep {
            if delay > Duration::from_secs(1) {
                    // TODO: once this available on crates.io https://github.com/mitsuhiko/indicatif/pull/41
                    // go progress.println instead
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
                flame.increment(&traces)?;
                samples += 1;
                if samples >= max_samples {
                    break;
                }
            },
            Err(err) => {
                if process_exitted(&err) {
                    exitted_count += 1;
                    // there must be a better way to figure out if the process is still running
                    if exitted_count > 3 {
                        exit_message = "Stopped sampling because the process ended";
                        break;
                    }
                }
                errors += 1;
            }
        }
        progress.inc(1);
    }
    progress.finish();
    // write out a message here (so as not to interfere with progress bar) if we ended earlier
    if exit_message.len() > 0 {
        println!("{}", exit_message);
    }

    let out_file = std::fs::File::create(filename)?;
    flame.write(out_file, stacks)?;
    if stacks {
        println!("Wrote intermediate stacks '{}'. Samples: {} Errors: {}", filename, samples, errors);
    } else {
        println!("Wrote flame graph '{}'. Samples: {} Errors: {}", filename, samples, errors);

        // open generated flame graph in the browser on OSX (theory being that on linux
        // you might be SSH'ed into a server somewhere and this isn't desired, but on
        // that is pretty unlikely for osx) (note to self: xdg-open will open on linux)
        #[cfg(target_os = "macos")]
        std::process::Command::new("open").arg(filename).spawn()?;
    }

    Ok(())
}

fn pyspy_main() -> Result<(), Error> {
    let config = config::Config::from_commandline()?;

    #[cfg(target_os="macos")]
    {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("This program requires root on OSX.");
            eprintln!("Try running again with elevated permissions by going 'sudo !!'");
            std::process::exit(1)
        }
    }


    if let Some(pid) = config.pid {
        let process = PythonSpy::retry_new(pid, &config, 3)?;
        if config.dump {
            print_traces(&process.get_stack_traces()?, true);
        } else if let Some(ref flame_file) = config.flame_file_name {
            sample_flame(&process, &flame_file, false, &config)?;
        } else if let Some(ref stacks_file) = config.stacks_file_name {
            sample_flame(&process, &stacks_file, true, &config)?;
        } else {
            sample_console(&process, &format!("pid: {}", pid), &config)?;
        }
    }

    else if let Some(ref subprocess) = config.python_program {
        // Dump out stdout/stderr from the process to a temp file, so we can view it later if needed
        let mut process_output = tempfile::NamedTempFile::new()?;
        let mut command = std::process::Command::new(&subprocess[0])
            .args(&subprocess[1..])
            .stdin(std::process::Stdio::null())
            .stdout(process_output.reopen()?)
            .stderr(process_output.reopen()?)
            .spawn().map_err(|e| format_err!("Failed to create process '{}': {}", subprocess[0], e))?;

        #[cfg(target_os="macos")]
        {
            // sleep just in case: https://jvns.ca/blog/2018/01/28/mac-freeze/
            std::thread::sleep(Duration::from_millis(50));
        }
        let result = match PythonSpy::retry_new(command.id() as read_process_memory::Pid, &config, 8) {
            Ok(process) => {
                if let Some(ref flame_file) = config.flame_file_name {
                    sample_flame(&process, &flame_file, false, &config)
                } else if let Some(ref stacks_file) = config.stacks_file_name {
                    sample_flame(&process, &stacks_file, true, &config)
                } else {
                    sample_console(&process, &subprocess.join(" "), &config)
                }
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
