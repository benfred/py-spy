#[cfg(unwind)]
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
