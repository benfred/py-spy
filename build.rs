use std::env;

fn main() {
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    println!("cargo:rustc-cfg=unwind");
    println!("cargo:warning=Building for target architecture: {}", target_arch);
}
