use std::env;

fn main() {
    // We only support native unwinding on x86_64 platforms
    if env::var("CARGO_CFG_TARGET_ARCH").unwrap() != "x86_64" {
        return;
    }

    match env::var("CARGO_CFG_TARGET_OS").unwrap().as_ref() {
        "linux" => {
            // statically link libunwind if compiling for musl, dynamically link otherwise
            if env::var("CARGO_FEATURE_UNWIND").is_ok() {
                if env::var("CARGO_CFG_TARGET_ENV").unwrap() == "musl" {
                    println!("cargo:rustc-link-search=native=/usr/local/lib");
                    println!("cargo:rustc-link-lib=static=unwind");
                    println!("cargo:rustc-link-lib=static=unwind-ptrace");
                    println!("cargo:rustc-link-lib=static=unwind-x86_64");
                } else {
                    println!("cargo:rustc-link-lib=dylib=unwind");
                    println!("cargo:rustc-link-lib=dylib=unwind-ptrace");
                    println!("cargo:rustc-link-lib=dylib=unwind-x86_64");
                }
            }
        },
        _ => { }
    }
}
