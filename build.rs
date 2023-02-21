use std::env;

fn main() {
    if env::var("CARGO_CFG_TARGET_ARCH").unwrap() != "x86_64" {
        return;
    }

    match env::var("CARGO_CFG_TARGET_OS").unwrap().as_ref() {
        "windows" => println!("cargo:rustc-cfg=unwind"),
        "linux" => println!("cargo:rustc-cfg=unwind"),
        _ => {}
    }
}
