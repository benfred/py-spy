use std;
use std::time::{Instant, Duration};
#[cfg(windows)]
use winapi::um::timeapi;
use failure::Error;

use read_process_memory::{CopyAddress, Pid};

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

#[cfg(target_os = "macos")]
pub fn get_process_exe(pid: Pid) -> Result<String, Error> {
    use libproc::libproc::proc_pid::pidpath;
    pidpath(pid).map_err(|e| format_err!("proc_pidpath failed: {}", e))
}

#[cfg(target_os = "linux")]
pub fn get_process_exe(pid: Pid) -> Result<String, Error> {
    let path = std::fs::read_link(format!("/proc/{}/exe", pid))?;
    Ok(path.to_string_lossy().to_string())
}

#[cfg(windows)]
pub fn get_process_exe(pid: Pid) -> Result<String, Error> {
    use winapi::um::processthreadsapi::OpenProcess;
    use winapi::um::winnt::{PROCESS_QUERY_INFORMATION, WCHAR};
    use winapi::shared::minwindef::{FALSE, DWORD, MAX_PATH};
    use winapi::um::handleapi::{INVALID_HANDLE_VALUE, CloseHandle};
    use winapi::um::winbase::QueryFullProcessImageNameW;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStringExt};

    unsafe {
        let process = OpenProcess(PROCESS_QUERY_INFORMATION, FALSE, pid as DWORD);
        if process == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }

        let mut size = MAX_PATH as DWORD;
        let mut filename: [WCHAR; MAX_PATH] = std::mem::zeroed();
        let ret = QueryFullProcessImageNameW(process, 0, filename.as_mut_ptr(), &mut size);
        CloseHandle(process);

        if ret == 0 {
            return Err(std::io::Error::last_os_error().into());
        }

        Ok(OsString::from_wide(&filename[0..size as usize]).to_string_lossy().into_owned())
    }
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
