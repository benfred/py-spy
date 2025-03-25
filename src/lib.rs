//! py-spy: a sampling profiler for python programs
//!
//! This crate lets you use py-spy as a rust library, and gather stack traces from
//! your python process programmatically.
//!
//! # Example:
//!
//! ```rust,no_run
//! fn print_python_stacks(pid: py_spy::Pid) -> Result<(), anyhow::Error> {
//!     // Create a new PythonSpy object with the default config options
//!     let config = py_spy::Config::default();
//!     let mut process = py_spy::PythonSpy::new(pid, &config)?;
//!
//!     // get stack traces for each thread in the process
//!     let traces = process.get_stack_traces()?;
//!
//!     // Print out the python stack for each thread
//!     for trace in traces {
//!         println!("Thread {:#X} ({})", trace.thread_id, trace.status_str());
//!         for frame in &trace.frames {
//!             println!("\t {} ({}:{})", frame.name, frame.filename, frame.line);
//!         }
//!     }
//!     Ok(())
//! }
//! ```
#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate log;

pub mod binary_parser;
pub mod config;
#[cfg(target_os = "linux")]
pub mod coredump;
#[cfg(feature = "unwind")]
mod cython;
pub mod dump;
#[cfg(feature = "unwind")]
mod native_stack_trace;
mod python_bindings;
mod python_data_access;
mod python_interpreters;
pub mod python_process_info;
pub mod python_spy;
mod python_threading;
pub mod sampler;
pub mod stack_trace;
pub mod timer;
mod utils;
mod version;

pub use config::Config;
pub use python_spy::PythonSpy;
pub use remoteprocess::Pid;
pub use stack_trace::Frame;
pub use stack_trace::StackTrace;
