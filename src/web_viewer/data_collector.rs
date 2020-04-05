use std::collections::{HashMap, HashSet};
use std::vec::Vec;
use std::sync::{Mutex, Arc};
use std::sync::mpsc::{self, Sender, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use failure::{Error, format_err};
use log::info;
use serde_derive::Serialize;

use crate::config::Config;
use crate::sampler::Sample;
use crate::stack_trace::{StackTrace};
use super::frame_node::{FrameNode, AggregateOptions};

pub struct Data {
    traces: Vec<Arc<StackTrace>>,
    trace_lookup: HashSet<Arc<StackTrace>>,
    trace_ms: Vec<u64>,
    pub short_filenames: HashMap<String, String>,
    pub stats: ProgramStats,
}

impl Data {
    pub fn new(python_command: &str, version: &str, config: &Config) -> Data {
        let stats = ProgramStats{gil: Vec::new(), threads: Vec::new(),
            python_command: python_command.to_owned(),
            version: version.to_owned(),
            running: true,
            sampling_rate: config.sampling_rate,
            sampling_delay: None,
            subprocesses: config.subprocesses,};
        Data{traces: Vec::new(), trace_ms: Vec::new(), trace_lookup: HashSet::new(), short_filenames: HashMap::new(), stats}
    }

    pub fn aggregate(&self, start_time: u64, end_time: u64, options: &AggregateOptions) -> Result<FrameNode, Error> {
        let start = if start_time > 0 {
            match self.trace_ms.binary_search(&start_time) {
                Ok(v) => v,
                Err(v) => if v > 0 { v - 1 } else { v }
            }
        } else {
            0
        };

        let end = if end_time > 0 {
            match self.trace_ms.binary_search(&end_time) {
                Ok(v) => v,
                Err(v) => if v > 0 { v - 1 } else { v }
            }
        } else {
            self.traces.len() - 1
        };

        if end <= start {
            return Err(format_err!("end_time {} is before start_time {}", end_time, start_time));
        }

        if start >= self.traces.len() || end >= self.traces.len() {
            return Err(format_err!("Invalid trace slice found"));
        }
        FrameNode::from_traces(&self.traces[start..end], options)
    }
}

pub struct TraceCollector {
    // Owns the channel
    tx: Sender<(Sample, u64)>,
    start: Instant,
    pub data: Arc<Mutex<Data>>
}

impl TraceCollector {
    pub fn new(python_command: &str, version: &str, config: &Config) -> Result<TraceCollector, Error> {
        let data = Arc::new(Mutex::new(Data::new(python_command, version, config)));
        let send_data = data.clone();
        let (tx, rx): (Sender<(Sample, u64)>, Receiver<(Sample, u64)>) = mpsc::channel();
        thread::spawn(move || { update_data(rx, send_data); });
        Ok(TraceCollector{start: Instant::now(), tx, data})
    }

    pub fn increment(&mut self, sample: Sample) -> Result<(), Error> {
        let timestamp = Instant::now() - self.start;
        let timestamp_ms = timestamp.as_secs() * 1000 + timestamp.subsec_millis() as u64;
        self.tx.send((sample, timestamp_ms))?;
        Ok(())
    }

    pub fn notify_exitted(&mut self) {
        // TODO: does this belong here?
        self.data.lock().unwrap().stats.running = false;
    }
}

#[derive(Debug, Serialize)]
pub struct ProgramStats {
    // timeseries represented the gil usage (every 100ms)
    gil: Vec<f32>,

    // a bunch of (threadid, timeseries) of activity for each thread (sampled every 100ms)
    threads: Vec<(u64, Vec<f32>)>,

    python_command: String,
    version: String,
    running: bool,
    sampling_rate: u64,
    subprocesses: bool,
    sampling_delay: Option<Duration>
}


fn update_data(rx: Receiver<(Sample, u64)>, send_data: Arc<Mutex<Data>>) {
    let mut current_gil: u64 = 0;
    let mut current: u64 = 0;
    let mut total: u64 = 0;
    let mut threads = HashMap::<u64, u64>::new();
    let mut thread_ids = HashMap::<u64, usize>::new();

    loop {
        match rx.recv() {
            Err(_) => {
                info!("stopped updating data - channel closed");
                return;
            },
            Ok((sample, timestamp_ms)) => {

                for trace in sample.traces {
                    if trace.owns_gil {
                        current_gil += 1;
                    }

                    let active = if trace.active { 1 } else { 0 };
                    *threads.entry(trace.thread_id).or_insert(0) += active;

                    // if we haven't seen this thread, create new timeseries for it
                    thread_ids.entry(trace.thread_id).or_insert_with(|| {
                        let mut data = send_data.lock().unwrap();
                        let thread_index = data.stats.threads.len();
                        let items = data.stats.gil.len();
                        data.stats.threads.push((trace.thread_id, vec![0.0; items]));
                        thread_index
                    });

                    let trace = Arc::new(trace);
                    let mut data = send_data.lock().unwrap();
                    data.stats.sampling_delay = sample.late;

                    let trace = match data.trace_lookup.get(&trace) {
                        Some(trace) => trace.clone(),
                        None => {
                            data.trace_lookup.insert(trace.clone());
                            trace
                        }
                    };

                    // update short_filenames map. this somewhat annoyingly assumes thatshort_filenames are unique
                    // TODO: lets fix this properly
                    for frame in trace.frames.iter() {
                        if let Some(short_filename) = frame.short_filename.as_ref() {
                            if !data.short_filenames.contains_key(short_filename) {
                                data.short_filenames.insert(short_filename.clone(), frame.filename.clone());
                            }
                        }
                    }

                    data.traces.push(trace);
                    data.trace_ms.push(timestamp_ms);
                }
                current += 1;

                // Store statistics as a time series, taking a sample every 100ms
                if total <= timestamp_ms  {
                    total += 100;
                    let mut data = send_data.lock().unwrap();
                    for (thread, active) in threads.iter_mut() {
                        let thread_index = thread_ids[thread];
                        data.stats.threads[thread_index].1.push(*active as f32 / current as f32);
                        *active = 0;
                    }
                    data.stats.gil.push(current_gil as f32 / current as f32);
                    current_gil = 0;
                    current = 0;
                }
            }
        }
    }
}
