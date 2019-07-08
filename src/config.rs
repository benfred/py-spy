use clap::{App, Arg};
use failure::Error;
use remoteprocess::Pid;

/// Options on how to collect samples from a python process
#[derive(Debug, Clone)]
pub struct Config {
    /// Whether or not we should stop the python process when taking samples.
    /// Setting this to false will reduce the performance impact on the target
    /// python process, but can lead to incorrect results like partial stack
    /// traces being returned or a higher sampling error rate
    pub non_blocking: bool,

    /// Whether or not to profile native extensions. Note: this option can not be
    /// used with the nonblocking option, as we have to pause the process to collect
    /// the native stack traces
    pub native: bool,

    // The following config options only apply when using py-spy as an application
    #[doc(hidden)]
    pub sampling_rate: u64,
    #[doc(hidden)]
    pub pid: Option<Pid>,
    #[doc(hidden)]
    pub python_program: Option<Vec<String>>,
    #[doc(hidden)]
    pub dump: bool,
    #[doc(hidden)]
    pub flame_file_name: Option<String>,
    #[doc(hidden)]
    pub show_line_numbers: bool,
    #[doc(hidden)]
    pub duration: u64,
}

impl Default for Config {
    /// Initializes a new Config object with default parameters
    #[allow(dead_code)]
    fn default() -> Config {
        Config{pid: None, python_program: None, dump: false, flame_file_name: None,
               non_blocking: false, show_line_numbers: false, sampling_rate: 100,
               duration: 2, native: false}
    }
}

impl Config {
    /// Uses clap to set config options from commandline arguments
    pub fn from_commandline() -> Result<Config, Error> {
        // we don't yet support native tracing on 32 bit linux
        let allow_native = cfg!(unwind);

        let matches = App::new(crate_name!())
            .version(crate_version!())
            .about(crate_description!())
            .arg(Arg::with_name("function")
                .short("F")
                .long("function")
                .help("Aggregate samples by function name instead of by line number"))
            .arg(Arg::with_name("native")
                .short("n")
                .long("native")
                .hidden(!allow_native)
                .help("Collect stack traces from native extensions written in Cython, C or C++"))
            .arg(Arg::with_name("pid")
                .short("p")
                .long("pid")
                .value_name("pid")
                .help("PID of a running python program to spy on")
                .takes_value(true)
                .required_unless("python_program"))
            .arg(Arg::with_name("dump")
                .long("dump")
                .help("Dump the current stack traces to stdout"))
            .arg(Arg::with_name("nonblocking")
                .long("nonblocking")
                .help("Don't pause the python process when collecting samples. Setting this option will reduce \
                      the perfomance impact of sampling, but may lead to inaccurate results"))
            .arg(Arg::with_name("flame")
                .short("f")
                .long("flame")
                .value_name("flamefile")
                .help("Generate a flame graph and write to a file")
                .takes_value(true))
            .arg(Arg::with_name("rate")
                .short("r")
                .long("rate")
                .value_name("rate")
                .help("The number of samples to collect per second")
                .default_value("100")
                .takes_value(true))
            .arg(Arg::with_name("duration")
                .short("d")
                .long("duration")
                .value_name("duration")
                .help("The number of seconds to sample for when generating a flame graph")
                .default_value("2")
                .takes_value(true))
            .arg(Arg::with_name("python_program")
                .help("commandline of a python program to run")
                .multiple(true)
                )
            .get_matches();
        info!("Command line args: {:?}", matches);

        // what to sample
        let pid = matches.value_of("pid").map(|p| p.parse().expect("invalid pid"));
        let python_program = matches.values_of("python_program").map(|vals| {
            vals.map(|v| v.to_owned()).collect()
        });

        // what to generate
        let flame_file_name = matches.value_of("flame").map(|f| f.to_owned());
        let dump = matches.occurrences_of("dump") > 0;

        // how to sample
        let sampling_rate = value_t!(matches, "rate", u64)?;
        let duration = value_t!(matches, "duration", u64)?;
        let show_line_numbers = matches.occurrences_of("function") == 0;
        let non_blocking = matches.occurrences_of("nonblocking") > 0;
        let mut native = matches.occurrences_of("native") > 0;

        if !allow_native && native {
            error!("Native stack traces are not yet supported on this OS. Disabling");
            native = false;
        }

        if native && non_blocking {
            error!("Can't get native stack traces with the --nonblocking option. Disabling native.");
            native = false;
        }

        Ok(Config{pid, python_program, dump, flame_file_name,
                  sampling_rate, duration,
                  show_line_numbers, non_blocking, native})
    }
}
