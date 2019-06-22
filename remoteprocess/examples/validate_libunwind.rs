/* Simple test to validate that the unwinding done by our gimli based unwinder
is exactly the same as that from libunwind-ptrace.

This uses both unwinders, and compares the instruction pointer and
stack pointer for each frame and make sure they match up */
extern crate remoteprocess;
extern crate env_logger;

#[cfg(all(target_os="linux", unwind))]
extern crate goblin;
#[cfg(all(target_os="linux", unwind))]
extern crate nix;

#[cfg(all(target_os="linux", unwind))]
#[macro_use]
extern crate log;


#[cfg(all(target_os="linux", unwind))]
fn libunwind_compare(pid: remoteprocess::Pid) -> Result<(), remoteprocess::Error> {
    let process = remoteprocess::Process::new(pid)?;
    let unwinder = process.unwinder()?;
    let libunwinder = remoteprocess::LibUnwind::new()?;

    let _lock = process.lock()?;

    let thread = remoteprocess::Thread::new(pid);

    let mut gimli_cursor = unwinder.cursor(&thread)?;
    let mut libunwind_cursor = libunwinder.cursor(pid)?;

    loop {
        match libunwind_cursor.next() {
            Some(lip) => {
                let ip = match gimli_cursor.next() {
                    None => { panic!("gimli cursor exitted before libunwind"); },
                    Some(Err(e)) => { panic!("gimli cursor errored {:?}", e); },
                    Some(Ok(ip)) => ip
                };

                let lip = lip?;
                if libunwind_cursor.sp()? != gimli_cursor.sp() {
                    panic!("gimli sp 0x{:016x} != libunwind sp 0x{:016x}", gimli_cursor.sp(), libunwind_cursor.sp()?);
                }
                if lip != ip {
                    panic!("gimli ip 0x{:016x} != libunwind ip 0x{:016x}", ip, lip);
                }

                info!("ip 0x{:016x} sp 0x{:016x}", ip, gimli_cursor.sp());
            }
            None => {
                if let Some(state) = gimli_cursor.next() {
                    panic!("libunwind cursor is finished, but gimli cursor produced: {:#?}", state);
                } else {
                    break;
                }
            }
        }
    }

    Ok(())
}

#[cfg(all(target_os="linux", unwind))]
fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();

    let pid = if args.len() > 1 {
        args[1].parse().expect("invalid pid")
    } else {
        std::process::id()
    };

    libunwind_compare(pid as i32).unwrap();
}

#[cfg(not(all(target_os="linux", unwind)))]
fn main() {
    panic!("This example only works on linux built with unwinding support");
}
