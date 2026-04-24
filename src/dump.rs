use std::io::Write;

use anyhow::{Context, Error};
use console::{style, Term};

use crate::config::Config;
use crate::python_spy::PythonSpy;
use crate::stack_trace::StackTrace;

use remoteprocess::Pid;

pub fn print_traces(pid: Pid, config: &Config, parent: Option<Pid>) -> Result<(), Error> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    write_traces(&mut out, pid, config, parent)
}

pub fn write_traces<W: Write>(
    out: &mut W,
    pid: Pid,
    config: &Config,
    parent: Option<Pid>,
) -> Result<(), Error> {
    let mut process = PythonSpy::new(pid, config)?;
    if config.dump_json {
        let traces = process
            .get_stack_traces()
            .context("Failed to get stack traces")?;
        writeln!(out, "{}", serde_json::to_string_pretty(&traces)?)?;
        return Ok(());
    }

    writeln!(
        out,
        "Process {}: {}",
        style(process.pid).bold().yellow(),
        process.process.cmdline()?.join(" ")
    )?;

    writeln!(
        out,
        "Python v{} ({})",
        style(&process.version).bold(),
        style(process.process.exe()?).dim()
    )?;

    if let Some(parentpid) = parent {
        let parentprocess = remoteprocess::Process::new(parentpid)?;
        writeln!(
            out,
            "Parent Process {}: {}",
            style(parentpid).bold().yellow(),
            parentprocess.cmdline()?.join(" ")
        )?;
    }
    writeln!(out)?;
    let traces = process
        .get_stack_traces()
        .context("Failed to get stack traces")?;
    for trace in traces.iter().rev() {
        write_trace(out, trace, true)?;
    }

    if config.subprocesses {
        for (childpid, parentpid) in process
            .process
            .child_processes()
            .expect("failed to get subprocesses")
        {
            let term = Term::stdout();
            let (_, width) = term.size();

            writeln!(out, "\n{}", &style("-".repeat(width as usize)).dim())?;
            // child_processes() returns the whole process tree, since we're recursing here
            // though we could end up printing grandchild processes multiple times. Limit down
            // to just once
            if parentpid == pid {
                write_traces(out, childpid, config, Some(parentpid))?;
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn print_trace(trace: &StackTrace, include_activity: bool) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    // Swallow write errors here to preserve the old println! behaviour.
    let _ = write_trace(&mut out, trace, include_activity);
}

pub fn write_trace<W: Write>(
    out: &mut W,
    trace: &StackTrace,
    include_activity: bool,
) -> Result<(), Error> {
    let thread_id = trace.format_threadid();

    let status = if include_activity {
        format!(" ({})", trace.status_str())
    } else if trace.owns_gil {
        " (gil)".to_owned()
    } else {
        "".to_owned()
    };

    match trace.thread_name.as_ref() {
        Some(name) => {
            writeln!(
                out,
                "Thread {}{}: \"{}\"",
                style(thread_id).bold().yellow(),
                status,
                name
            )?;
        }
        None => {
            writeln!(out, "Thread {}{}", style(thread_id).bold().yellow(), status)?;
        }
    };

    for frame in &trace.frames {
        let filename = match &frame.short_filename {
            Some(f) => f,
            None => &frame.filename,
        };
        if frame.line != 0 {
            writeln!(
                out,
                "    {} ({}:{})",
                style(&frame.name).green(),
                style(&filename).cyan(),
                style(frame.line).dim()
            )?;
        } else {
            writeln!(
                out,
                "    {} ({})",
                style(&frame.name).green(),
                style(&filename).cyan()
            )?;
        }

        if let Some(locals) = &frame.locals {
            let mut shown_args = false;
            let mut shown_locals = false;
            for local in locals {
                if local.arg && !shown_args {
                    writeln!(out, "        {}", style("Arguments:").dim())?;
                    shown_args = true;
                } else if !local.arg && !shown_locals {
                    writeln!(out, "        {}", style("Locals:").dim())?;
                    shown_locals = true;
                }

                let repr = local.repr.as_deref().unwrap_or("?");
                writeln!(out, "            {}: {}", local.name, repr)?;
            }
        }
    }
    Ok(())
}
