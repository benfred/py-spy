extern crate py_spy;

use py_spy::{Config, PythonSpy};

struct TestRunner {
    child: std::process::Child,
    spy: PythonSpy
}

impl TestRunner {
    fn new(config: Config, filename: &str) -> TestRunner {
        let child = std::process::Command::new("python").arg(filename).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        let spy = PythonSpy::retry_new(child.id() as _, &config, 20).unwrap();

        TestRunner{child, spy}
    }
}

impl Drop for TestRunner {
    fn drop(&mut self) {
        if let Err(err) = self.child.kill() {
            eprintln!("Failed to kill child process {}", err);
        }
    }
}

#[test]
fn test_busy_loop() {
    #[cfg(target_os="macos")]
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

#[cfg(unwind)]
#[test]
fn test_thread_reuse() {
    // on linux we had an issue with the pthread -> native thread id caching
    // the problem was that the pthreadids were getting re-used,
    // and this caused errors on native unwind (since the native thread had
    // exitted). Test that this works with a simple script that creates
    // a couple short lived threads, and then profiling with native enabled
    let config = Config{native: true, ..Default::default()};
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
    #[cfg(target_os="macos")]
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
    assert_eq!(trace.frames[0].filename, "./tests/scripts/longsleep.py");
    assert_eq!(trace.frames[0].line, 5);

    assert_eq!(trace.frames[1].name, "<module>");
    assert_eq!(trace.frames[1].line, 9);
    assert_eq!(trace.frames[0].filename, "./tests/scripts/longsleep.py");

    assert!(!traces[0].owns_gil);

    // we should reliably be able to detect the thread is sleeping on osx/windows
    // linux+freebsd is trickier
    #[cfg(any(target_os="macos", target_os="windows"))]
    assert!(!traces[0].active);
}
