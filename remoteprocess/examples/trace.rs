extern crate remoteprocess;
extern crate env_logger;
extern crate goblin;
#[cfg(target_os="linux")]
extern crate nix;

#[cfg(unwind)]
fn get_backtrace(pid: remoteprocess::Pid) -> Result<(), remoteprocess::Error> {
    // Create a new handle to the process
    let process = remoteprocess::Process::new(pid)?;
    // Create a stack unwind object, and use it to get the stack for each thread
    let unwinder = process.unwinder()?;
    for thread in process.threads()?.iter() {
        println!("Thread {} - {}", thread.id()?, if thread.active()? { "running" } else { "idle" });

        // lock the thread to get a consistent snapshot (unwinding will fail otherwise)
        // Note: the thread will appear idle when locked, so wee are calling
        // thread.active() before this
        let _lock = thread.lock()?;

        // Iterate over the callstack for the current thread
        for ip in unwinder.cursor(&thread)? {
            let ip = ip?;

            // Lookup the current stack frame containing a filename/function/linenumber etc
            // for the current address
            unwinder.symbolicate(ip, true, &mut |sf| {
                println!("\t{}", sf);
            })?;
        }
    }
    Ok(())
}

#[cfg(unwind)]
fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();

    let pid = if args.len() > 1 {
        args[1].parse().expect("invalid pid")
    } else {
        std::process::id()
    };

    if let Err(e) = get_backtrace(pid as remoteprocess::Pid) {
        println!("Failed to get backtrace {:?}", e);
    }
}

#[cfg(not(unwind))]
fn main() {
    panic!("unwind not supported!");
}

