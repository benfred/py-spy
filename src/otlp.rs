use crate::stack_trace::StackTrace;
use anyhow::Error;
use opentelemetry_proto::tonic::collector::profiles::v1development::profiles_service_client::ProfilesServiceClient;
use opentelemetry_proto::tonic::collector::profiles::v1development::ExportProfilesServiceRequest;
use opentelemetry_proto::tonic::profiles::v1development::{ProfilesDictionary, Sample};
use opentelemetry_proto::tonic::profiles::v1development::{
    Function, Line, Location, Mapping, Profile, ResourceProfiles, ScopeProfiles,
};
use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Drop;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

const DUMMY_MAPPING_IDX: i32 = 0;
const DUMMY_MAPPING: Mapping = Mapping {
    memory_start: 0,
    memory_limit: 0,
    file_offset: 0,
    filename_strindex: 0,
    attribute_indices: vec![],
    has_functions: false,
    has_filenames: false,
    has_line_numbers: false,
    has_inline_frames: false,
};

/// OTLPBuilder is responsible for building the profile data
pub struct OTLPBuilder {
    pd: ProfilesDictionary,
    profile: Profile,
    strings: HashMap<String, i32>,
    functions: HashMap<FunctionMirror, i32>,
    locations: HashMap<LocationMirror, i32>,
}
impl Default for OTLPBuilder {
    fn default() -> Self {
        let mut res = Self {
            pd: ProfilesDictionary::default(),
            profile: Profile{
                sample_type: vec![], //todo
                sample: vec![],
                location_indices: vec![],
                time_nanos: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as i64,
                duration_nanos: 0, //todo
                period_type: None,
                period: 0,
                comment_strindices: vec![],
                default_sample_type_index: 0,
                profile_id: vec![],
                dropped_attributes_count: 0,
                original_payload_format: "".to_string(),
                original_payload: vec![],
                attribute_indices: vec![],
            },
            strings: HashMap::default(),
            functions: HashMap::default(),
            locations: HashMap::default(),
        };
        res.str("".to_string());
        res.pd.mapping_table.push(DUMMY_MAPPING);
        res
    }
}
impl OTLPBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, trace: &StackTrace) -> Result<(), Error> {
        let mut s = Sample{
            locations_start_index: self.profile.location_indices.len() as i32,
            locations_length: 0,
            value: vec![1],
            attribute_indices: vec![],
            link_index: None,
            timestamps_unix_nano: vec![],
        };

        for x in &trace.frames {
            let f = FunctionMirror {
                name_strindex: self.str(x.name.clone()), //todo just move the whole StackTrace here and dont clone
                filename_strindex: self.str(x.filename.clone()), //todo just move the whole StackTrace here and dont clone
            };
            let l = LocationMirror {
                function_index: self.fun(f),
                line: x.line,
            };
            let l = self.loc(l);
            self.profile.location_indices.push(l);
            s.locations_length+=1;
        }


        self.profile.sample.push(s);

        Ok(())
    }

    pub fn take(self) -> (ProfilesDictionary, ScopeProfiles) {
        (self.pd, ScopeProfiles{
            scope: None,
            profiles: vec![self.profile],
            schema_url: "".to_string(),
        })
    }

    fn str(&mut self, s: String) -> i32 {
        Self::insert(&mut self.strings, &mut self.pd.string_table, s)
    }

    fn fun(&mut self, fm: FunctionMirror) -> i32 {
        Self::insert(&mut self.functions, &mut self.pd.function_table, fm)
    }

    fn loc(&mut self, lm: LocationMirror) -> i32 {
        Self::insert(&mut self.locations, &mut self.pd.location_table, lm)
    }

    fn insert<M, V>(hm: &mut HashMap<M, i32>, table: &mut Vec<V>, m: M) -> i32
    where
        M: PartialEq + Eq + Hash + Clone,
        V: From<M>,
    {
        match hm.get(&m) {
            None => {
                let idx = table.len() as i32;
                table.push(m.clone().into()); //todo think how this clone can be avoided for strign table
                hm.insert(m, idx);
                idx
            }
            Some(idx) => *idx,
        }
    }
}

/// OTLPClient is responsible for sending profile data over gRPC
pub struct OTLPClient {
    runtime: Runtime,
    pub client: Arc<Mutex<ProfilesServiceClient<tonic::transport::Channel>>>,
}

impl OTLPClient {
    pub fn new(endpoint: String) -> Result<Self, Error> {
        let runtime = Runtime::new()
            .map_err(|e| Error::msg(format!("Failed to create tokio runtime: {}", e)))?;
        let c = runtime.block_on(async { ProfilesServiceClient::connect(endpoint).await })?;

        Ok(Self {
            runtime,
            client: Arc::new(Mutex::new(c)),
        })
    }

    pub fn export(&self, d: ProfilesDictionary, p: ScopeProfiles) {
        let c = self.client.clone();
        self.runtime.spawn(async move {
            let request = ExportProfilesServiceRequest {
                resource_profiles: vec![ResourceProfiles {
                    resource: None,
                    scope_profiles: vec![p],
                    schema_url: String::new(),
                }],
                dictionary: Some(d),
            };
            let mut guard = c.lock().await;
            match guard.export(request).await {
                Ok(_) => {
                    log::info!("Exported profiles to OTLP endpoint");
                }
                Err(e) => {
                    log::error!("Failed to export profiles: {}", e);
                }
            }
        });
    }
}

pub struct OTLP {
    builder: OTLPBuilder,
    client: OTLPClient,
}

impl OTLP {
    pub fn new(host: String) -> Result<Self, Error> {
        Ok(Self {
            builder: OTLPBuilder::new(),
            client: OTLPClient::new(host)?,
        })
    }

    pub fn record(&mut self, trace: &StackTrace) -> Result<(), Error> {
        self.builder.record(trace)
    }

    pub fn export(&mut self) {
        let (d, p) = std::mem::take(&mut self.builder).take();
        self.client.export(d, p)
    }
}

impl Drop for OTLP {
    fn drop(&mut self) {}
}

#[derive(PartialEq, Clone, Eq, Hash)]
struct FunctionMirror {
    pub name_strindex: i32,
    pub filename_strindex: i32,
}

impl From<FunctionMirror> for Function {
    fn from(m: FunctionMirror) -> Self {
        Self {
            name_strindex: m.name_strindex,
            system_name_strindex: 0,
            filename_strindex: m.filename_strindex,
            start_line: 0,
        }
    }
}

#[derive(PartialEq, Clone, Eq, Hash)]
struct LocationMirror {
    function_index: i32,
    line: i32,
}

impl From<LocationMirror> for Location {
    fn from(m: LocationMirror) -> Self {
        Self {
            mapping_index: Some(DUMMY_MAPPING_IDX),
            address: 0,
            line: vec![Line {
                function_index: m.function_index,
                line: m.line as i64,
                column: 0,
            }],
            is_folded: false,
            attribute_indices: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stack_trace::{Frame, StackTrace};

    fn create_mock_frame(name: &str, filename: &str, line: i32) -> Frame {
        Frame {
            name: name.to_string(),
            filename: filename.to_string(),
            module: None,
            short_filename: Some(filename.to_string()),
            line,
            locals: None,
            is_entry: false,
            is_shim_entry: false,
        }
    }

    fn create_mock_stack_trace(thread_id: u64, frames: Vec<Frame>) -> StackTrace {
        StackTrace {
            pid: 12345,
            thread_id,
            thread_name: Some(format!("Thread-{}", thread_id)),
            os_thread_id: Some(thread_id + 1000),
            active: true,
            owns_gil: thread_id == 1,
            frames,
            process_info: None,
        }
    }

    #[test]
    fn test_otlp_export_to_localhost() {
        // Create a couple of mock stack traces
        let trace1 = create_mock_stack_trace(
            1,
            vec![
                create_mock_frame("calculate", "math_utils.py", 42),
                create_mock_frame("process_data", "utils.py", 25),
                create_mock_frame("main", "main.py", 10),
            ],
        );

        let trace2 = create_mock_stack_trace(
            2,
            vec![
                create_mock_frame("parse_json", "parser.py", 67),
                create_mock_frame("handle_request", "handler.py", 33),
                create_mock_frame("worker_thread", "worker.py", 15),
            ],
        );

        // Create OTLP client and record traces
        let mut otlp = match OTLP::new("http://localhost:4040".to_string()) {
            Ok(client) => client,
            Err(e) => {
                println!("[DEBUG_LOG] Failed to create OTLP client: {}. This is expected if no OTLP server is running on localhost:4040", e);
                return; // Skip test if can't connect
            }
        };

        // Record the stack traces
        if let Err(e) = otlp.record(&trace1) {
            println!("[DEBUG_LOG] Failed to record trace1: {}", e);
        } else {
            println!("[DEBUG_LOG] Successfully recorded trace1 with {} frames", trace1.frames.len());
        }

        if let Err(e) = otlp.record(&trace2) {
            println!("[DEBUG_LOG] Failed to record trace2: {}", e);
        } else {
            println!("[DEBUG_LOG] Successfully recorded trace2 with {} frames", trace2.frames.len());
        }

        // Export the traces to localhost:4040
        println!("[DEBUG_LOG] Exporting traces to localhost:4040");
        otlp.export();

        // Give some time for the async export to complete
        std::thread::sleep(std::time::Duration::from_millis(100));

        println!("[DEBUG_LOG] OTLP test completed successfully");
    }
}
