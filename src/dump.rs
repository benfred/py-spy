use anyhow::Error;
use console::{Term, style};

use crate::config::Config;
use crate::python_spy::PythonSpy;

use remoteprocess::Pid;

pub fn print_traces(pid: Pid, config: &Config, parent: Option<Pid>) -> Result<(), Error> {
    let mut process = PythonSpy::new(pid, config)?;
    if config.dump_json {
        let traces = process.get_stack_traces()?;
        println!("{}", serde_json::to_string_pretty(&traces)?);
        return Ok(())
    }

    println!("Process {}: {}",
        style(process.pid).bold().yellow(),
        process.process.cmdline()?.join(" "));

    println!("Python v{} ({})",
        style(&process.version).bold(),
        style(process.process.exe()?).dim());

    if let Some(parentpid) = parent {
        let parentprocess = remoteprocess::Process::new(parentpid)?;
        println!("Parent Process {}: {}",
            style(parentpid).bold().yellow(),
            parentprocess.cmdline()?.join(" "));
    }
    println!("");

    let traces = process.get_stack_traces()?;

    for trace in traces.iter().rev() {
        let thread_id = trace.format_threadid();
        match trace.thread_name.as_ref() {
            Some(name) => {
                println!("Thread {} ({}): \"{}\"", style(thread_id).bold().yellow(), trace.status_str(), name);
            }
            None => {
                println!("Thread {} ({})", style(thread_id).bold().yellow(), trace.status_str());
            }
        };

        for frame in &trace.frames {
            let filename = match &frame.short_filename { Some(f) => &f, None => &frame.filename };
            if frame.line != 0 {
                println!("    {} ({}:{})", style(&frame.name).green(), style(&filename).cyan(), style(frame.line).dim());
            } else {
                println!("    {} ({})", style(&frame.name).green(), style(&filename).cyan());
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

                    let repr = local.repr.as_ref().map(String::as_str).unwrap_or("?");
                    println!("            {}: {}", local.name, repr);
                }
            }
        }

        if config.subprocesses {
            for (childpid, parentpid) in process.process.child_processes().expect("failed to get subprocesses") {
                let term = Term::stdout();
                let (_, width) = term.size();

                println!("\n{}", &style("-".repeat(width as usize)).dim());
                // child_processes() returns the whole process tree, since we're recursing here
                // though we could end up printing grandchild processes multiple times. Limit down
                // to just once
                if parentpid == pid {
                    print_traces(childpid, &config, Some(parentpid))?;
                }
            }
        }
    }
    Ok(())
}
