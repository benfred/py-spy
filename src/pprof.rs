pub mod profile {
    include!(concat!(env!("OUT_DIR"), "/perftools.profiles.rs"));
}

use std::io::Write;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Error;
use flate2::write::GzEncoder;
use flate2::Compression;
use prost::Message;

use crate::pprof::profile::Function;
use crate::pprof::profile::Label;
use crate::pprof::profile::Line;
use crate::pprof::profile::Location;
use crate::pprof::profile::Profile;
use crate::pprof::profile::Sample;
use crate::pprof::profile::ValueType;
use crate::stack_trace::Frame;
use crate::stack_trace::StackTrace;

pub struct Pprof {
    profile: Profile,
    start_instant: Instant,
    start_system: SystemTime,
    // previous_samples_to_sample_idx: HashMap<SampleKey, i64>, // TODO(torshepherd) don't make every stacktrace unique, batch them to save space.
    gzip_profile: bool,
    command_line: String,

    current_function_id: u64,
    current_location_id: u64,
}

enum LabelValue<'a> {
    Str(String),
    Num { value: i64, unit: &'a str },
}

impl<'a> LabelValue<'a> {
    pub fn unitless(value: i64) -> Self {
        LabelValue::Num { value, unit: "" }
    }
}

impl Pprof {
    fn add_string(&mut self, s: &str) -> i64 {
        if let Some(index) = self.profile.string_table.iter().position(|x| x == s) {
            return index as i64;
        }
        self.profile.string_table.push(s.to_string());
        (self.profile.string_table.len() - 1) as i64
    }

    fn add_label(&mut self, sample: &mut Sample, key: &str, value: LabelValue) {
        let mut label = Label::default();
        label.key = self.add_string(key);
        match value {
            LabelValue::Str(str) => label.str = self.add_string(&str),
            LabelValue::Num { value, unit } => {
                label.num = value;
                label.num_unit = self.add_string(unit)
            }
        }
        sample.label.push(label);
    }

    fn add_location(&mut self, frame: &Frame) -> u64 {
        // TODO(torshepherd) add Function caching as well

        // Determine the function name to use. We rename <module> and raw addresses as the shortened
        // filename to make pprof more intuitive and helpful at a glance. Consistent filenames also
        // help with difference-mode pprofs.
        let function_name: &str = if frame.name == "<module>" || frame.name.starts_with("0x") {
            frame.short_filename.as_deref().unwrap_or(&frame.filename)
        } else {
            &frame.name
        };

        let function = Function {
            id: {
                self.current_function_id += 1;
                self.current_function_id
            },
            name: self.add_string(function_name),
            system_name: self.add_string(""),
            filename: self.add_string(&frame.filename),
            start_line: 0,
        };
        self.profile.function.push(function);

        let location = Location {
            id: {
                self.current_location_id += 1;
                self.current_location_id
            },
            mapping_id: 0,
            address: 0,
            line: vec![Line {
                function_id: self.current_function_id,
                line: frame.line as i64,
                column: 0,
            }],
            is_folded: false,
        };
        self.profile.location.push(location);

        self.current_location_id
    }

    pub fn new(gzip_profile: bool) -> Pprof {
        let command_line = std::env::args().collect::<Vec<String>>().join(" ");
        let mut pprof = Pprof {
            profile: Profile::default(),
            start_instant: Instant::now(),
            start_system: SystemTime::now(),
            // previous_samples_to_sample_idx: HashMap::default(),
            gzip_profile,
            command_line,
            current_function_id: 1,
            current_location_id: 1,
        };

        // First index should always be empty string
        pprof.add_string("");

        // Set the sample type
        let r#type = pprof.add_string("py-spy periodic oncpu"); // TODO(torshepherd) is this a good name?
        let unit = pprof.add_string("count");
        pprof.profile.doc_url = pprof.add_string("https://github.com/benfred/py-spy");
        pprof.profile.sample_type.push(ValueType { r#type, unit });
        pprof.profile.default_sample_type = r#type;

        pprof
    }

    pub fn increment(&mut self, trace: &StackTrace) -> std::io::Result<()> {
        // // First, look up if we already have this stack in the profile.
        // if let Some(sample_idx) = self.previous_samples_to_sample_idx.get(trace) {
        //     self.profile.sample[sample_idx]?.value
        // }

        let mut sample = Sample::default();
        sample.value.push(1);
        self.add_label(&mut sample, "pid", LabelValue::unitless(trace.pid as i64));
        // self.add_label(sample, "thread_id", trace.thread_id);
        // self.add_label(sample, "thread_name", trace.thread_name);
        // self.add_label(sample, "os_thread_id", trace.os_thread_id);
        // self.add_label(sample, "active", trace.active);
        // self.add_label(sample, "owns_gil", trace.owns_gil);
        // self.add_label(sample, "command_line", trace.process_info.command_line);
        // self.add_label(sample, "parent_pid", trace.process_info.parent_pid);

        sample.location_id = trace.frames.iter().map(|f| self.add_location(&f)).collect();
        self.profile.sample.push(sample);

        Ok(())
    }

    pub fn write(&self, w: &mut dyn Write) -> Result<(), Error> {
        let mut profile = self.profile.clone(); // TODO(torshepherd) lol this is dumb

        // Set timing information
        profile.time_nanos = self
            .start_system
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_nanos()).ok())
            .unwrap_or(0);
        let dur = Instant::now().duration_since(self.start_instant);
        profile.duration_nanos = i64::try_from(dur.as_nanos()).unwrap_or(0);

        // Add command line invocation as a comment
        // TODO(torshepherd) this is also dumb, duplicating impl of add_string
        if !self.command_line.is_empty() {
            // Find the string index or add it to the string table
            let command_line_idx = if let Some(index) = profile
                .string_table
                .iter()
                .position(|x| x == &self.command_line)
            {
                index as i64
            } else {
                profile.string_table.push(self.command_line.clone());
                (profile.string_table.len() - 1) as i64
            };
            profile.comment.push(command_line_idx);
        }

        // Serialize the protobuf
        let bytes = profile.encode_to_vec();
        if self.gzip_profile {
            let mut encoder = GzEncoder::new(w, Compression::default());
            encoder.write_all(&bytes)?;
            encoder.finish()?;
        } else {
            w.write_all(&bytes)?;
        }

        Ok(())
    }
}
