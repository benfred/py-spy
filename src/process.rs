use std;
use failure::Error;
use read_process_memory::{Pid};

#[cfg(target_os = "macos")]
mod os_impl {
    use super::*;
    use libproc::libproc::proc_pid::pidpath;
    pub fn get_exe(pid: Pid) -> Result<String, Error> {
        pidpath(pid).map_err(|e| format_err!("proc_pidpath failed: {}", e))
    }
}

#[cfg(target_os = "linux")]
mod os_impl {
    use super::*;
    pub fn get_exe(pid: Pid) -> Result<String, Error> {
        let path = std::fs::read_link(format!("/proc/{}/exe", pid))?;
        Ok(path.to_string_lossy().to_string())
    }
}

#[cfg(windows)]
mod os_impl {
    use super::*;
    use winapi::um::processthreadsapi::OpenProcess;
    use winapi::um::winnt::{PROCESS_QUERY_INFORMATION, WCHAR};
    use winapi::shared::minwindef::{FALSE, DWORD, MAX_PATH};
    use winapi::um::handleapi::{INVALID_HANDLE_VALUE, CloseHandle};
    use winapi::um::winbase::QueryFullProcessImageNameW;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStringExt};

    pub fn get_exe(pid: Pid) -> Result<String, Error> {
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
}

pub use self::os_impl::*;