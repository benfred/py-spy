use console::style;
use failure::Error;

use crate::config::Config;
use crate::python_spy::PythonSpy;

use remoteprocess::Pid;

pub fn print_traces(pid: Pid, config: &Config) -> Result<(), Error> {
    let mut process = PythonSpy::new(pid, config)?;

    println!("Process {}: {}",
             style(process.pid).bold().yellow(),
             process.process.cmdline()?.join(" "));

    println!("Python v{} ({})\n",
             style(&process.version).bold(),
             style(process.process.exe()?).dim());

    for i in 1..10 {
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
                let filename = match &frame.short_filename {
                    Some(f) => &f,
                    None => &frame.filename
                };
                if frame.line != 0 {
                    if filename.ends_with(config.trace_line.as_ref().unwrap()) {
                        println!("    {} ({}:{})", style(&frame.name).green(), style(&filename).cyan(), style(frame.line).dim());
                    } else {
                        // println!("    {}", style(&config.trace_line).green());
                        continue
                        // println!("    {} ({}:{})", style(&frame.name).green(), style(&filename).cyan(), style(frame.line).dim());
                    }
                } else {
                    println!("    {} ({})", style(&frame.name).green(), style(&filename).cyan());
                }

                if let Some(locals) = &frame.locals {
                    let mut shown_args = false;
                    let mut shown_locals = false;
                    for local in locals {
                        if local.arg && !shown_args {
                            println!("        {}:", style("Arguments:").dim());
                            shown_args = true;
                        } else if !local.arg && !shown_locals {
                            println!("        {}:", style("Locals:").dim());
                            shown_locals = true;
                        }

                        let repr = local.repr.as_ref().map(String::as_str).unwrap_or("?");
                        if local.name == "a" {
                            println!("            {}: {}", local.name, repr);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
