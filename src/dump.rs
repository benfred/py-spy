use anyhow::Error;
use console::{style, Term};

use crate::config::Config;
use crate::python_spy::PythonSpy;
use crate::stack_trace::StackTrace;

use remoteprocess::Pid;

fn get_stack_traces_with_config(pid: Pid, config: &Config) -> Result<Vec<StackTrace>, Error> {
    let mut process = PythonSpy::new(pid, config)?;
    let mut traces = process.get_stack_traces()?;
    if config.subprocesses {
        let sub_results: Result<Vec<Vec<StackTrace>>, Error> = process
            .process
            .child_processes()
            .expect("failed to get subprocesses")
            .into_iter()
            .filter_map(|(cpid, ppid)| {
                // child_processes() returns the whole process tree, since we're recursing here
                // though we could end up printing grandchild processes multiple times. Limit down
                // to just once
                if ppid == pid {
                    Some(get_stack_traces_with_config(cpid, config))
                } else {
                    None
                }
            })
            .collect();
        let mut subtraces = sub_results?.into_iter().flatten().collect();
        traces.append(&mut subtraces);
    }

    Ok(traces)
}

pub fn print_traces(pid: Pid, config: &Config, parent: Option<Pid>) -> Result<(), Error> {
    let mut process = PythonSpy::new(pid, config)?;
    if config.dump_json {
        let traces = get_stack_traces_with_config(pid, config)?;
        println!("{}", serde_json::to_string_pretty(&traces)?);
        return Ok(());
    }

    println!(
        "Process {}: {}",
        style(process.pid).bold().yellow(),
        process.process.cmdline()?.join(" ")
    );

    println!(
        "Python v{} ({})",
        style(&process.version).bold(),
        style(process.process.exe()?).dim()
    );

    if let Some(parentpid) = parent {
        let parentprocess = remoteprocess::Process::new(parentpid)?;
        println!(
            "Parent Process {}: {}",
            style(parentpid).bold().yellow(),
            parentprocess.cmdline()?.join(" ")
        );
    }
    println!();
    let traces = process.get_stack_traces()?;
    for trace in traces.iter().rev() {
        print_trace(trace, true);
        if config.subprocesses {
            for (childpid, parentpid) in process
                .process
                .child_processes()
                .expect("failed to get subprocesses")
            {
                let term = Term::stdout();
                let (_, width) = term.size();

                println!("\n{}", &style("-".repeat(width as usize)).dim());
                // child_processes() returns the whole process tree, since we're recursing here
                // though we could end up printing grandchild processes multiple times. Limit down
                // to just once
                if parentpid == pid {
                    print_traces(childpid, config, Some(parentpid))?;
                }
            }
        }
    }
    Ok(())
}

pub fn print_trace(trace: &StackTrace, include_activity: bool) {
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
            println!(
                "Thread {}{}: \"{}\"",
                style(thread_id).bold().yellow(),
                status,
                name
            );
        }
        None => {
            println!("Thread {}{}", style(thread_id).bold().yellow(), status);
        }
    };

    for frame in &trace.frames {
        let filename = match &frame.short_filename {
            Some(f) => f,
            None => &frame.filename,
        };
        if frame.line != 0 {
            println!(
                "    {} ({}:{})",
                style(&frame.name).green(),
                style(&filename).cyan(),
                style(frame.line).dim()
            );
        } else {
            println!(
                "    {} ({})",
                style(&frame.name).green(),
                style(&filename).cyan()
            );
        }

        if let Some(locals) = &frame.locals {
            let mut shown_args = false;
            let mut shown_locals = false;
            for local in locals {
                if local.arg && !shown_args {
                    println!("        {}", style("Arguments:").dim());
                    shown_args = true;
                } else if !local.arg && !shown_locals {
                    println!("        {}", style("Locals:").dim());
                    shown_locals = true;
                }

                let repr = local.repr.as_deref().unwrap_or("?");
                println!("            {}: {}", local.name, repr);
            }
        }
    }
}
