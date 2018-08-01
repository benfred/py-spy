use std;
use read_process_memory::CopyAddress;

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
