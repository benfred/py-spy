use std::collections::HashMap;
use std::sync::mpsc::{self, Sender, Receiver};
use std::sync::{Mutex, Arc};
use std::time::Duration;
use std::thread;

use failure::Error;

use remoteprocess::Pid;

use crate::timer::Timer;
use crate::python_spy::PythonSpy;
use crate::config::Config;
use crate::stack_trace::StackTrace;
use crate::version::Version;

pub struct Sampler {
    pub version: Option<Version>,
    rx: Receiver<Sample>,
}

pub struct Sample {
    pub traces: Vec<StackTrace>,
    pub sampling_errors: Option<Vec<(Pid, Error)>>,
    pub process_info: Option<Arc<Mutex<HashMap<Pid, ProcessInfo>>>>,
    pub late: Option<Duration>
}

impl Sampler {
    pub fn new(pid: Pid, config: &Config) -> Result<Sampler, Error> {
        if config.subprocesses {
            Self::new_subprocess_sampler(pid, config)
        } else {
            Self::new_sampler(pid, config)
        }
    }

    /// Creates a new sampler object, reading from a single process only
    fn new_sampler(pid: Pid, config: &Config) -> Result<Sampler, Error> {
        let (tx, rx): (Sender<Sample>, Receiver<Sample>) = mpsc::channel();
        let (initialized_tx, initialized_rx): (Sender<Result<Version, Error>>, Receiver<Result<Version, Error>>) = mpsc::channel();
        let config = config.clone();
        thread::spawn(move || {
            // We need to create this object inside the thread here since PythonSpy objects don't
            // have the Send trait implemented on linux
            let mut spy = match PythonSpy::retry_new(pid, &config, 5) {
                Ok(spy) => {
                    if let Err(_) = initialized_tx.send(Ok(spy.version.clone())) {
                        return;
                    }
                    spy
                },
                Err(e) =>  {
                    if initialized_tx.send(Err(e)).is_err() {}
                    return;
                }
            };

            for sleep in Timer::new(spy.config.sampling_rate as f64) {
                let mut sampling_errors = None;
                let traces = match spy.get_stack_traces() {
                    Ok(traces) => traces,
                    Err(e) => {
                        if spy.process.exe().is_err() {
                            info!("stopped sampling pid {} because the process exitted", spy.pid);
                            break;
                        }
                        sampling_errors = Some(vec![(spy.pid, e)]);
                        Vec::new()
                    }
                };

                let late = sleep.err();
                if tx.send(Sample{traces: traces, sampling_errors, process_info: None, late}).is_err() {
                    break;
                }
            }
        });

        let version = initialized_rx.recv()??;
        Ok(Sampler{rx, version: Some(version)})
    }

    /// Creates a new sampler object that samples any python process in the
    /// process or child processes
    fn new_subprocess_sampler(pid: Pid, config: &Config) -> Result<Sampler, Error> {
        // Initialize a PythonSpy object per child, and build up the process tree
        let mut processes = HashMap::new();
        let mut spies = HashMap::new();
        processes.insert(pid, ProcessInfo::new(pid, None)?);
        spies.insert(pid, PythonSpyThread::new(pid, &config));
        let process = remoteprocess::Process::new(pid)?;
        for (childpid, parentpid) in process.child_processes()? {
            spies.insert(childpid, PythonSpyThread::new(childpid, &config));
            // If we can't create the child process, don't worry about it
            // can happen with zombie child processes etc
            match ProcessInfo::new(childpid, Some(parentpid)) {
                Ok(process) => { processes.insert(childpid, process); }
                Err(e) => { warn!("Failed to open process {}: {}", childpid, e); }
            };
        }

        // wait for all the various python spy objects to initialize, and if none
        // of them initialize appropiately fail right away
        if spies.values_mut().all(|spy| !spy.wait_initialized()) {
            return Err(format_err!("No python processes found in process {} or any of its subprocesses", pid));
        }

        // Create a new thread to periodically monitor for new child processes, and update
        // the procesess map
        let processes = Arc::new(Mutex::new(processes));
        let monitor_processes = processes.clone();
        std::thread::spawn(move || {
            while process.exe().is_ok() {
                match monitor_processes.lock() {
                    Ok(mut processes) => {
                        for (childpid, parentpid) in process.child_processes().expect("failed to get subprocesses") {
                            if processes.contains_key(&childpid) {
                                continue;
                            }
                            match ProcessInfo::new(childpid, Some(parentpid)) {
                                Ok(process) => { processes.insert(childpid, process); }
                                Err(e) => { warn!("Failed to open process {}: {}", childpid, e);  }
                            };
                        }
                    },
                    Err(e) => { error!("Failed to acquire lock: {}", e); }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });

        // Create a new thread to generate samples
        let config = config.clone();
        let (tx, rx): (Sender<Sample>, Receiver<Sample>) = mpsc::channel();
        std::thread::spawn(move || {
            for sleep in Timer::new(config.sampling_rate as f64) {
                let mut traces = Vec::new();
                let mut sampling_errors = None;
                let mut current = match processes.lock() {
                    Ok(current) => current,
                    Err(e) => {
                        error!("Failed to get process tree: {}", e);
                        continue;
                    }
                };

                // Notify all the initialized spies to generate a trace
                for process_info in current.values_mut() {
                    let pid = process_info.pid;
                    let spy = spies.entry(pid).or_insert_with(|| PythonSpyThread::new(pid, &config));
                    if spy.initialized() {
                        spy.notify();
                    }
                }

                // collect the traces from each python spy if possible
                for process_info in current.values_mut() {
                    if let Some(spy) = spies.get_mut(&process_info.pid) {
                        match spy.collect() {
                            Some(Ok(mut t)) => traces.append(&mut t),
                            Some(Err(e)) => {
                                let errors = sampling_errors.get_or_insert_with(|| Vec::new());
                                errors.push((process_info.pid, e));
                            },
                            None => {}
                        }
                    }
                }

                // Send the collected info back
                let process_info = Some(processes.clone());
                let late = sleep.err();
                if tx.send(Sample{traces, sampling_errors, late, process_info}).is_err() {
                    break;
                }

                // remove dead processes from the map, and check after removal
                // if we have any python processes left
                current.retain(|_, x| x.process.exe().is_ok());
                spies.retain(|pid, _| current.contains_key(pid));
                if spies.values().all(|x| !x.running) {
                    break;
                }
            }
        });
        Ok(Sampler{rx, version: None})
    }
}

impl Iterator for Sampler {
    type Item = Sample;
    fn next(&mut self) -> Option<Self::Item> {
        self.rx.recv().ok()
    }
}

pub struct ProcessInfo {
    pub pid: Pid,
    pub ppid: Option<Pid>,
    pub cmdline: String,
    pub process: remoteprocess::Process
}

impl ProcessInfo {
    fn new(pid: Pid, ppid: Option<Pid>) -> Result<ProcessInfo, Error> {
        let process = remoteprocess::Process::new(pid)?;
        let cmdline = process.cmdline().map(|x| x.join(" ")).unwrap_or("".to_owned());
        Ok(ProcessInfo{pid, ppid, cmdline, process})
    }
}

struct PythonSpyThread {
    initialized_rx: Receiver<Result<Version, Error>>,
    notify_tx: Sender<()>,
    sample_rx: Receiver<Result<Vec<StackTrace>, Error>>,
    initialized: Option<Result<Version, Error>>,
    pub running: bool,
    notified: bool,
}

impl PythonSpyThread {
    fn new(pid: Pid, config: &Config) -> PythonSpyThread {
        let (initialized_tx, initialized_rx): (Sender<Result<Version, Error>>, Receiver<Result<Version, Error>>) = mpsc::channel();
        let (notify_tx, notify_rx): (Sender<()>, Receiver<()>) = mpsc::channel();
        let (sample_tx, sample_rx): (Sender<Result<Vec<StackTrace>, Error>>, Receiver<Result<Vec<StackTrace>, Error>>) = mpsc::channel();
        let config = config.clone();
        thread::spawn(move || {
            // We need to create this object inside the thread here since PythonSpy objects don't
            // have the Send trait implemented on linux
            let mut spy = match PythonSpy::retry_new(pid, &config, 5) {
                Ok(spy) => {
                    if let Err(_) = initialized_tx.send(Ok(spy.version.clone())) {
                        return;
                    }
                    spy
                },
                Err(e) =>  {
                    warn!("Failed to profile python from process {}: {}", pid, e);
                    if initialized_tx.send(Err(e)).is_err() {}
                    return;
                }
            };

            for _ in notify_rx.iter() {
                let result = spy.get_stack_traces();
                if let Err(_) = result {
                    if spy.process.exe().is_err() {
                        info!("stopped sampling pid {} because the process exitted", spy.pid);
                        break;
                    }
                }
                if sample_tx.send(result).is_err() {
                    break;
                }
            }
        });
        PythonSpyThread{initialized_rx, notify_tx, sample_rx, initialized: None, running: false, notified: false}
    }

    fn wait_initialized(&mut self) -> bool  {
        match self.initialized_rx.recv() {
            Ok(status) => {
                self.running = status.is_ok();
                self.initialized = Some(status);
                self.running
            },
            Err(e) => {
                // shouldn't happen, but will be ok if it does
                warn!("Failed to get initialization status from PythonSpyThread: {}", e);
                false
            }
        }
    }

    fn initialized(&mut self) -> bool {
        if let Some(init) = self.initialized.as_ref() {
            return init.is_ok();
        }
        match self.initialized_rx.try_recv() {
            Ok(status) => {
                self.running = status.is_ok();
                self.initialized = Some(status);
                self.running
            },
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // this *shouldn't* happen
                warn!("Failed to get initialization status from PythonSpyThread: disconnected");
                false
            }
        }
    }

    fn notify(&mut self) {
        match self.notify_tx.send(()) {
            Ok(_) => { self.notified = true; },
            Err(_) => { self.running = false; }
        }
    }

    fn collect(&mut self) -> Option<Result<Vec<StackTrace>, Error>>  {
        if !self.notified {
            return None;
        }
        self.notified = false;
        match self.sample_rx.recv() {
            Ok(sample) => Some(sample),
            Err(_) => {
                self.running = false;
                None
            }
        }
    }
}