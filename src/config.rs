use clap::{App, Arg};
use failure::Error;
use read_process_memory::Pid;

#[derive(Debug, Clone)]
pub struct Config {
    pub pid: Option<Pid>,
    pub python_program: Option<Vec<String>>,

    pub dump: bool,
    pub flame_file_name: Option<String>,

    pub non_blocking: bool,
    pub show_line_numbers: bool,
    pub sampling_rate: u64,
    pub duration: u64,
    pub native: bool
}

impl Config {
    pub fn from_commandline() -> Result<Config, Error> {
        // we don't yet support native tracing on 32 bit linux
        let allow_native = !cfg!(all(target_os="linux", target_pointer_width="32"));

        let matches = App::new("py-spy")
            .version("0.1.10")
            .about("A sampling profiler for Python programs")
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

        Ok(Config{pid, python_program, dump, flame_file_name,
                  sampling_rate, duration,
                  show_line_numbers, non_blocking, native})
    }
}
