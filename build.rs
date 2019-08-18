use std::env;

fn main() {
    // copied from remoteprocess/build.rs because I couldn't find a way to share this
    match env::var("CARGO_CFG_TARGET_OS").unwrap().as_ref() {
        "windows" => println!("cargo:rustc-cfg=unwind"),
        "macos" => {
            // OSX native profiling doesn't work all that well right now, and
            // its broken enough that I don't want to support it at the moment.
            // only enable if a specific env variable is set
            if std::env::var("PYSPY_ALLOW_NATIVE_PROFILING").is_ok() {
                println!("cargo:rustc-cfg=unwind");
            }
        },
        "linux" => {
            // We only support native unwinding on x86_64 linux
            if env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "x86_64"{
                println!("cargo:rustc-cfg=unwind");
            }
        },
        _ => { }
    }
}
