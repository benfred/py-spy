use clap::{
    crate_description, crate_name, crate_version, value_parser, Arg, ArgEnum, Command,
    PossibleValue,
};
use remoteprocess::Pid;

/// Options on how to collect samples from a python process
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Whether or not we should stop the python process when taking samples.
    /// Setting this to false will reduce the performance impact on the target
    /// python process, but can lead to incorrect results like partial stack
    /// traces being returned or a higher sampling error rate
    pub blocking: LockingStrategy,

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
    pub subprocesses: bool,
    #[doc(hidden)]
    pub gil_only: bool,
    #[doc(hidden)]
    pub hide_progress: bool,
    #[doc(hidden)]
    pub capture_output: bool,
    #[doc(hidden)]
    pub dump_json: bool,
    #[doc(hidden)]
    pub dump_locals: u64,
    #[doc(hidden)]
    pub full_filenames: bool,
    #[doc(hidden)]
    pub lineno: LineNo,
    #[doc(hidden)]
    pub refresh_seconds: f64,
    #[doc(hidden)]
    pub core_filename: Option<String>,
}

#[allow(non_camel_case_types)]
#[derive(ArgEnum, Debug, Copy, Clone, Eq, PartialEq)]
pub enum FileFormat {
    flamegraph,
    raw,
    speedscope,
    chrometrace,
}

impl FileFormat {
    pub fn possible_values() -> impl Iterator<Item = PossibleValue<'static>> {
        FileFormat::value_variants()
            .iter()
            .filter_map(ArgEnum::to_possible_value)
    }
}

impl std::str::FromStr for FileFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        for variant in Self::value_variants() {
            if variant.to_possible_value().unwrap().matches(s, false) {
                return Ok(*variant);
            }
        }
        Err(format!("Invalid fileformat: {}", s))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum LockingStrategy {
    NonBlocking,
    #[allow(dead_code)]
    AlreadyLocked,
    Lock,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RecordDuration {
    Unlimited,
    Seconds(u64),
}

#[derive(Debug, Clone, Eq, PartialEq, Copy)]
pub enum LineNo {
    NoLine,
    First,
    LastInstruction,
}

impl Default for Config {
    /// Initializes a new Config object with default parameters
    #[allow(dead_code)]
    fn default() -> Config {
        Config {
            pid: None,
            python_program: None,
            filename: None,
            format: None,
            command: String::from("top"),
            blocking: LockingStrategy::Lock,
            show_line_numbers: false,
            sampling_rate: 100,
            duration: RecordDuration::Unlimited,
            native: false,
            gil_only: false,
            include_idle: false,
            include_thread_ids: false,
            hide_progress: false,
            capture_output: true,
            dump_json: false,
            dump_locals: 0,
            subprocesses: false,
            full_filenames: false,
            lineno: LineNo::LastInstruction,
            refresh_seconds: 1.0,
            core_filename: None,
        }
    }
}

impl Config {
    /// Uses clap to set config options from commandline arguments
    pub fn from_commandline() -> Config {
        let args: Vec<String> = std::env::args().collect();
        Config::from_args(&args).unwrap_or_else(|e| e.exit())
    }

    pub fn from_args(args: &[String]) -> clap::Result<Config> {
        // pid/native/nonblocking/rate/python_program/subprocesses/full_filenames arguments can be
        // used across various subcommand - define once here
        let pid = Arg::new("pid")
            .short('p')
            .long("pid")
            .value_name("pid")
            .help("PID of a running python program to spy on")
            .takes_value(true);

        #[cfg(unwind)]
        let native = Arg::new("native")
            .short('n')
            .long("native")
            .help("Collect stack traces from native extensions written in Cython, C or C++");

        #[cfg(not(target_os="freebsd"))]
        let nonblocking = Arg::new("nonblocking")
                    .long("nonblocking")
                    .help("Don't pause the python process when collecting samples. Setting this option will reduce \
                          the performance impact of sampling, but may lead to inaccurate results");

        let rate = Arg::new("rate")
            .short('r')
            .long("rate")
            .value_name("rate")
            .help("The number of samples to collect per second")
            .default_value("100")
            .takes_value(true);

        let subprocesses = Arg::new("subprocesses")
            .short('s')
            .long("subprocesses")
            .help("Profile subprocesses of the original process");

        let full_filenames = Arg::new("full_filenames").long("full-filenames").help(
            "Show full Python filenames, instead of shortening to show only the package part",
        );
        let program = Arg::new("python_program")
            .help("commandline of a python program to run")
            .multiple_values(true);

        let idle = Arg::new("idle")
            .short('i')
            .long("idle")
            .help("Include stack traces for idle threads");

        let gil = Arg::new("gil")
            .short('g')
            .long("gil")
            .help("Only include traces that are holding on to the GIL");

        let top_delay = Arg::new("delay")
            .long("delay")
            .value_name("seconds")
            .help("Delay between 'top' refreshes.")
            .default_value("1.0")
            .value_parser(clap::value_parser!(f64))
            .takes_value(true);

        let record = Command::new("record")
            .about("Records stack trace information to a flamegraph, speedscope or raw file")
            .arg(program.clone())
            .arg(pid.clone().required_unless_present("python_program"))
            .arg(full_filenames.clone())
            .arg(
                Arg::new("output")
                    .short('o')
                    .long("output")
                    .value_name("filename")
                    .help("Output filename")
                    .takes_value(true)
                    .required(false),
            )
            .arg(
                Arg::new("format")
                    .short('f')
                    .long("format")
                    .value_name("format")
                    .help("Output file format")
                    .takes_value(true)
                    .possible_values(FileFormat::possible_values())
                    .ignore_case(true)
                    .default_value("flamegraph"),
            )
            .arg(
                Arg::new("duration")
                    .short('d')
                    .long("duration")
                    .value_name("duration")
                    .help("The number of seconds to sample for")
                    .default_value("unlimited")
                    .takes_value(true),
            )
            .arg(rate.clone())
            .arg(subprocesses.clone())
            .arg(Arg::new("function").short('F').long("function").help(
                "Aggregate samples by function's first line number, instead of current line number",
            ))
            .arg(
                Arg::new("nolineno")
                    .long("nolineno")
                    .help("Do not show line numbers"),
            )
            .arg(
                Arg::new("threads")
                    .short('t')
                    .long("threads")
                    .help("Show thread ids in the output"),
            )
            .arg(gil.clone())
            .arg(idle.clone())
            .arg(
                Arg::new("capture")
                    .long("capture")
                    .hide(true)
                    .help("Captures output from child process"),
            )
            .arg(
                Arg::new("hideprogress")
                    .long("hideprogress")
                    .hide(true)
                    .help("Hides progress bar (useful for showing error output on record)"),
            );

        let top = Command::new("top")
            .about("Displays a top like view of functions consuming CPU")
            .arg(program.clone())
            .arg(pid.clone().required_unless_present("python_program"))
            .arg(rate.clone())
            .arg(subprocesses.clone())
            .arg(full_filenames.clone())
            .arg(gil.clone())
            .arg(idle.clone())
            .arg(top_delay.clone());

        #[cfg(target_os = "linux")]
        let dump_pid = pid.clone().required_unless_present("core");

        #[cfg(not(target_os = "linux"))]
        let dump_pid = pid.clone().required(true);

        let dump = Command::new("dump")
            .about("Dumps stack traces for a target program to stdout")
            .arg(dump_pid);

        #[cfg(target_os = "linux")]
        let dump = dump.arg(
            Arg::new("core")
                .short('c')
                .long("core")
                .help("Filename of coredump to display python stack traces from")
                .value_name("core")
                .takes_value(true),
        );

        let dump = dump.arg(full_filenames.clone())
            .arg(Arg::new("locals")
                .short('l')
                .long("locals")
                .multiple_occurrences(true)
                .help("Show local variables for each frame. Passing multiple times (-ll) increases verbosity"))
            .arg(Arg::new("json")
                .short('j')
                .long("json")
                .help("Format output as JSON"))
            .arg(subprocesses.clone());

        let completions = Command::new("completions")
            .about("Generate shell completions")
            .hide(true)
            .arg(
                Arg::new("shell")
                    .value_parser(value_parser!(clap_complete::Shell))
                    .help("Shell type"),
            );

        // add native unwinding if appropriate
        #[cfg(unwind)]
        let record = record.arg(native.clone());
        #[cfg(unwind)]
        let top = top.arg(native.clone());
        #[cfg(unwind)]
        let dump = dump.arg(native.clone());

        // Nonblocking isn't an option for freebsd, remove
        #[cfg(not(target_os = "freebsd"))]
        let record = record.arg(nonblocking.clone());
        #[cfg(not(target_os = "freebsd"))]
        let top = top.arg(nonblocking.clone());
        #[cfg(not(target_os = "freebsd"))]
        let dump = dump.arg(nonblocking.clone());

        let mut app = Command::new(crate_name!())
            .version(crate_version!())
            .about(crate_description!())
            .subcommand_required(true)
            .infer_subcommands(true)
            .arg_required_else_help(true)
            .global_setting(clap::AppSettings::DeriveDisplayOrder)
            .subcommand(record)
            .subcommand(top)
            .subcommand(dump)
            .subcommand(completions);
        let matches = app.clone().try_get_matches_from(args)?;
        info!("Command line args: {:?}", matches);

        let mut config = Config::default();

        let (subcommand, matches) = matches.subcommand().unwrap();

        match subcommand {
            "record" => {
                config.sampling_rate = matches.value_of_t("rate")?;
                config.duration = match matches.value_of("duration") {
                    Some("unlimited") | None => RecordDuration::Unlimited,
                    Some(seconds) => {
                        RecordDuration::Seconds(seconds.parse().expect("invalid duration"))
                    }
                };
                config.format = Some(matches.value_of_t("format")?);
                config.filename = matches.value_of("output").map(|f| f.to_owned());
                config.show_line_numbers = matches.occurrences_of("nolineno") == 0;
                config.lineno = if matches.occurrences_of("nolineno") > 0 {
                    LineNo::NoLine
                } else if matches.occurrences_of("function") > 0 {
                    LineNo::First
                } else {
                    LineNo::LastInstruction
                };
                config.include_thread_ids = matches.occurrences_of("threads") > 0;
                if matches.occurrences_of("nolineno") > 0 && matches.occurrences_of("function") > 0
                {
                    eprintln!("--function & --nolinenos can't be used together");
                    std::process::exit(1);
                }
                config.hide_progress = matches.occurrences_of("hideprogress") > 0;
            }
            "top" => {
                config.sampling_rate = matches.value_of_t("rate")?;
                config.refresh_seconds = *matches.get_one::<f64>("delay").unwrap();
            }
            "dump" => {
                config.dump_json = matches.occurrences_of("json") > 0;
                config.dump_locals = matches.occurrences_of("locals");

                #[cfg(target_os = "linux")]
                {
                    config.core_filename = matches.value_of("core").map(|f| f.to_owned());
                }
            }
            "completions" => {
                let shell = matches.get_one::<clap_complete::Shell>("shell").unwrap();
                let app_name = app.get_name().to_string();
                clap_complete::generate(*shell, &mut app, app_name, &mut std::io::stdout());
                std::process::exit(0);
            }
            _ => {}
        }

        match subcommand {
            "record" | "top" => {
                config.python_program = matches
                    .values_of("python_program")
                    .map(|vals| vals.map(|v| v.to_owned()).collect());
                config.gil_only = matches.occurrences_of("gil") > 0;
                config.include_idle = matches.occurrences_of("idle") > 0;
            }
            _ => {}
        }

        config.subprocesses = matches.occurrences_of("subprocesses") > 0;
        config.command = subcommand.to_owned();

        // options that can be shared between subcommands
        config.pid = matches
            .value_of("pid")
            .map(|p| p.parse().expect("invalid pid"));
        config.full_filenames = matches.occurrences_of("full_filenames") > 0;
        if cfg!(unwind) {
            config.native = matches.occurrences_of("native") > 0;
        }

        config.capture_output = config.command != "record" || matches.occurrences_of("capture") > 0;
        if !config.capture_output {
            config.hide_progress = true;
        }

        if matches.occurrences_of("nonblocking") > 0 {
            // disable native profiling if invalidly asked for
            if config.native {
                eprintln!("Can't get native stack traces with the --nonblocking option.");
                std::process::exit(1);
            }
            config.blocking = LockingStrategy::NonBlocking;
        }

        #[cfg(windows)]
        {
            if config.native && config.subprocesses {
                // the native extension profiling code relies on dbghelp library, which doesn't
                // seem to work when connecting to multiple processes. disallow
                eprintln!(
                    "Can't get native stack traces with the ---subprocesses option on windows."
                );
                std::process::exit(1);
            }
        }

        #[cfg(target_os = "freebsd")]
        {
            if config.pid.is_some() {
                if std::env::var("PYSPY_ALLOW_FREEBSD_ATTACH").is_err() {
                    eprintln!("On FreeBSD, running py-spy can cause an exception in the profiled process if the process \
                        is calling 'socket.connect'.");
                    eprintln!("While this is fixed in recent versions of python, you need to acknowledge the risk here by \
                        setting an environment variable PYSPY_ALLOW_FREEBSD_ATTACH to run this command.");
                    eprintln!(
                        "\nSee https://github.com/benfred/py-spy/issues/147 for more information"
                    );
                    std::process::exit(-1);
                }
            }
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn get_config(cmd: &str) -> clap::Result<Config> {
        #[cfg(target_os = "freebsd")]
        std::env::set_var("PYSPY_ALLOW_FREEBSD_ATTACH", "1");
        let args: Vec<String> = cmd.split_whitespace().map(|x| x.to_owned()).collect();
        Config::from_args(&args)
    }

    #[test]
    fn test_parse_record_args() {
        // basic use case
        let config = get_config("py-spy record --pid 1234 --output foo").unwrap();
        assert_eq!(config.pid, Some(1234));
        assert_eq!(config.filename, Some(String::from("foo")));
        assert_eq!(config.format, Some(FileFormat::flamegraph));
        assert_eq!(config.command, String::from("record"));

        // same command using short versions of everything
        let short_config = get_config("py-spy r -p 1234 -o foo").unwrap();
        assert_eq!(config, short_config);

        // missing the --pid argument should fail
        assert_eq!(
            get_config("py-spy record -o foo").unwrap_err().kind,
            clap::ErrorKind::MissingRequiredArgument
        );

        // but should work when passed a python program
        let program_config = get_config("py-spy r -o foo -- python test.py").unwrap();
        assert_eq!(
            program_config.python_program,
            Some(vec![String::from("python"), String::from("test.py")])
        );
        assert_eq!(program_config.pid, None);

        // passing an invalid file format should fail
        assert_eq!(
            get_config("py-spy r -p 1234 -o foo -f unknown")
                .unwrap_err()
                .kind,
            clap::ErrorKind::InvalidValue
        );

        // test out overriding these params by setting flags
        assert_eq!(config.include_idle, false);
        assert_eq!(config.gil_only, false);
        assert_eq!(config.include_thread_ids, false);

        let config_flags = get_config("py-spy r -p 1234 -o foo --idle --gil --threads").unwrap();
        assert_eq!(config_flags.include_idle, true);
        assert_eq!(config_flags.gil_only, true);
        assert_eq!(config_flags.include_thread_ids, true);
    }

    #[test]
    fn test_parse_dump_args() {
        // basic use case
        let config = get_config("py-spy dump --pid 1234").unwrap();
        assert_eq!(config.pid, Some(1234));
        assert_eq!(config.command, String::from("dump"));

        // short version
        let short_config = get_config("py-spy d -p 1234").unwrap();
        assert_eq!(config, short_config);

        // missing the --pid argument should fail
        assert_eq!(
            get_config("py-spy dump").unwrap_err().kind,
            clap::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn test_parse_top_args() {
        // basic use case
        let config = get_config("py-spy top --pid 1234").unwrap();
        assert_eq!(config.pid, Some(1234));
        assert_eq!(config.command, String::from("top"));

        // short version
        let short_config = get_config("py-spy t -p 1234").unwrap();
        assert_eq!(config, short_config);
    }

    #[test]
    fn test_parse_args() {
        assert_eq!(
            get_config("py-spy dude").unwrap_err().kind,
            clap::ErrorKind::UnrecognizedSubcommand
        );
    }
}
