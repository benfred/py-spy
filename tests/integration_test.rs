extern crate py_spy;
use py_spy::{Config, Pid, PythonSpy};
use std::collections::HashSet;

struct ScriptRunner {
    #[allow(dead_code)]
    child: std::process::Child,
}

impl ScriptRunner {
    fn new(process_name: &str, filename: &str) -> ScriptRunner {
        let child = std::process::Command::new(process_name)
            .arg(filename)
            .spawn()
            .unwrap();
        ScriptRunner { child }
    }

    fn id(&self) -> Pid {
        self.child.id() as _
    }
}

impl Drop for ScriptRunner {
    fn drop(&mut self) {
        if let Err(err) = self.child.kill() {
            eprintln!("Failed to kill child process {}", err);
        }
    }
}

struct TestRunner {
    #[allow(dead_code)]
    child: ScriptRunner,
    spy: PythonSpy,
}

impl TestRunner {
    fn new(config: Config, filename: &str) -> TestRunner {
        let child = ScriptRunner::new("python", filename);
        std::thread::sleep(std::time::Duration::from_millis(400));
        let spy = PythonSpy::retry_new(child.id(), &config, 20).unwrap();
        TestRunner { child, spy }
    }
}

#[test]
fn test_busy_loop() {
    #[cfg(target_os = "macos")]
    {
        // We need root permissions here to run this on OSX
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
    }
    let mut runner = TestRunner::new(Config::default(), "./tests/scripts/busyloop.py");
    let traces = runner.spy.get_stack_traces().unwrap();

    // we can't be guaranteed what line the script is processing, but
    // we should be able to say that the script is active and
    // catch issues like https://github.com/benfred/py-spy/issues/141
    assert!(traces[0].active);
}

#[cfg(feature = "unwind")]
#[test]
fn test_thread_reuse() {
    // on linux we had an issue with the pthread -> native thread id caching
    // the problem was that the pthreadids were getting re-used,
    // and this caused errors on native unwind (since the native thread had
    // exited). Test that this works with a simple script that creates
    // a couple short lived threads, and then profiling with native enabled
    let config = Config {
        native: true,
        ..Default::default()
    };
    let mut runner = TestRunner::new(config, "./tests/scripts/thread_reuse.py");

    let mut errors = 0;

    for _ in 0..100 {
        // should be able to get traces here BUT we do sometimes get errors about
        // not being able to suspend process ("No such file or directory (os error 2)"
        // when threads exit. Allow a small number of errors here.
        if let Err(e) = runner.spy.get_stack_traces() {
            println!("Failed to get traces {}", e);
            errors += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    assert!(errors <= 3);
}

#[test]
fn test_long_sleep() {
    #[cfg(target_os = "macos")]
    {
        // We need root permissions here to run this on OSX
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
    }

    let mut runner = TestRunner::new(Config::default(), "./tests/scripts/longsleep.py");

    let traces = runner.spy.get_stack_traces().unwrap();
    assert_eq!(traces.len(), 1);
    let trace = &traces[0];

    // Make sure the stack trace is what we expect
    assert_eq!(trace.frames[0].name, "longsleep");
    assert_eq!(
        trace.frames[0].short_filename,
        Some("longsleep.py".to_owned())
    );
    assert_eq!(trace.frames[0].line, 5);

    assert_eq!(trace.frames[1].name, "<module>");
    assert_eq!(trace.frames[1].line, 9);
    assert_eq!(
        trace.frames[1].short_filename,
        Some("longsleep.py".to_owned())
    );

    assert!(!traces[0].owns_gil);

    // we should reliably be able to detect the thread is sleeping on osx/windows
    // linux+freebsd is trickier
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    assert!(!traces[0].active);
}

#[test]
fn test_thread_names() {
    #[cfg(target_os = "macos")]
    {
        // We need root permissions here to run this on OSX
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
    }
    let config = Config {
        include_idle: true,
        ..Default::default()
    };
    let mut runner = TestRunner::new(config, "./tests/scripts/thread_names.py");

    let traces = runner.spy.get_stack_traces().unwrap();
    assert_eq!(traces.len(), 11);

    // dictionary + thread name lookup is only supported with python 3.6+
    if runner.spy.version.major == 3 && runner.spy.version.minor >= 6 {
        let mut expected_threads: HashSet<String> =
            (0..10).map(|n| format!("CustomThreadName-{}", n)).collect();
        expected_threads.insert("MainThread".to_string());
        let detected_threads: HashSet<String> = traces
            .iter()
            .map(|trace| trace.thread_name.as_ref().unwrap().clone())
            .collect();
        assert_eq!(expected_threads, detected_threads);
    } else {
        for trace in traces.iter() {
            assert!(trace.thread_name.is_none());
        }
    }
}

#[test]
fn test_recursive() {
    #[cfg(target_os = "macos")]
    {
        // We need root permissions here to run this on OSX
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
    }

    // there used to be a problem where the top-level functions being returned
    // weren't actually entry points: https://github.com/benfred/py-spy/issues/56
    // This was fixed by locking the process while we are profiling it. Test that
    // the fix works by generating some samples from a program that would exhibit
    // this behaviour
    let mut runner = TestRunner::new(Config::default(), "./tests/scripts/recursive.py");

    for _ in 0..100 {
        let traces = runner.spy.get_stack_traces().unwrap();
        assert_eq!(traces.len(), 1);
        let trace = &traces[0];

        assert!(trace.frames.len() <= 22);

        let top_level_frame = &trace.frames[trace.frames.len() - 1];
        assert_eq!(top_level_frame.name, "<module>");
        assert!((top_level_frame.line == 8) || (top_level_frame.line == 7));

        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn test_unicode() {
    #[cfg(target_os = "macos")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
    }
    let mut runner = TestRunner::new(Config::default(), "./tests/scripts/unicode💩.py");

    let traces = runner.spy.get_stack_traces().unwrap();
    assert_eq!(traces.len(), 1);
    let trace = &traces[0];

    assert_eq!(trace.frames[0].name, "function1");
    assert_eq!(
        trace.frames[0].short_filename,
        Some("unicode💩.py".to_owned())
    );
    assert_eq!(trace.frames[0].line, 6);

    assert_eq!(trace.frames[1].name, "<module>");
    assert_eq!(trace.frames[1].line, 9);
    assert_eq!(
        trace.frames[1].short_filename,
        Some("unicode💩.py".to_owned())
    );

    assert!(!traces[0].owns_gil);
}

#[test]
fn test_cyrillic() {
    #[cfg(target_os = "macos")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
    }

    // Identifiers with characters outside the ASCII range are supported from Python 3
    let runner = TestRunner::new(Config::default(), "./tests/scripts/longsleep.py");
    if runner.spy.version.major == 2 {
        return;
    }

    let mut runner = TestRunner::new(Config::default(), "./tests/scripts/cyrillic.py");

    let traces = runner.spy.get_stack_traces().unwrap();
    assert_eq!(traces.len(), 1);
    let trace = &traces[0];

    assert_eq!(trace.frames[0].name, "кириллица");
    assert_eq!(trace.frames[0].line, 4);

    assert_eq!(trace.frames[1].name, "<module>");
    assert_eq!(trace.frames[1].line, 7);
}

#[test]
fn test_local_vars() {
    #[cfg(target_os = "macos")]
    {
        // We need root permissions here to run this on OSX
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
    }

    let config = Config {
        dump_locals: 1,
        ..Default::default()
    };
    let mut runner = TestRunner::new(config, "./tests/scripts/local_vars.py");

    let traces = runner.spy.get_stack_traces().unwrap();
    assert_eq!(traces.len(), 1);
    let trace = &traces[0];
    assert_eq!(trace.frames.len(), 2);
    let frame = &trace.frames[0];
    let locals = frame.locals.as_ref().unwrap();

    assert_eq!(locals.len(), 29);

    let arg1 = &locals[0];
    assert_eq!(arg1.name, "arg1");
    assert!(arg1.arg);
    assert_eq!(arg1.repr, Some("\"foo\"".to_owned()));

    let arg2 = &locals[1];
    assert_eq!(arg2.name, "arg2");
    assert!(arg2.arg);
    assert_eq!(arg2.repr, Some("None".to_owned()));

    let arg3 = &locals[2];
    assert_eq!(arg3.name, "arg3");
    assert!(arg3.arg);
    assert_eq!(arg3.repr, Some("True".to_owned()));

    let local1 = &locals[3];
    assert_eq!(local1.name, "local1");
    assert!(!local1.arg);
    assert_eq!(local1.repr, Some("[-1234, 5678]".to_owned()));

    let local2 = &locals[4];
    assert_eq!(local2.name, "local2");
    assert!(!local2.arg);
    assert_eq!(local2.repr, Some("(\"a\", \"b\", \"c\")".to_owned()));

    let local3 = &locals[5];
    assert_eq!(local3.name, "local3");
    assert!(!local3.arg);

    assert_eq!(local3.repr, Some("123456789123456789".to_owned()));

    let local4 = &locals[6];
    assert_eq!(local4.name, "local4");
    assert!(!local4.arg);
    assert_eq!(local4.repr, Some("3.1415".to_owned()));

    let local5 = &locals[7];
    assert_eq!(local5.name, "local5");
    assert!(!local5.arg);

    let local6 = &locals[8];
    assert_eq!(local6.name, "local6");
    assert!(!local6.arg);

    // Numpy scalars
    let local7 = &locals[9];
    assert_eq!(local7.name, "local7");
    assert_eq!(local7.repr, Some("true".to_string()));

    let local8 = &locals[10];
    assert_eq!(local8.name, "local8");
    assert_eq!(local8.repr, Some("2".to_string()));

    let local9 = &locals[11];
    assert_eq!(local9.name, "local9");
    assert_eq!(local9.repr, Some("3".to_string()));

    let local10 = &locals[12];
    assert_eq!(local10.name, "local10");
    assert_eq!(local10.repr, Some("42".to_string()));

    let local11 = &locals[13];
    assert_eq!(local11.name, "local11");
    assert_eq!(local11.repr, Some("43".to_string()));

    let local12 = &locals[14];
    assert_eq!(local12.name, "local12");
    assert_eq!(local12.repr, Some("44".to_string()));

    let local13 = &locals[15];
    assert_eq!(local13.name, "local13");
    assert_eq!(local13.repr, Some("45".to_string()));

    let local14 = &locals[16];
    assert_eq!(local14.name, "local14");
    assert_eq!(local14.repr, Some("46".to_string()));

    let local15 = &locals[17];
    assert_eq!(local15.name, "local15");
    assert_eq!(local15.repr, Some("7".to_string()));

    let local16 = &locals[18];
    assert_eq!(local16.name, "local16");
    assert_eq!(local16.repr, Some("8".to_string()));

    fn test_repr_prefix(local: &py_spy::stack_trace::LocalVariable, expected: &str) {
        assert!(
            local
                .repr
                .as_ref()
                .map(|result| result.starts_with(expected))
                .unwrap_or(false),
            "local '{}' repr = '{:?}' doesn't start with '{}'",
            &local.name,
            &local.repr,
            expected
        );
    }

    let local17 = &locals[19];
    assert_eq!(local17.name, "local17");

    #[cfg(not(windows))]
    test_repr_prefix(local17, "<numpy.ulonglong at");

    let local18 = &locals[20];
    assert_eq!(local18.name, "local18");
    test_repr_prefix(local18, "<numpy.float16 at");

    let local19 = &locals[21];
    assert_eq!(local19.name, "local19");
    assert_eq!(local19.repr, Some("0.5".to_string()));

    let local20 = &locals[22];
    assert_eq!(local20.name, "local20");
    assert_eq!(local20.repr, Some("0.7".to_string()));

    let local21 = &locals[23];
    assert_eq!(local21.name, "local21");
    test_repr_prefix(local21, "<numpy.longdouble at");

    let local22 = &locals[24];
    assert_eq!(local22.name, "local22");
    test_repr_prefix(local22, "<numpy.complex64 at");

    let local23 = &locals[25];
    assert_eq!(local23.name, "local23");
    test_repr_prefix(local23, "<numpy.complex128 at");

    let local24 = &locals[26];
    assert_eq!(local24.name, "local24");
    test_repr_prefix(local24, "<numpy.clongdouble at");

    // https://github.com/benfred/py-spy/issues/766
    let local25 = &locals[27];
    assert_eq!(local25.name, "local25");
    let unicode_val = local25.repr.as_ref().unwrap();
    let end = unicode_val.char_indices().map(|(i, _)| i).nth(4).unwrap();
    assert_eq!(unicode_val[0..end], *"\"测试1");

    // Empty string
    let local26 = &locals[28];
    assert_eq!(local26.name, "local26");
    assert_eq!(local26.repr, Some("\"\"".to_string()));

    // we only support dictionary lookup on python 3.6+ right now
    if runner.spy.version.major == 3 && runner.spy.version.minor >= 6 {
        assert_eq!(
            local5.repr,
            Some("{\"a\": False, \"b\": (1, 2, 3)}".to_owned())
        );
    }
}

#[cfg(not(target_os = "freebsd"))]
#[test]
fn test_subprocesses() {
    #[cfg(target_os = "macos")]
    {
        // We need root permissions here to run this on OSX
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
    }

    // We used to not be able to create a sampler object if one of the child processes
    // was in a zombie state. Verify that this works now
    let process = ScriptRunner::new("python", "./tests/scripts/subprocesses.py");
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let config = Config {
        subprocesses: true,
        ..Default::default()
    };
    let sampler = py_spy::sampler::Sampler::new(process.id(), &config).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1000));

    // Get samples from all the subprocesses, verify that we got from all 3 processes
    let mut attempts = 0;

    for sample in sampler {
        // wait for other processes here if we don't have the expected number
        let traces = sample.traces;
        if traces.len() != 3 && attempts < 4 {
            attempts += 1;
            std::thread::sleep(std::time::Duration::from_millis(1000));
            continue;
        }
        assert_eq!(traces.len(), 3);
        assert!(traces[0].pid != traces[1].pid);
        assert!(traces[1].pid != traces[2].pid);
        break;
    }
}

#[cfg(not(target_os = "freebsd"))]
#[test]
fn test_subprocesses_zombiechild() {
    #[cfg(target_os = "macos")]
    {
        // We need root permissions here to run this on OSX
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
    }

    // We used to not be able to create a sampler object if one of the child processes
    // was in a zombie state. Verify that this works now
    let process = ScriptRunner::new("python", "./tests/scripts/subprocesses_zombie_child.py");
    std::thread::sleep(std::time::Duration::from_millis(200));
    let config = Config {
        subprocesses: true,
        ..Default::default()
    };
    let _sampler = py_spy::sampler::Sampler::new(process.id(), &config).unwrap();
}

#[test]
fn test_negative_linenumber_increment() {
    #[cfg(target_os = "macos")]
    {
        // We need root permissions here to run this on OSX
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
    }
    let mut runner = TestRunner::new(
        Config::default(),
        "./tests/scripts/negative_linenumber_offsets.py",
    );

    let traces = runner.spy.get_stack_traces().unwrap();
    assert_eq!(traces.len(), 1);
    let trace = &traces[0];

    // Python 3.12 inlined comprehensions - see https://peps.python.org/pep-0709/
    match (runner.spy.version.major, runner.spy.version.minor) {
        (3, 0..=11) => {
            assert_eq!(trace.frames[0].name, "<listcomp>");
            assert!(trace.frames[0].line >= 5 && trace.frames[0].line <= 10);
            assert_eq!(trace.frames[1].name, "f");
            assert!(trace.frames[1].line >= 5 && trace.frames[0].line <= 10);
            assert_eq!(trace.frames[2].name, "<module>");
            assert_eq!(trace.frames[2].line, 13)
        }
        (2, _) | (3, 12..) => {
            assert_eq!(trace.frames[0].name, "f");
            assert!(trace.frames[0].line >= 5 && trace.frames[0].line <= 10);
            assert_eq!(trace.frames[1].name, "<module>");
            assert_eq!(trace.frames[1].line, 13);
        }
        _ => panic!("Unknown python major version"),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn test_delayed_subprocess() {
    let process = ScriptRunner::new("bash", "./tests/scripts/delayed_launch.sh");
    let config = Config {
        subprocesses: true,
        ..Default::default()
    };
    let sampler = py_spy::sampler::Sampler::new(process.id(), &config).unwrap();
    for sample in sampler {
        // should have one trace from the subprocess
        let traces = sample.traces;
        assert_eq!(traces.len(), 1);
        assert!(traces[0].pid != process.id());
        break;
    }
}

fn require_root_on_macos() -> bool {
    #[cfg(target_os = "macos")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return false;
        }
    }
    true
}

fn assert_traces_equivalent(trait_traces: &[py_spy::StackTrace], offset_traces: &[py_spy::StackTrace]) {

    assert_eq!(
        trait_traces.len(),
        offset_traces.len(),
        "Thread count mismatch: trait={} offset={}",
        trait_traces.len(),
        offset_traces.len()
    );

    for (t_trace, o_trace) in trait_traces.iter().zip(offset_traces.iter()) {
        assert_eq!(
            t_trace.thread_id, o_trace.thread_id,
            "Thread ID mismatch"
        );
        assert_eq!(
            t_trace.os_thread_id, o_trace.os_thread_id,
            "OS thread ID mismatch for thread {}",
            t_trace.thread_id
        );
        assert_eq!(
            t_trace.owns_gil, o_trace.owns_gil,
            "GIL ownership mismatch for thread {}",
            t_trace.thread_id
        );
        assert_eq!(
            t_trace.frames.len(),
            o_trace.frames.len(),
            "Frame count mismatch for thread {}: trait={} offset={}.\n  trait frames: {:?}\n  offset frames: {:?}",
            t_trace.thread_id,
            t_trace.frames.len(),
            o_trace.frames.len(),
            t_trace.frames.iter().map(|f| format!("{}:{}", f.filename, f.name)).collect::<Vec<_>>(),
            o_trace.frames.iter().map(|f| format!("{}:{}", f.filename, f.name)).collect::<Vec<_>>(),
        );

        for (i, (t_frame, o_frame)) in
            t_trace.frames.iter().zip(o_trace.frames.iter()).enumerate()
        {
            assert_eq!(
                t_frame.filename, o_frame.filename,
                "Filename mismatch at frame {} of thread {}",
                i, t_trace.thread_id
            );
            assert_eq!(
                t_frame.name, o_frame.name,
                "Function name mismatch at frame {} of thread {}",
                i, t_trace.thread_id
            );
            assert_eq!(
                t_frame.line, o_frame.line,
                "Line number mismatch at frame {} ({}.{}) of thread {}",
                i, t_frame.filename, t_frame.name, t_trace.thread_id
            );
            assert_eq!(
                t_frame.is_entry, o_frame.is_entry,
                "is_entry mismatch at frame {} of thread {}",
                i, t_trace.thread_id
            );
        }
    }
}

/// Run an offset-vs-trait oracle comparison with the process suspended so both
/// paths see identical interpreter state.
fn run_offset_oracle(runner: &mut TestRunner) {
    if runner.spy.version.minor < 13 {
        eprintln!(
            "Skipping: Python {}.{} < 3.13",
            runner.spy.version.major, runner.spy.version.minor
        );
        return;
    }
    // Suspend the process externally so both reads see the same state.
    // Override blocking to NonBlocking so the internal get_stack_traces
    // doesn't try to ptrace-attach (which would conflict with our lock).
    let _lock = match runner.spy.process.lock() {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("Skipping oracle test: cannot lock process ({})", e);
            return;
        }
    };
    runner.spy.config.blocking = py_spy::config::LockingStrategy::NonBlocking;
    let trait_traces = runner.spy.get_stack_traces().unwrap();
    let offset_traces = runner.spy.get_stack_traces_via_offsets().unwrap();
    drop(_lock);
    assert_traces_equivalent(&trait_traces, &offset_traces);
}

#[test]
fn test_offset_oracle_longsleep() {
    if !require_root_on_macos() {
        return;
    }
    let mut runner = TestRunner::new(Config::default(), "./tests/scripts/longsleep.py");
    run_offset_oracle(&mut runner);
}

#[test]
fn test_offset_oracle_busyloop() {
    if !require_root_on_macos() {
        return;
    }
    let mut runner = TestRunner::new(Config::default(), "./tests/scripts/busyloop.py");
    run_offset_oracle(&mut runner);
}

#[test]
fn test_offset_oracle_threads() {
    if !require_root_on_macos() {
        return;
    }
    let mut runner = TestRunner::new(Config::default(), "./tests/scripts/thread_names.py");
    run_offset_oracle(&mut runner);
}

#[test]
fn test_offset_oracle_recursive() {
    if !require_root_on_macos() {
        return;
    }
    let mut runner = TestRunner::new(Config::default(), "./tests/scripts/recursive.py");
    run_offset_oracle(&mut runner);
}

const PYTHON314: &str = "python3.14";

fn python314_available() -> bool {
    std::process::Command::new(PYTHON314)
        .arg("--version")
        .output()
        .is_ok()
}

#[test]
fn test_314_longsleep() {
    if !require_root_on_macos() {
        return;
    }
    if !python314_available() {
        eprintln!("Skipping: {} not found in PATH", PYTHON314);
        return;
    }
    let child = ScriptRunner::new(PYTHON314, "./tests/scripts/longsleep.py");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut spy = PythonSpy::retry_new(child.id(), &Config::default(), 20).unwrap();
    assert_eq!(spy.version.minor, 14);

    let traces = spy.get_stack_traces().unwrap();
    assert_eq!(traces.len(), 1);
    let trace = &traces[0];
    assert_eq!(trace.frames[0].name, "longsleep");
    assert!(trace.frames[0].filename.contains("longsleep.py"));
    assert_eq!(trace.frames[0].line, 5);
    assert_eq!(trace.frames[1].name, "<module>");
}

#[test]
fn test_314_busyloop() {
    if !require_root_on_macos() {
        return;
    }
    if !python314_available() {
        eprintln!("Skipping: {} not found in PATH", PYTHON314);
        return;
    }
    let child = ScriptRunner::new(PYTHON314, "./tests/scripts/busyloop.py");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut spy = PythonSpy::retry_new(child.id(), &Config::default(), 20).unwrap();
    assert_eq!(spy.version.minor, 14);

    let traces = spy.get_stack_traces().unwrap();
    assert!(!traces.is_empty());
    assert!(traces[0].active);
    // Verify we get valid frames with expected filenames
    assert!(traces[0].frames.iter().any(|f| f.filename.contains("busyloop.py")));
}

#[test]
fn test_314_threads() {
    if !require_root_on_macos() {
        return;
    }
    if !python314_available() {
        eprintln!("Skipping: {} not found in PATH", PYTHON314);
        return;
    }
    let child = ScriptRunner::new(PYTHON314, "./tests/scripts/thread_names.py");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut spy = PythonSpy::retry_new(child.id(), &Config::default(), 20).unwrap();
    assert_eq!(spy.version.minor, 14);

    let traces = spy.get_stack_traces().unwrap();
    // thread_names.py creates 10 threads + main = 11
    assert!(traces.len() >= 2, "Expected multiple threads, got {}", traces.len());
}

const PYTHON314T: &str = "python3.14t";

fn python314t_available() -> bool {
    std::process::Command::new(PYTHON314T)
        .arg("--version")
        .output()
        .is_ok()
}

#[test]
fn test_314t_longsleep() {
    if !require_root_on_macos() {
        return;
    }
    if !python314t_available() {
        eprintln!("Skipping: {} not found in PATH", PYTHON314T);
        return;
    }
    let child = ScriptRunner::new(PYTHON314T, "./tests/scripts/longsleep.py");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut spy = PythonSpy::retry_new(child.id(), &Config::default(), 20).unwrap();
    assert_eq!(spy.version.minor, 14);

    let traces = spy.get_stack_traces().unwrap();
    assert_eq!(traces.len(), 1);
    let trace = &traces[0];
    assert_eq!(trace.frames[0].name, "longsleep");
    assert!(trace.frames[0].filename.contains("longsleep.py"));
    assert_eq!(trace.frames[0].line, 5);
    assert_eq!(trace.frames[1].name, "<module>");
    // In free-threaded builds with GIL disabled, no thread owns the GIL
    assert!(!trace.owns_gil);
}

#[test]
fn test_314t_busyloop() {
    if !require_root_on_macos() {
        return;
    }
    if !python314t_available() {
        eprintln!("Skipping: {} not found in PATH", PYTHON314T);
        return;
    }
    let child = ScriptRunner::new(PYTHON314T, "./tests/scripts/busyloop.py");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut spy = PythonSpy::retry_new(child.id(), &Config::default(), 20).unwrap();
    assert_eq!(spy.version.minor, 14);

    let traces = spy.get_stack_traces().unwrap();
    assert!(!traces.is_empty());
    assert!(traces[0].frames.iter().any(|f| f.filename.contains("busyloop.py")));
}

#[test]
fn test_314t_threads() {
    if !require_root_on_macos() {
        return;
    }
    if !python314t_available() {
        eprintln!("Skipping: {} not found in PATH", PYTHON314T);
        return;
    }
    let child = ScriptRunner::new(PYTHON314T, "./tests/scripts/thread_names.py");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut spy = PythonSpy::retry_new(child.id(), &Config::default(), 20).unwrap();
    assert_eq!(spy.version.minor, 14);

    let traces = spy.get_stack_traces().unwrap();
    assert!(traces.len() >= 2, "Expected multiple threads, got {}", traces.len());
    // No thread should own the GIL in a free-threaded build
    for trace in &traces {
        assert!(!trace.owns_gil, "Thread {} unexpectedly owns GIL in free-threaded build", trace.thread_id);
    }
}

#[test]
fn test_314t_thread_names() {
    if !require_root_on_macos() {
        return;
    }
    if !python314t_available() {
        eprintln!("Skipping: {} not found in PATH", PYTHON314T);
        return;
    }
    let config = Config {
        include_idle: true,
        ..Default::default()
    };
    let child = ScriptRunner::new(PYTHON314T, "./tests/scripts/thread_names.py");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut spy = PythonSpy::retry_new(child.id(), &config, 20).unwrap();
    assert_eq!(spy.version.minor, 14);

    let traces = spy.get_stack_traces().unwrap();
    assert_eq!(traces.len(), 11, "Expected 11 threads (main + 10 custom)");

    let mut expected_threads: HashSet<String> =
        (0..10).map(|n| format!("CustomThreadName-{}", n)).collect();
    expected_threads.insert("MainThread".to_string());
    let detected_threads: HashSet<String> = traces
        .iter()
        .filter_map(|trace| trace.thread_name.clone())
        .collect();
    assert_eq!(expected_threads, detected_threads);
}

#[test]
fn test_314t_recursive() {
    if !require_root_on_macos() {
        return;
    }
    if !python314t_available() {
        eprintln!("Skipping: {} not found in PATH", PYTHON314T);
        return;
    }
    let child = ScriptRunner::new(PYTHON314T, "./tests/scripts/recursive.py");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut spy = PythonSpy::retry_new(child.id(), &Config::default(), 20).unwrap();

    let traces = spy.get_stack_traces().unwrap();
    assert!(!traces.is_empty());
    assert!(traces[0].frames.len() >= 2, "Expected recursive frames");
    assert!(traces[0].frames.iter().any(|f| f.name == "recurse"));
}
