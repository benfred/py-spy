use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    // enable unwind code for supported platforms
    if env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "x86_64" {
        match env::var("CARGO_CFG_TARGET_OS").unwrap().as_ref() {
            "windows" => println!("cargo:rustc-cfg=unwind"),
            "linux" => println!("cargo:rustc-cfg=unwind"),
            _ => { }
        }
    }

    if env::var("CARGO_FEATURE_SERVE").is_ok() {
        // Rest of this generates a js bundle of our visualizations using npm / rollup
        let visualization_dir = Path::new("src").join("web_viewer").join("visualizations");
        Command::new("npm").args(&["install"])
                           .current_dir(&visualization_dir)
                           .status().expect("Failed to call npm install");

        let build_target = format!("build:{}", env::var("PROFILE").unwrap());
        Command::new("npm").args(&["run", &build_target])
                              .current_dir(&visualization_dir)
                              .status().expect("Failed to run npm run build");

        // rerun if anything changes in the vis directory
        for entry in std::fs::read_dir(&visualization_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                println!("cargo:rerun-if-changed={}", path.to_string_lossy());
            }
        }

        // proxy PROFILE environment variable so that we can use in rustembed
        println!("cargo:rustc-env=PROFILE={}", env::var("PROFILE").unwrap());
    }
}
