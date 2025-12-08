# Cargo test results with Python 3.14

Environment: Python 3.14.0 virtualenv (`.venv`) via pyenv. Installing `numpy` with `pip install numpy` failed because the proxy blocked package downloads (403 Forbidden).

Command: `cargo test`

Failures observed:
- `test_local_vars` (integration_test): missing `numpy` in test script led to `ModuleNotFoundError`, followed by failure to read process executable name (`No such file or directory`).
- `test_delayed_subprocess` (integration_test): expected Python subprocesses were not found for the target PID.
- `test_thread_names` (integration_test): panicked on `None` unwrap when reading thread names.
- `test_subprocesses` (integration_test): subprocess count assertion failed (left: 1, right: 3).

Other tests passed (all unit tests and remaining integration tests).
