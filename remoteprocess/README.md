remoteprocess
=====

This crate provides a cross platform way of querying information about other processes running on
the system. This let's you build profiling and debugging tools.

Features:

- Suspending the execution of the process
- Getting the process executable name and current working directory
- Listing all the threads in the process
- Figure out if a thread is active or not
- Read memory from the other proceses (using read_proceses_memory crate)
- Getting a stack trace for a thread in the target process
- Resolve symbols for an address in the other process

This crate provides implementations for Linux, OSX, FreeBSD and Windows

## Usage

To show a stack trace from each thread in a program

```rust
fn get_backtrace(pid: remoteprocess::Pid) -> Result<(), remoteprocess::Error> {
    // Create a new handle to the process
    let process = remoteprocess::Process::new(pid)?;

    // lock the process to get a consistent snapshot. Unwinding will fail otherwise
    let _lock = process.lock()?;

    // Create a stack unwind object, and use it to get the stack for each thread
    let unwinder = process.unwinder()?;
    for thread in process.threads()?.iter() {
        println!("Thread {}", thread);

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
```

A complete program with this code can be found in the examples folder.

## Limitations

Currently we only have implementations for getting stack traces on x86_64 processors running
Linux/Windows or OSX. We don't have the abilitiy to get stack traces at all from ARM or i686
processors, or from FreeBSD.

## Credits

This crate heavily relies on the [gimli](https://github.com/gimli-rs/gimli) project. Gimli is an
amazing tool for parsing DWARF debugging information, and we are using it here for looking up filename
and line numbers given an instruction pointeer.