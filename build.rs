use std::env;

fn main() {
    // copied from remoteprocess/build.rs because I couldn't find a way to share this
    // We only support native unwinding on x86_64 platforms
    if env::var("CARGO_CFG_TARGET_ARCH").unwrap() != "x86_64" {
        return;
    }

    match env::var("CARGO_CFG_TARGET_OS").unwrap().as_ref() {
        "windows" => println!("cargo:rustc-cfg=unwind"),
        "linux" => println!("cargo:rustc-cfg=unwind"),
        _ => { }
    }
}
