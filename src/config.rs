use clap::{App, Arg};
use remoteprocess::Pid;

/// Options on how to collect samples from a python process
#[derive(Debug, Clone, Eq, PartialEq)]
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
    pub command: String,
    #[doc(hidden)]
    pub pid: Option<Pid>,
    #[doc(hidden)]
    pub python_program: Option<Vec<String>>,
    #[doc(hidden)]
    pub sampling_rate: u64,
    #[doc(hidden)]
    pub filename: Option<String>,
    #[doc(hidden)]
    pub format: Option<FileFormat>,
    #[doc(hidden)]
    pub show_line_numbers: bool,
    #[doc(hidden)]
    pub duration: RecordDuration,
    #[doc(hidden)]
    pub include_idle: bool,
    #[doc(hidden)]
    pub include_thread_ids: bool,
    #[doc(hidden)]
    pub gil_only: bool,
}

arg_enum!{
    #[derive(Debug, Clone, Eq, PartialEq)]
    #[allow(non_camel_case_types)]
    pub enum FileFormat {
        flamegraph,
        raw,
        speedscope
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RecordDuration {
    Unlimited,
    Seconds(u64)
}

impl Default for Config {
    /// Initializes a new Config object with default parameters
    #[allow(dead_code)]
    fn default() -> Config {
        Config{pid: None, python_program: None, filename: None, format: None,
               command: String::from("top"),
               non_blocking: false, show_line_numbers: false, sampling_rate: 100,
               duration: RecordDuration::Unlimited, native: false,
               gil_only: false, include_idle: false, include_thread_ids: false}
    }
}

impl Config {
    /// Uses clap to set config options from commandline arguments
    pub fn from_commandline() -> Config {
        let args: Vec<String> = std::env::args().collect();
        Config::from_args(&args).unwrap_or_else( |e| e.exit() )
    }

    pub fn from_args(args: &[String]) -> clap::Result<Config> {
        // we don't yet support native tracing on 32 bit linux
        let allow_native = cfg!(unwind);

        // pid/native/nonblocking/rate/pythonprogram arguments can be
        // used across various subcommand - define once here
        let pid = Arg::with_name("pid")
                    .short("p")
                    .long("pid")
                    .value_name("pid")
                    .help("PID of a running python program to spy on")
                    .takes_value(true)
                    .required_unless("python_program");
        let native = Arg::with_name("native")
                    .short("n")
                    .long("native")
                    .hidden(!allow_native)
                    .help("Collect stack traces from native extensions written in Cython, C or C++");
        let nonblocking = Arg::with_name("nonblocking")
                    .long("nonblocking")
                    .help("Don't pause the python process when collecting samples. Setting this option will reduce \
                          the perfomance impact of sampling, but may lead to inaccurate results");
        let rate = Arg::with_name("rate")
                    .short("r")
                    .long("rate")
                    .value_name("rate")
                    .help("The number of samples to collect per second")
                    .default_value("100")
                    .takes_value(true);
        let program = Arg::with_name("python_program")
                    .help("commandline of a python program to run")
                    .multiple(true);

        let matches = App::new(crate_name!())
            .version(crate_version!())
            .about(crate_description!())
            .setting(clap::AppSettings::InferSubcommands)
            .setting(clap::AppSettings::SubcommandRequiredElseHelp)
            .global_setting(clap::AppSettings::DeriveDisplayOrder)
            .global_setting(clap::AppSettings::UnifiedHelpMessage)
            .subcommand(clap::SubCommand::with_name("record")
                .about("Records stack trace information to a flamegraph, speedscope or raw file")
                .arg(program.clone())
                .arg(pid.clone())
                .arg(Arg::with_name("output")
                    .short("o")
                    .long("output")
                    .value_name("filename")
                    .help("Output filename")
                    .takes_value(true)
                    .required(true))
                .arg(Arg::with_name("format")
                    .short("f")
                    .long("format")
                    .value_name("format")
                    .help("Output file format")
                    .takes_value(true)
                    .possible_values(&FileFormat::variants())
                    .case_insensitive(true)
                    .default_value("flamegraph"))
                .arg(Arg::with_name("duration")
                    .short("d")
                    .long("duration")
                    .value_name("duration")
                    .help("The number of seconds to sample for")
                    .default_value("unlimited")
                    .takes_value(true))
                .arg(rate.clone())
                .arg(Arg::with_name("function")
                    .short("F")
                    .long("function")
                    .help("Aggregate samples by function name instead of by line number"))
                .arg(Arg::with_name("gil")
                    .short("g")
                    .long("gil")
                    .help("Only include traces that are holding on to the GIL"))
                .arg(Arg::with_name("threads")
                    .short("t")
                    .long("threads")
                    .help("Show thread ids in the output"))
                .arg(Arg::with_name("idle")
                    .short("i")
                    .long("idle")
                    .help("Include stack traces for idle threads"))
                .arg(native.clone())
                .arg(nonblocking.clone())
            )
            .subcommand(clap::SubCommand::with_name("top")
                .about("Displays a top like view of functions consuming CPU")
                .arg(program.clone())
                .arg(pid.clone())
                .arg(rate.clone())
                .arg(native.clone())
                .arg(nonblocking.clone())
            )
            .subcommand(clap::SubCommand::with_name("dump")
                .about("Dumps stack traces for a target program to stdout")
                .arg(pid.clone().required(true))
                .arg(native.clone())
                .arg(nonblocking.clone())
            )
            .get_matches_from_safe(args)?;
        info!("Command line args: {:?}", matches);

        let mut config = Config::default();

        let (subcommand, matches) = matches.subcommand();
        let matches = matches.unwrap();

        match subcommand {
            "record" => {
                config.sampling_rate = value_t!(matches, "rate", u64)?;
                config.duration = match matches.value_of("duration") {
                    Some("unlimited") | None => RecordDuration::Unlimited,
                    Some(seconds) => RecordDuration::Seconds(seconds.parse().expect("invalid duration"))
                };
                config.format = Some(value_t!(matches.value_of("format"), FileFormat).unwrap_or_else(|e| e.exit()));
                config.filename = matches.value_of("output").map(|f| f.to_owned());
            },
            "top" => {
                config.sampling_rate = value_t!(matches, "rate", u64)?;
            }
            _ => {}
        }
        config.command = subcommand.to_owned();

        // options that can be shared between subcommands
        config.pid = matches.value_of("pid").map(|p| p.parse().expect("invalid pid"));
        config.python_program = matches.values_of("python_program").map(|vals| {
            vals.map(|v| v.to_owned()).collect()
        });
        config.show_line_numbers = matches.occurrences_of("function") == 0;
        config.include_idle = matches.occurrences_of("idle") > 0;
        config.gil_only = matches.occurrences_of("gil") > 0;
        config.include_thread_ids = matches.occurrences_of("threads") > 0;

        config.non_blocking = matches.occurrences_of("nonblocking") > 0;
        config.native = matches.occurrences_of("native") > 0;

        // disable native profiling if invalidly asked for
        if !allow_native && config.native {
            error!("Native stack traces are not yet supported on this OS. Disabling");
            config.native = false;
        }

        if config.native && config.non_blocking {
            error!("Can't get native stack traces with the --nonblocking option. Disabling native.");
            config.native = false;
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn split(cmd: &str) -> Vec<String> {
        cmd.split_whitespace().map(|x| x.to_owned()).collect()
    }

    #[test]
    fn test_parse_record_args() {
        // basic use case
        let config = Config::from_args(&split("py-spy record --pid 1234 --output foo")).unwrap();
        assert_eq!(config.pid, Some(1234));
        assert_eq!(config.filename, Some(String::from("foo")));
        assert_eq!(config.format, Some(FileFormat::flamegraph));
        assert_eq!(config.command, String::from("record"));

        // same command using short versions of everything
        let short_config = Config::from_args(&split("py-spy r -p 1234 -o foo")).unwrap();
        assert_eq!(config, short_config);

        // missing the --pid argument should fail
        assert_eq!(Config::from_args(&split("py-spy record -o foo")).unwrap_err().kind,
                   clap::ErrorKind::MissingRequiredArgument);

        // but should work when passed a python program
        let program_config = Config::from_args(&split("py-spy r -o foo -- python test.py")).unwrap();
        assert_eq!(program_config.python_program, Some(vec![String::from("python"), String::from("test.py")]));
        assert_eq!(program_config.pid, None);

        // passing an invalid file format should fail
        assert_eq!(Config::from_args(&split("py-spy r -p 1234 -o foo -f unknown")).unwrap_err().kind,
                   clap::ErrorKind::InvalidValue);

        // test out overriding these params by setting flags
        assert_eq!(config.include_idle, false);
        assert_eq!(config.gil_only, false);
        assert_eq!(config.include_thread_ids, false);

        let config_flags = Config::from_args(&split("py-spy r -p 1234 -o foo --idle --gil --threads")).unwrap();
        assert_eq!(config_flags.include_idle, true);
        assert_eq!(config_flags.gil_only, true);
        assert_eq!(config_flags.include_thread_ids, true);
    }

    #[test]
    fn test_parse_dump_args() {
        // basic use case
        let config = Config::from_args(&split("py-spy dump --pid 1234")).unwrap();
        assert_eq!(config.pid, Some(1234));
        assert_eq!(config.command, String::from("dump"));

        // short version
        let short_config = Config::from_args(&split("py-spy d -p 1234")).unwrap();
        assert_eq!(config, short_config);

        // missing the --pid argument should fail
        assert_eq!(Config::from_args(&split("py-spy dump")).unwrap_err().kind,
                   clap::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn test_parse_top_args() {
        // basic use case
        let config = Config::from_args(&split("py-spy top --pid 1234")).unwrap();
        assert_eq!(config.pid, Some(1234));
        assert_eq!(config.command, String::from("top"));

        // short version
        let short_config = Config::from_args(&split("py-spy t -p 1234")).unwrap();
        assert_eq!(config, short_config);
    }

    #[test]
    fn test_parse_args() {
        assert_eq!(Config::from_args(&split("py-spy dude")).unwrap_err().kind,
                   clap::ErrorKind::UnrecognizedSubcommand);
    }
}
