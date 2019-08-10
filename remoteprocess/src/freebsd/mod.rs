mod kinfo_proc;
mod procstat;
mod ptrace;
mod lock;

mod registers;

use libc::{pid_t, lwpid_t};
use read_process_memory::{CopyAddress, ProcessHandle};

use std::cell::RefCell;
use std::convert::TryInto;
use std::rc::{Rc, Weak};

use super::{ProcessMemory, Error};
use freebsd::lock::ProcessLock;
use freebsd::registers::Registers;

pub type Pid = pid_t;
pub type Tid = lwpid_t;

pub struct Process {
    pub pid: Pid,
    lock: Rc<RefCell<Weak<ProcessLock>>>,
}

pub struct Thread {
    pub tid: lwpid_t,
    pid: pid_t,
    active: bool,
    lock: Rc<RefCell<Weak<ProcessLock>>>,
}

fn process_lock(pid: Pid, container: &RefCell<Weak<ProcessLock>>)
                -> Result<Rc<ProcessLock>, Error> {
    if let Some(ref lock) = Weak::upgrade(&container.borrow_mut()) {
        return Ok(Rc::clone(lock))
    }

    let lock = Rc::new(ProcessLock::new(pid)?);

    container.replace(Rc::downgrade(&lock));

    Ok(lock)
}

impl Process {
    pub fn new(pid: Pid) -> Result<Process, Error> {
        Ok(Process { pid, lock: Rc::new(RefCell::new(Weak::new())) })
    }

    pub fn exe(&self) -> Result<String, Error> {
        let filename = procstat::exe(self.pid)?;
        if filename.is_empty() {
            return Err(
                Error::Other("Failed to get process executable name".into())
            );
        }
        Ok(filename)
    }

    pub fn cwd(&self) -> Result<String, Error> {
        Ok(procstat::cwd(self.pid)?)
    }

    pub fn threads(&self) -> Result<Vec<Thread>, Error> {
        let threads = procstat::threads_info(self.pid)?;
        let result = threads.iter().map(|th| {
            Thread {
                tid: th.ki_tid,
                active: th.ki_stat == 2,
                pid: self.pid,
                lock: Rc::clone(&self.lock),
            }
        });

        Ok(result.collect())
    }

    pub fn lock(&self) -> Result<Rc<ProcessLock>, Error> {
        process_lock(self.pid, &self.lock)
    }

    pub fn unwinder(&self) -> Result<(), Error> {
        unimplemented!("No unwinding yet!")
    }
}

impl Thread {
    pub fn id(&self) -> Result<lwpid_t, Error> {
        Ok(self.tid)
    }

    pub fn active(&self) -> Result<bool, Error> {
        Ok(self.active)
    }

    pub fn lock(&self) -> Result<Rc<ProcessLock>, Error> {
        process_lock(self.pid, &self.lock)
    }

    pub fn registers(&self) -> Result<Registers, Error> {
        let data: Registers = Registers::default();

        let _lock = self.lock()?;

        ptrace::getregs(self.tid, &data as *const _ as _)?;

        Ok(data)
    }
}

impl ProcessMemory for Process {
    fn read(&self, addr: usize, buf: &mut [u8]) -> Result<(), Error> {
        let handle: ProcessHandle = self.pid.try_into()?;
        Ok(handle.copy_address(addr, buf)?)
    }
}


#[cfg(test)]
mod tests {
    use libc::pid_t;

    use std::process::{Child, Command};
    use std::{thread, time};
    use super::Error;

    use super::Process;

    struct DroppableProcess {
        inner: Child,
    }

    impl Drop for DroppableProcess {
        fn drop(&mut self) {
            self.inner.kill();
        }
    }

    /// We'll be tracing Perl programs, since Perl is
    /// installed by default.
    ///  This program spawns 2 threads, 1 active
    const PERL_PROGRAM: &str =r#"
          use threads;
          my $sleeping = async {  sleep; };
          my $running = async { while(true) {} };

          map { $_->join } ($sleeping, $running);
    "#;

    const EXECUTABLE: &str = "/usr/local/bin/perl";
    const CWD: &str = "/usr/local/share";

    fn trace_perl_program(
        program: &str
    ) -> Result<(Process, DroppableProcess), Error> {
        // Let's give perl some time.
        let wait_time = time::Duration::from_millis(50);

        Command::new(EXECUTABLE)
            .current_dir(CWD)
            .args(&["-e", program])
            .spawn()
            .and_then(|child| {
                let pid = child.id() as pid_t;

                let result = (Process::new(pid).unwrap(),
                              DroppableProcess { inner: child });

                thread::sleep(wait_time);

                Ok(result)
            })
            .map_err(|err| err.into())
    }

    #[test]
    fn test_threads() {
        let threads = trace_perl_program(PERL_PROGRAM)
            .and_then(|(process, _p)| process.threads())
            .expect("test failed!");

        let active_count = threads.iter().filter(|x| {
            x.active().unwrap()
        }).count();

        assert_eq!(threads.len(), 3); // 1 main thread, 2 spawned.
        assert_eq!(active_count, 1);

    }

    #[test]
    fn test_thread_lock_unlock() {
        trace_perl_program(PERL_PROGRAM)
            .and_then(|(process, _p)| {
                let threads = process.threads()?;

                let active_thread =
                    threads.iter().find(|x| x.active().unwrap());

                assert!(active_thread.is_some());

                if let Some(thread) = active_thread {
                    let _lock = thread.lock();

                    let threads = process.threads()?;

                    let active_thread =
                        threads.iter().find(|x| x.active().unwrap());

                    assert!(active_thread.is_none());
                }

                let threads = process.threads()?;

                let active_thread =
                    threads.iter().find(|x| x.active().unwrap());

                assert!(active_thread.is_some());

                Ok(())
            })
            .expect("test failed!");
    }

    #[test]
    fn test_exe() {
        trace_perl_program(PERL_PROGRAM)
            .and_then(|(process, _p)| {
                assert_eq!(process.exe()?, EXECUTABLE);

                Ok(())
            });
    }

    #[test]
    fn test_cwd() {
        trace_perl_program(PERL_PROGRAM)
            .and_then(|(process, _p)| {
                assert_eq!(process.cwd()?, CWD);

                Ok(())
            });
    }


    #[test]
    fn test_process_lock() {
        trace_perl_program(PERL_PROGRAM)
            .and_then(|(process, _p)| {
                let threads = process.threads()?;

                let active_thread =
                    threads.iter().find(|x| x.active().unwrap());

                assert!(active_thread.is_some());

                if let Some(thread) = active_thread {
                    let _lock = process.lock();

                    let threads = process.threads()?;

                    let active_thread =
                        threads.iter().find(|x| x.active().unwrap());

                    assert!(active_thread.is_none());
                }

                let threads = process.threads()?;

                let active_thread =
                    threads.iter().find(|x| x.active().unwrap());

                assert!(active_thread.is_some());

                Ok(())
            })
            .expect("test failed!");
    }


    /// Since threads and their process use the same locking mechanics, it's
    /// crucial to ensure that double-locking doesn't occur. In case of
    /// double-lock program would panic, since ptrace(2) returns EBUSY.
    #[test]
    fn test_process_and_thread_lock() {
        trace_perl_program(PERL_PROGRAM)
            .and_then(|(process, _p)| {
                let threads = process.threads()?;

                let active_thread =
                    threads.iter().find(|x| x.active().unwrap());

                assert!(active_thread.is_some());

                if let Some(thread) = active_thread {
                    let _lock = process.lock()?;
                    let _thread_lock = active_thread.unwrap().lock()?;

                    let threads = process.threads()?;

                    let active_thread =
                        threads.iter().find(|x| x.active().unwrap());

                    assert!(active_thread.is_none());
                }

                Ok(())
            })
            .expect("test failed!");
    }
}
