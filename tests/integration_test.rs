extern crate py_spy;

use py_spy::{Config, PythonSpy, Pid};

struct TestRunner {
    child: std::process::Child,
    spy: PythonSpy
}

impl TestRunner {
    fn new(filename: &str) -> TestRunner {
        let mut child = std::process::Command::new("python").arg(filename).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));

        let config = Config::default();
        let mut spy = PythonSpy::retry_new(child.id() as _, &config, 20).unwrap();

        TestRunner{child, spy}
    }
}

impl Drop for TestRunner {
    fn drop(&mut self) {
        self.child.kill().unwrap();
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
    let mut runner = TestRunner::new("./tests/scripts/busyloop.py");
    let traces = runner.spy.get_stack_traces().unwrap();

    // we can't be guaranteed what line the script is processing, but
    // we should be able to say that the script is active and
    // catch issues like https://github.com/benfred/py-spy/issues/141
    assert!(traces[0].active);
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

    let mut runner = TestRunner::new("./tests/scripts/longsleep.py");

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

    // we will only know this thread is sleeping in certain cases,
    // and having unwind support is a reasonable proxy for that
    #[cfg(unwind)]
    assert!(!traces[0].active);
}
