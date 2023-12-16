#![allow(clippy::type_complexity)]

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Error;

use remoteprocess::Pid;

use crate::config::Config;
use crate::python_spy::PythonSpy;
use crate::stack_trace::{ProcessInfo, StackTrace};
use crate::timer::Timer;
use crate::version::Version;

pub struct Sampler {
    pub version: Option<Version>,
    rx: Option<Receiver<Sample>>,
    sampling_thread: Option<thread::JoinHandle<()>>,
}

pub struct Sample {
    pub traces: Vec<StackTrace>,
    pub sampling_errors: Option<Vec<(Pid, Error)>>,
    pub late: Option<Duration>,
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
        let (initialized_tx, initialized_rx): (
            Sender<Result<Version, Error>>,
            Receiver<Result<Version, Error>>,
        ) = mpsc::channel();
        let config = config.clone();
        let sampling_thread = thread::spawn(move || {
            // We need to create this object inside the thread here since PythonSpy objects don't
            // have the Send trait implemented on linux
            let mut spy = match PythonSpy::retry_new(pid, &config, 20) {
                Ok(spy) => {
                    if initialized_tx.send(Ok(spy.version.clone())).is_err() {
                        return;
                    }
                    spy
                }
                Err(e) => {
                    initialized_tx.send(Err(e)).unwrap();
                    return;
                }
            };

            for sleep in Timer::new(spy.config.sampling_rate as f64) {
                let mut sampling_errors = None;
                let traces = match spy.get_stack_traces() {
                    Ok(traces) => traces,
                    Err(e) => {
                        if spy.process.exe().is_err() {
                            info!(
                                "stopped sampling pid {} because the process exited",
                                spy.pid
                            );
                            break;
                        }
                        sampling_errors = Some(vec![(spy.pid, e)]);
                        Vec::new()
                    }
                };

                let late = sleep.err();
                if tx
                    .send(Sample {
                        traces,
                        sampling_errors,
                        late,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        let version = initialized_rx.recv()??;
        Ok(Sampler {
            rx: Some(rx),
            version: Some(version),
            sampling_thread: Some(sampling_thread),
        })
    }

    /// Creates a new sampler object that samples any python process in the
    /// process or child processes
    fn new_subprocess_sampler(pid: Pid, config: &Config) -> Result<Sampler, Error> {
        let process = remoteprocess::Process::new(pid)?;

        // Initialize a PythonSpy object per child, and build up the process tree
        let mut spies = HashMap::new();
        let mut retries = 10;
        spies.insert(pid, PythonSpyThread::new(pid, None, config)?);

        loop {
            for (childpid, parentpid) in process.child_processes()? {
                // If we can't create the child process, don't worry about it
                // can happen with zombie child processes etc
                match PythonSpyThread::new(childpid, Some(parentpid), config) {
                    Ok(spy) => {
                        spies.insert(childpid, spy);
                    }
                    Err(e) => {
                        warn!("Failed to open process {}: {}", childpid, e);
                    }
                }
            }

            // wait for all the various python spy objects to initialize, and break out of here
            // if we have one of them started.
            if spies.values_mut().any(|spy| spy.wait_initialized()) {
                break;
            }

            // Otherwise sleep for a short time and retry
            retries -= 1;
            if retries == 0 {
                return Err(format_err!(
                    "No python processes found in process {} or any of its subprocesses",
                    pid
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // Create a new thread to periodically monitor for new child processes, and update
        // the procesess map
        let spies = Arc::new(Mutex::new(spies));
        let monitor_spies = spies.clone();
        let monitor_config = config.clone();
        std::thread::spawn(move || {
            while process.exe().is_ok() {
                match monitor_spies.lock() {
                    Ok(mut spies) => {
                        for (childpid, parentpid) in process
                            .child_processes()
                            .expect("failed to get subprocesses")
                        {
                            if spies.contains_key(&childpid) {
                                continue;
                            }
                            match PythonSpyThread::new(childpid, Some(parentpid), &monitor_config) {
                                Ok(spy) => {
                                    spies.insert(childpid, spy);
                                }
                                Err(e) => {
                                    warn!("Failed to create spy for {}: {}", childpid, e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to acquire lock: {}", e);
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });

        let mut process_info = HashMap::new();

        // Create a new thread to generate samples
        let config = config.clone();
        let (tx, rx): (Sender<Sample>, Receiver<Sample>) = mpsc::channel();
        let sampling_thread = std::thread::spawn(move || {
            for sleep in Timer::new(config.sampling_rate as f64) {
                let mut traces = Vec::new();
                let mut sampling_errors = None;

                let mut spies = match spies.lock() {
                    Ok(current) => current,
                    Err(e) => {
                        error!("Failed to get process tree: {}", e);
                        continue;
                    }
                };

                // Notify all the initialized spies to generate a trace
                for spy in spies.values_mut() {
                    if spy.initialized() {
                        spy.notify();
                    }
                }

                // collect the traces from each python spy if possible
                for spy in spies.values_mut() {
                    match spy.collect() {
                        Some(Ok(mut t)) => traces.append(&mut t),
                        Some(Err(e)) => {
                            let errors = sampling_errors.get_or_insert_with(Vec::new);
                            errors.push((spy.process.pid, e));
                        }
                        None => {}
                    }
                }

                // Annotate each trace with the process info
                for trace in traces.iter_mut() {
                    let pid = trace.pid;
                    // Annotate each trace with the process info for the current
                    let process = process_info
                        .entry(pid)
                        .or_insert_with(|| get_process_info(pid, &spies).map(|p| Arc::new(*p)));
                    trace.process_info = process.clone();
                }

                // Send the collected info back
                let late = sleep.err();
                if tx
                    .send(Sample {
                        traces,
                        sampling_errors,
                        late,
                    })
                    .is_err()
                {
                    break;
                }

                // If all of our spies have stopped, we're done
                if spies.len() == 0 || spies.values().all(|x| !x.running) {
                    break;
                }
            }
        });

        Ok(Sampler {
            rx: Some(rx),
            version: None,
            sampling_thread: Some(sampling_thread),
        })
    }
}

impl Iterator for Sampler {
    type Item = Sample;
    fn next(&mut self) -> Option<Self::Item> {
        self.rx.as_ref().unwrap().recv().ok()
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.rx = None;
        if let Some(t) = self.sampling_thread.take() {
            t.join().unwrap();
        }
    }
}

struct PythonSpyThread {
    initialized_rx: Receiver<Result<Version, Error>>,
    notify_tx: Sender<()>,
    sample_rx: Receiver<Result<Vec<StackTrace>, Error>>,
    initialized: Option<Result<Version, Error>>,
    pub running: bool,
    notified: bool,
    pub process: remoteprocess::Process,
    pub parent: Option<Pid>,
    pub command_line: String,
}

impl PythonSpyThread {
    fn new(pid: Pid, parent: Option<Pid>, config: &Config) -> Result<PythonSpyThread, Error> {
        let (initialized_tx, initialized_rx): (
            Sender<Result<Version, Error>>,
            Receiver<Result<Version, Error>>,
        ) = mpsc::channel();
        let (notify_tx, notify_rx): (Sender<()>, Receiver<()>) = mpsc::channel();
        let (sample_tx, sample_rx): (
            Sender<Result<Vec<StackTrace>, Error>>,
            Receiver<Result<Vec<StackTrace>, Error>>,
        ) = mpsc::channel();
        let config = config.clone();
        let process = remoteprocess::Process::new(pid)?;
        let command_line = process
            .cmdline()
            .map(|x| x.join(" "))
            .unwrap_or_else(|_| "".to_owned());

        thread::spawn(move || {
            // We need to create this object inside the thread here since PythonSpy objects don't
            // have the Send trait implemented on linux
            let mut spy = match PythonSpy::retry_new(pid, &config, 5) {
                Ok(spy) => {
                    if initialized_tx.send(Ok(spy.version.clone())).is_err() {
                        return;
                    }
                    spy
                }
                Err(e) => {
                    warn!("Failed to profile python from process {}: {}", pid, e);
                    initialized_tx.send(Err(e)).unwrap();
                    return;
                }
            };

            for _ in notify_rx.iter() {
                let result = spy.get_stack_traces();
                if result.is_err() && spy.process.exe().is_err() {
                    info!(
                        "stopped sampling pid {} because the process exited",
                        spy.pid
                    );
                    break;
                }
                if sample_tx.send(result).is_err() {
                    break;
                }
            }
        });
        Ok(PythonSpyThread {
            initialized_rx,
            notify_tx,
            sample_rx,
            process,
            command_line,
            parent,
            initialized: None,
            running: false,
            notified: false,
        })
    }

    fn wait_initialized(&mut self) -> bool {
        match self.initialized_rx.recv() {
            Ok(status) => {
                self.running = status.is_ok();
                self.initialized = Some(status);
                self.running
            }
            Err(e) => {
                // shouldn't happen, but will be ok if it does
                warn!(
                    "Failed to get initialization status from PythonSpyThread: {}",
                    e
                );
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
            }
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
            Ok(_) => {
                self.notified = true;
            }
            Err(_) => {
                self.running = false;
            }
        }
    }

    fn collect(&mut self) -> Option<Result<Vec<StackTrace>, Error>> {
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

fn get_process_info(pid: Pid, spies: &HashMap<Pid, PythonSpyThread>) -> Option<Box<ProcessInfo>> {
    spies.get(&pid).map(|spy| {
        let parent = spy
            .parent
            .and_then(|parentpid| get_process_info(parentpid, spies));
        Box::new(ProcessInfo {
            pid,
            parent,
            command_line: spy.command_line.clone(),
        })
    })
}
