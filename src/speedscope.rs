// This code is adapted from rbspy:
// https://github.com/rbspy/rbspy/tree/master/src/ui/speedscope.rs
// licensed under the MIT License:
/*
MIT License

Copyright (c) 2016 Julia Evans, Kamal Marhubi
Portions (continuous integration setup) Copyright (c) 2016 Jorge Aparicio

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/

use std::collections::HashMap;
use std::io;
use std::io::Write;

use crate::stack_trace;
use remoteprocess::{Pid, Tid};

use anyhow::Error;
use serde_derive::{Deserialize, Serialize};

use crate::config::Config;

/*
 * This file contains code to export rbspy profiles for use in https://speedscope.app
 *
 * The TypeScript definitions that define this file format can be found here:
 * https://github.com/jlfwong/speedscope/blob/9d13d9/src/lib/file-format-spec.ts
 *
 * From the TypeScript definition, a JSON schema is generated. The latest
 * schema can be found here: https://speedscope.app/file-format-schema.json
 *
 * This JSON schema conveniently allows to generate type bindings for generating JSON.
 * You can use https://app.quicktype.io/ to generate serde_json Rust bindings for the
 * given JSON schema.
 *
 * There are multiple variants of the file format. The variant we're going to generate
 * is the "type: sampled" profile, since it most closely maps to rbspy's data recording
 * structure.
 */

#[derive(Debug, Deserialize, Serialize)]
struct SpeedscopeFile {
    #[serde(rename = "$schema")]
    schema: String,
    profiles: Vec<Profile>,
    shared: Shared,

    #[serde(rename = "activeProfileIndex")]
    active_profile_index: Option<f64>,

    exporter: Option<String>,

    name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Profile {
    #[serde(rename = "type")]
    profile_type: ProfileType,

    name: String,
    unit: ValueUnit,

    #[serde(rename = "startValue")]
    start_value: f64,

    #[serde(rename = "endValue")]
    end_value: f64,

    samples: Vec<Vec<usize>>,
    weights: Vec<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Shared {
    frames: Vec<Frame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Frame {
    name: String,
    file: Option<String>,
    line: Option<u32>,
    col: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
enum ProfileType {
    #[serde(rename = "evented")]
    Evented,
    #[serde(rename = "sampled")]
    Sampled,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
enum ValueUnit {
    #[serde(rename = "bytes")]
    Bytes,
    #[serde(rename = "microseconds")]
    Microseconds,
    #[serde(rename = "milliseconds")]
    Milliseconds,
    #[serde(rename = "nanoseconds")]
    Nanoseconds,
    #[serde(rename = "none")]
    None,
    #[serde(rename = "seconds")]
    Seconds,
}

impl SpeedscopeFile {
    pub fn new(
        samples: &HashMap<(Pid, Tid), Vec<Vec<usize>>>,
        frames: &[Frame],
        thread_name_map: &HashMap<(Pid, Tid), String>,
        sample_rate: u64,
    ) -> SpeedscopeFile {
        let mut profiles: Vec<Profile> = samples
            .iter()
            .map(|(thread_id, samples)| {
                let end_value = samples.len();
                // we sample at 100 Hz, so scale the end value and weights to match the time unit
                let scaled_end_value = end_value as f64 / sample_rate as f64;
                let weights: Vec<f64> = samples
                    .iter()
                    .map(|_s| 1_f64 / sample_rate as f64)
                    .collect();

                Profile {
                    profile_type: ProfileType::Sampled,
                    name: thread_name_map
                        .get(thread_id)
                        .map_or_else(|| "py-spy".to_string(), |x| x.clone()),
                    unit: ValueUnit::Seconds,
                    start_value: 0.0,
                    end_value: scaled_end_value,
                    samples: samples.clone(),
                    weights,
                }
            })
            .collect();

        profiles.sort_by(|a, b| a.name.cmp(&b.name));

        SpeedscopeFile {
            // This is always the same
            schema: "https://www.speedscope.app/file-format-schema.json".to_string(),
            active_profile_index: None,
            name: Some("py-spy profile".to_string()),
            exporter: Some(format!("py-spy@{}", env!("CARGO_PKG_VERSION"))),
            profiles,
            shared: Shared {
                frames: frames.to_owned(),
            },
        }
    }
}

impl Frame {
    pub fn new(stack_frame: &stack_trace::Frame, show_line_numbers: bool) -> Frame {
        Frame {
            name: stack_frame.name.clone(),
            // TODO: filename?
            file: Some(stack_frame.filename.clone()),
            line: if show_line_numbers {
                Some(stack_frame.line as u32)
            } else {
                None
            },
            col: None,
        }
    }
}

pub struct Stats {
    samples: HashMap<(Pid, Tid), Vec<Vec<usize>>>,
    frames: Vec<Frame>,
    frame_to_index: HashMap<stack_trace::Frame, usize>,
    thread_name_map: HashMap<(Pid, Tid), String>,
    config: Config,
}

impl Stats {
    pub fn new(config: &Config) -> Stats {
        Stats {
            samples: HashMap::new(),
            frames: vec![],
            frame_to_index: HashMap::new(),
            thread_name_map: HashMap::new(),
            config: config.clone(),
        }
    }

    pub fn record(&mut self, stack: &stack_trace::StackTrace) -> Result<(), io::Error> {
        let show_line_numbers = self.config.show_line_numbers;
        let mut frame_indices: Vec<usize> = stack
            .frames
            .iter()
            .map(|frame| {
                let frames = &mut self.frames;
                let mut key = frame.clone();
                if !show_line_numbers {
                    key.line = 0;
                }
                *self.frame_to_index.entry(key).or_insert_with(|| {
                    let len = frames.len();
                    frames.push(Frame::new(frame, show_line_numbers));
                    len
                })
            })
            .collect();
        frame_indices.reverse();

        let key = (stack.pid as Pid, stack.thread_id as Tid);

        self.samples.entry(key).or_default().push(frame_indices);
        let subprocesses = self.config.subprocesses;
        self.thread_name_map.entry(key).or_insert_with(|| {
            let thread_name = stack
                .thread_name
                .as_ref()
                .map_or_else(|| "".to_string(), |x| x.clone());
            if subprocesses {
                format!(
                    "Process {} Thread {} \"{}\"",
                    stack.pid,
                    stack.format_threadid(),
                    thread_name
                )
            } else {
                format!("Thread {} \"{}\"", stack.format_threadid(), thread_name)
            }
        });

        Ok(())
    }

    pub fn write(&self, w: &mut dyn Write) -> Result<(), Error> {
        let json = serde_json::to_string(&SpeedscopeFile::new(
            &self.samples,
            &self.frames,
            &self.thread_name_map,
            self.config.sampling_rate,
        ))?;
        writeln!(w, "{}", json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Seek, SeekFrom};

    #[test]
    fn test_speedscope_units() {
        let sample_rate = 100;
        let config = Config {
            show_line_numbers: true,
            sampling_rate: sample_rate,
            ..Default::default()
        };
        let mut stats = Stats::new(&config);
        let mut cursor = Cursor::new(Vec::new());

        let frame = stack_trace::Frame {
            name: String::from("test"),
            filename: String::from("test.py"),
            module: None,
            short_filename: None,
            line: 0,
            locals: None,
            is_entry: true,
        };

        let trace = stack_trace::StackTrace {
            pid: 1,
            thread_id: 1,
            thread_name: None,
            os_thread_id: None,
            active: true,
            owns_gil: false,
            frames: vec![frame],
            process_info: None,
        };

        stats.record(&trace).unwrap();
        stats.write(&mut cursor).unwrap();

        cursor.seek(SeekFrom::Start(0)).unwrap();
        let mut s = String::new();
        let read = cursor.read_to_string(&mut s).unwrap();
        assert!(read > 0);
        let trace: SpeedscopeFile = serde_json::from_str(&s).unwrap();

        assert_eq!(trace.profiles[0].unit, ValueUnit::Seconds);
        assert_eq!(trace.profiles[0].end_value, 1.0 / sample_rate as f64);
    }
}
