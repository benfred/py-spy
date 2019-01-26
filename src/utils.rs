use std;
use std::time::{Instant, Duration};
#[cfg(windows)]
use winapi::um::timeapi;

use read_process_memory::{CopyAddress};

/// Copies a struct from another process
pub fn copy_struct<T, P>(addr: usize, process: &P) -> std::io::Result<T>
    where P: CopyAddress {
    let mut data = vec![0; std::mem::size_of::<T>()];
    process.copy_address(addr, &mut data)?;
    Ok(unsafe { std::ptr::read(data.as_ptr() as *const _) })
}

/// Given a pointer that points to a struct in another process, returns the
/// struct from the other process
pub fn copy_pointer<T, P>(ptr: *const T, process: &P) -> std::io::Result<T>
    where P: CopyAddress {
    copy_struct(ptr as usize, process)
}

/// Timer is an iterator that sleeps an appropiate amount of time so that
/// each loop happens at a constant rate.
pub struct Timer {
    rate: Duration,
    start: Instant,
    samples: u32,
}

impl Timer {
    pub fn new(rate: Duration) -> Timer {
        // This changes a system-wide setting on Windows so that the OS wakes up every 1ms
        // instead of the default 15.6ms. This is required to have a sleep call
        // take less than 15ms, which we need since we usually profile at more than 64hz.
        // The downside is that this will increase power usage: good discussions are:
        // https://randomascii.wordpress.com/2013/07/08/windows-timer-resolution-megawatts-wasted/
        // and http://www.belshe.com/2010/06/04/chrome-cranking-up-the-clock/
        #[cfg(windows)]
        unsafe { timeapi::timeBeginPeriod(1); }

        Timer{rate, samples: 0, start: Instant::now()}
    }
}

impl Iterator for Timer {
    type Item = Result<Duration, Duration>;

    fn next(&mut self) -> Option<Self::Item> {
        self.samples += 1;
        let elapsed = self.start.elapsed();
        let desired = self.rate * self.samples;
        if desired > elapsed {
            std::thread::sleep(desired - elapsed);
            Some(Ok(desired - elapsed))
        } else {
            Some(Err(elapsed - desired))
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe { timeapi::timeEndPeriod(1); }
    }
}

#[cfg(unix)]
pub fn resolve_filename(filename: &str, modulename: &str) -> Option<String> {
    // check the filename first, if it exists use it
    use std::path::Path;
    let path = Path::new(filename);
    if path.exists() {
        return Some(filename.to_owned());
    }

    // try resolving relative the shared library the file is in
    let module = Path::new(modulename);
    if let Some(parent) = module.parent() {
        if let Some(name) = path.file_name() {
        let temp = parent.join(name);
            if temp.exists() {
                return Some(temp.to_string_lossy().to_owned().to_string())
            }
        }
    }

    None
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Mock for using CopyAddress on the local process.
    pub struct LocalProcess;
    impl CopyAddress for LocalProcess {
        fn copy_address(&self, addr: usize, buf: &mut [u8]) -> std::io::Result<()> {
            unsafe {
                std::ptr::copy_nonoverlapping(addr as *mut u8, buf.as_mut_ptr(), buf.len());
            }
            Ok(())
        }
    }

    struct Point { x: i32, y: i64 }

    #[test]
    fn test_copy_pointer() {
        let original = Point{x:15, y:25};
        let copy = copy_pointer(&original, &LocalProcess).unwrap();
        assert_eq!(original.x, copy.x);
        assert_eq!(original.y, copy.y);
    }

    #[test]
    fn test_copy_struct() {
        let original = Point{x:10, y:20};
        let copy: Point = copy_struct(&original as *const Point as usize, &LocalProcess).unwrap();
        assert_eq!(original.x, copy.x);
        assert_eq!(original.y, copy.y);
    }
}
