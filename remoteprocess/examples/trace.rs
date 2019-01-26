extern crate remoteprocess;
extern crate env_logger;
extern crate goblin;
#[cfg(target_os="linux")]
extern crate nix;


fn get_backtrace(pid: remoteprocess::Pid) -> Result<(), remoteprocess::Error> {
    // Create a new handle to the process
    let process = remoteprocess::Process::new(pid)?;

    // lock the process to get a consistent snapshot. Unwinding will fail otherwise
    let _lock = process.lock()?;

    // Create a stack unwind object, and use it to get the stack for each thread
    let unwinder = process.unwinder()?;
    for (i, thread) in process.threads()?.iter().enumerate() {
        let thread = *thread;
        println!("Thread {} ({})", i, thread);

        /* TODO: cross pross thread status
        let threadid = get_thread_identifier_info(thread)?;
        let threadstatus = get_thread_basic_info(thread)?;
        println!("status: {:?} id {:?}", threadstatus, threadid);
        */

        // Iterate over the callstack for the current thread
        for ip in unwinder.cursor(thread)? {
            let ip = ip?;

            // Lookup the current stack frame containing a filename/function/linenumber etc
            // for the current address
            unwinder.symbolicate(ip, &mut |sf| {
                println!("{}", sf);
            })?;
        }
    }
    Ok(())
}

fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();

    let pid = if args.len() > 1 {
        args[1].parse().expect("invalid pid")
    } else {
        std::process::id()
    };

    if let Err(e) = get_backtrace(pid as i32) {
        println!("Failed to get backtrace {:?}", e);
    }
}
