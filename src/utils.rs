use num_traits::{CheckedAdd, Zero};
use std::ops::Add;

#[cfg(feature = "unwind")]
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
                return Some(temp.to_string_lossy().to_string());
            }
        }
    }

    None
}

pub fn is_subrange<T: Eq + Ord + Add + CheckedAdd + Zero>(
    start: T,
    size: T,
    sub_start: T,
    sub_size: T,
) -> bool {
    !size.is_zero()
        && !sub_size.is_zero()
        && start.checked_add(&size).is_some()
        && sub_start.checked_add(&sub_size).is_some()
        && sub_start >= start
        && sub_start + sub_size <= start + size
}

pub fn offset_of<T, M>(object: *const T, member: *const M) -> usize {
    member as usize - object as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_subrange() {
        assert!(is_subrange(
            0u64,
            0xffff_ffff_ffff_ffff,
            0,
            0xffff_ffff_ffff_ffff
        ));
        assert!(is_subrange(0, 1, 0, 1));
        assert!(is_subrange(0, 100, 0, 10));
        assert!(is_subrange(0, 100, 90, 10));

        assert!(!is_subrange(0, 0, 0, 0));
        assert!(!is_subrange(1, 0, 0, 0));
        assert!(!is_subrange(1, 0, 1, 0));
        assert!(!is_subrange(0, 0, 0, 1));
        assert!(!is_subrange(0, 0, 1, 0));
        assert!(!is_subrange(
            1u64,
            0xffff_ffff_ffff_ffff,
            0,
            0xffff_ffff_ffff_ffff
        ));
        assert!(!is_subrange(
            0u64,
            0xffff_ffff_ffff_ffff,
            1,
            0xffff_ffff_ffff_ffff
        ));
        assert!(!is_subrange(0, 10, 0, 11));
        assert!(!is_subrange(0, 10, 1, 10));
        assert!(!is_subrange(0, 10, 9, 2));
    }
}
