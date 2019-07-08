// This example loops over native stack traces until it fails to get one for any reason
extern crate remoteprocess;
extern crate env_logger;
#[macro_use]
extern crate log;
extern crate failure;
extern crate py_spy;

fn native_stress_test(pid: remoteprocess::Pid) -> Result<(), failure::Error> {

    let config = py_spy::Config{native: true, ..Default::default() };
    let mut spy = py_spy::PythonSpy::retry_new(pid, &config, 3)?;


    let mut success = 0;
    let mut failed = 0;
    loop {
        match spy.get_stack_traces() {
            Ok(_) => {
                success += 1;
                if success % 1000 == 0 {
                    info!("Success {} fail {}", success, failed)
                }
            },
            Err(e) => {
                error!("Failed to get stack traces: {:#?}", e);
                for (_i, suberror) in e.iter_chain().enumerate() {
                    eprintln!("Reason: {:?}", suberror);
                }

                failed += 1;
                info!("Success {} fail {}", success, failed);
           }
        }
    }
    Ok(())
}


#[cfg(unwind)]
fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();

    let pid = if args.len() > 1 {
        args[1].parse::<remoteprocess::Pid>().expect("invalid pid")
    } else {
        error!("must specify a pid!");
        return;
    };

    if let Err(e) = native_stress_test(pid) {
        println!("Failed to get backtrace {:?}", e);
    }
}

#[cfg(not(unwind))]
fn main() {
    panic!("unwind not supported!");
}
