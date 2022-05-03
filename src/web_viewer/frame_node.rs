use std::collections::HashMap;
use std::vec::Vec;
use std::sync::Arc;
use std::time::Instant;
use log::info;
use failure::Error;

use crate::stack_trace::{StackTrace, Frame, ProcessInfo};
use serde::ser::{self, Serializer, SerializeStruct};
use serde_derive::Serialize;

#[derive(Debug, Serialize)]
pub struct FoldedTraces {
    pub total: u64,
    pub gil: u64,
    pub active: u64,
    pub filtered: u64,
    pub root: FrameNode
}

#[derive(Debug)]
pub struct FrameNode {
    pub count: u64,
    pub frame: Frame,
    pub children: HashMap<String, FrameNode>,
    pub line_numbers: bool
}

#[derive(Debug, Serialize)]
pub struct FrameInfo {
    pub own_count: u64,
    pub total_count: u64,
    pub frame: Frame
}

#[derive(Debug, Serialize)]
pub struct AggregateOptions {
    pub include_processes: bool,
    pub include_threads: bool,
    pub include_lines: bool,
    // TODO: move these two options into an enum
    pub include_idle: bool,
    pub gil_only: bool,
    pub short_filename: Option<String>,
    pub function: Option<String>,
}

impl FoldedTraces {
    pub fn from_traces(traces: &[Arc<StackTrace>], options: &AggregateOptions) -> Result<FoldedTraces, Error> {
        let aggregate_start = Instant::now();
        let mut root = FrameNode::new(Frame{name: "all".to_owned(), filename: "".to_owned(),
                                      short_filename: None, module:None, line: 0, locals: None}, options.include_lines,
                                    );

        // pre aggregate by memory address. since we're interning the objects in data_collector
        // duplicates here should be referring to the same memory address - making this code
        // significantly faster
        let mut trace_counts = HashMap::new();

        let mut filtered = 0;

        for trace in traces {
            if !(options.include_idle || trace.active) {
                filtered += 1;
                continue;
            }

            if options.gil_only && !trace.owns_gil {
                filtered += 1;
                continue;
            }

            let trace_addr = &*trace as &StackTrace as *const StackTrace as usize;
            trace_counts.entry(trace_addr)
                .or_insert_with(|| (trace.clone(), 0)).1 += 1;
        }

        let mut gil = 0;
        let mut active = 0;
        let mut total = 0;

        for (trace, count) in trace_counts.values() {
            if let Some(function) = &options.function {
                if !trace.frames.iter().any(|frame|
                        &frame.name == function && frame.short_filename == options.short_filename) {
                    filtered += count;
                    continue;
                }
            } else if options.short_filename.is_some() {
                if !trace.frames.iter().any(|frame| frame.short_filename == options.short_filename) {
                    filtered += count;
                    continue;
                }
            }

            total += count;
            if trace.active {
                active += count;
            }
            if trace.owns_gil {
                gil += count;
            }
            root.insert_trace(options, trace, *count);
        }

        info!("aggregated {} ({} unique) traces in {:2?} ", traces.len(), trace_counts.len(), Instant::now() - aggregate_start);
        Ok(FoldedTraces{root, total, gil, active, filtered})
    }
}

impl FrameNode {

    fn new(frame: Frame, line_numbers: bool) -> FrameNode {
        FrameNode{count: 0, frame, children: HashMap::new(), line_numbers}
    }

    fn insert_trace(&mut self, options: &AggregateOptions, trace: &StackTrace, count: u64) {
        let frame = if options.include_processes && trace.process_info.is_some() {
            self.insert_process(trace.process_info.as_ref().unwrap(), count)
        } else {
            self
        };
        let frame = if options.include_threads { frame.insert_thread(trace, count) } else { frame };
        frame.insert_frames(&mut trace.frames.iter().rev(), count);
    }

    fn insert_process(&mut self, process: &ProcessInfo, count: u64) -> & mut FrameNode {
        // need to insert parent processes first
        let current = match process.parent.as_ref() {
            Some(parent) => self.insert_process(parent, count),
            None => self
        };

        let line_numbers = current.line_numbers;
        current.count += count;
        current.children
            .entry(format!("process {}", process.pid))
            .or_insert_with(||
                FrameNode::new(Frame{name: format!("process {} \"{}\"", process.pid, process.command_line),
                                        filename: "".to_owned(), short_filename: None,
                                        module:None, line: 0, locals: None}, line_numbers))
    }

    fn insert_thread(&mut self, trace: &StackTrace, count: u64) -> &mut FrameNode {
        let line_numbers = self.line_numbers;
        self.count += count;
        self.children
            .entry(trace.format_threadid())
            .or_insert_with(|| {
                let thread_id = trace.format_threadid();
                let display = match trace.thread_name.as_ref() {
                    Some(name) => format!("Thread {} \"{}\"", thread_id, name),
                    None => format!("Thread {}", thread_id)
                };
                return FrameNode::new(Frame{name: display, filename: "".to_owned(),
                                            short_filename: None, module:None, line: 0,
                                            locals: None},
                                      line_numbers)
            })
    }

    fn insert_frames<'a, I>(&mut self, frames: & mut I, count: u64)
        where I: Iterator<Item = &'a Frame> {
        if let Some(frame) = frames.next() {
            let name = frame.format(self.line_numbers);
            let line_numbers = self.line_numbers;
            self.children.entry(name)
                .or_insert_with(|| FrameNode::new(frame.clone(), line_numbers))
                .insert_frames(frames, count);
        }
        self.count += count;
    }

    pub fn flatten(&self) -> HashMap<String, FrameInfo> {
        let mut ret = HashMap::new();
        let mut parents = Vec::new();
        self._flatten(&mut ret, &mut parents);
        ret
    }

    fn _flatten(&self, values: &mut HashMap<String, FrameInfo>, parents: &mut Vec<String>) {
        let mut own_count = self.count;
        let name = self.frame.format(self.line_numbers);
        parents.push(name);
        for child in self.children.values() {
            own_count -= child.count;
            child._flatten(values, parents);
        }
        let name = parents.pop().unwrap();

        let key = self.frame.format(self.line_numbers);
        let entry = values.entry(key).or_insert_with(|| FrameInfo{frame: self.frame.clone(), own_count: 0, total_count: 0});
        if !parents.iter().any(|x| x == &name) {
            entry.total_count += self.count;
        }
        entry.own_count += own_count;
    }
}

impl ser::Serialize for FrameNode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("FrameNode", 4)?;
        state.serialize_field("frame", &self.frame)?;
        let name = self.frame.format(self.line_numbers);
        state.serialize_field("name", &name)?;
        state.serialize_field("value", &self.count)?;

        let children: Vec<&FrameNode> = self.children
            .values()
            .collect();
        state.serialize_field("children", &children)?;
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(frames: Vec<Frame>) -> Arc<StackTrace> {
        Arc::new(StackTrace{pid: 1234, thread_id: 1234, os_thread_id: None, owns_gil: true,
        active: true, frames, thread_name: None, process_info: None})
    }
    fn frame(name: &str, line: i32) -> Frame {
        Frame{name: name.to_owned(), line, filename: "file.py".to_owned(), short_filename: None, module: None, locals: None}
    }

    #[test]
    fn test_from_traces() {

        let mut options =  AggregateOptions{include_processes: false, include_threads: false,
            include_idle:true, include_lines: true, gil_only: false, short_filename: None, function: None};

        let mut traces = Vec::new();
        traces.push(trace(vec![frame("fn2", 30), frame("fn1", 20), frame("root", 10)]));
        traces.push(trace(vec![frame("fn2", 30), frame("fn1", 20), frame("root", 10)]));
        traces.push(trace(vec![frame("fn2", 35), frame("fn1", 20), frame("root", 10)]));
        traces.push(trace(vec![frame("fn1", 20), frame("root", 10)]));
        let node = FoldedTraces::from_traces(&traces,&options).unwrap().root;

        assert_eq!(node.count, 4);
        assert_eq!(node.children.len(), 1);

        let node = node.children.values().next().unwrap();
        assert_eq!(node.frame.name, "root");
        assert_eq!(node.count, 4);
        assert_eq!(node.children.len(), 1);

        let node = node.children.values().next().unwrap();
        assert_eq!(node.frame.name, "fn1");
        assert_eq!(node.count, 4);
        assert_eq!(node.children.len(), 2);

        let mut nodes: Vec<&FrameNode> = node.children.values().collect();
        nodes.sort_by(|a, b| a.frame.partial_cmp(&b.frame).unwrap());

        let node = nodes[0];
        assert_eq!(node.frame.name, "fn2");
        assert_eq!(node.frame.line, 30);
        assert_eq!(node.count, 2);

        let node = nodes[1];
        assert_eq!(node.frame.name, "fn2");
        assert_eq!(node.frame.line, 35);
        assert_eq!(node.count, 1);

        // Try again with include_threads
        options.include_threads = true;
        let node = FoldedTraces::from_traces(&traces,&options).unwrap().root;
        assert_eq!(node.count, 4);
        assert_eq!(node.children.len(), 1);
        let node = node.children.values().next().unwrap();
        assert!(node.frame.name.starts_with("Thread"));
        assert_eq!(node.count, 4);
        assert_eq!(node.children.len(), 1);
    }
}

