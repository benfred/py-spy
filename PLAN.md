# Plan for investigating `test_thread_names` failure on Python 3.14
1. Re-run `cargo test test_thread_names -- --nocapture` under the Python 3.14 virtualenv to confirm the failure and capture full output.
2. Inspect the thread name resolution path in `src/python_threading.rs` and related data access for version-specific logic covering Python 3.14.
3. Compare thread state and dictionary layouts for Python 3.14 (bindings in `src/python_bindings/v3_14_0.rs`) against the versions currently handled to spot missing fields or offsets.
4. Instrument or log (via targeted debug assertions or temporary prints) the thread name lookup to see what data is returned for Python 3.14 during the failing test.
5. Identify the root cause and sketch the minimal code change needed to restore thread name collection on 3.14.
