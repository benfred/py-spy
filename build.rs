use std::env;

fn main() {
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    match (target_arch.as_ref(), target_os.as_ref()) {
        ("x86_64", "windows") | ("x86_64", "linux") | ("arm", "linux") => {
            println!("cargo:rustc-cfg=unwind")
        }
        _ => {}
    }

    // Only compile protobuf if pprof feature is enabled
    #[cfg(feature = "pprof")]
    {
        // Use bundled protoc from protobuf-src
        std::env::set_var("PROTOC", protobuf_src::protoc());

        // Add prost-build configuration
        prost_build::compile_protos(&["src/pprof/profile.proto"], &["src/pprof/"])
            .expect("Failed to compile protos");
    }
}
