use std::env;

fn main() {
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    match (target_arch.as_ref(), target_os.as_ref()) {
        ("x86_64", "windows") | ("x86_64", "linux") | ("arm", "linux") | ("aarch64", "linux") => {
            println!("cargo:rustc-cfg=unwind")
        }
        _ => {}
    }
}
