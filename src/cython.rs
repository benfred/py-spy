use regex::Regex;
use std::collections::{BTreeMap, HashMap};

use anyhow::Error;
use lazy_static::lazy_static;

use crate::stack_trace::Frame;
use crate::utils::resolve_filename;

pub struct SourceMaps {
    maps: HashMap<String, Option<SourceMap>>,
}

impl SourceMaps {
    pub fn new() -> SourceMaps {
        let maps = HashMap::new();
        SourceMaps { maps }
    }

    pub fn translate(&mut self, frame: &mut Frame) {
        if self.translate_frame(frame) {
            self.load_map(frame);
            self.translate_frame(frame);
        }
    }

    // tries to replace the frame using a cython sourcemap if possible
    // returns true if the corresponding cython sourcemap hasn't been loaded yet
    fn translate_frame(&mut self, frame: &mut Frame) -> bool {
        let line = frame.line as u32;
        if line == 0 {
            return false;
        }
        if let Some(map) = self.maps.get(&frame.filename) {
            if let Some(map) = map {
                if let Some((file, line)) = map.lookup(line) {
                    frame.filename = file.clone();
                    frame.line = *line as i32;
                }
            }
            return false;
        }
        true
    }

    // loads the corresponding cython source map for the frame
    fn load_map(&mut self, frame: &Frame) {
        if !(frame.filename.ends_with(".cpp") || frame.filename.ends_with(".c")) {
            self.maps.insert(frame.filename.clone(), None);
            return;
        }

        let map = match SourceMap::new(&frame.filename, &frame.module) {
            Ok(map) => map,
            Err(e) => {
                info!("Failed to load cython file {}: {:?}", &frame.filename, e);
                self.maps.insert(frame.filename.clone(), None);
                return;
            }
        };

        self.maps.insert(frame.filename.clone(), Some(map));
    }
}

struct SourceMap {
    lookup: BTreeMap<u32, (String, u32)>,
}

impl SourceMap {
    pub fn new(filename: &str, module: &Option<String>) -> Result<SourceMap, Error> {
        let contents = std::fs::read_to_string(filename)?;
        SourceMap::from_contents(&contents, filename, module)
    }

    pub fn from_contents(
        contents: &str,
        cpp_filename: &str,
        module: &Option<String>,
    ) -> Result<SourceMap, Error> {
        lazy_static! {
            static ref RE: Regex = Regex::new(r#"^\s*/\* "(.+\..+)":([0-9]+)"#).unwrap();
        }

        let mut lookup = BTreeMap::new();
        let mut resolved: HashMap<String, String> = HashMap::new();

        let mut line_count = 0;
        for (lineno, line) in contents.lines().enumerate() {
            if let Some(captures) = RE.captures(line) {
                let cython_file = captures.get(1).map_or("", |m| m.as_str());
                let cython_line = captures.get(2).map_or("", |m| m.as_str());

                if let Ok(cython_line) = cython_line.parse::<u32>() {
                    // try resolving the cython filename
                    let filename = match resolved.get(cython_file) {
                        Some(filename) => filename.clone(),
                        None => {
                            let filename = resolve_cython_file(cpp_filename, cython_file, module);
                            resolved.insert(cython_file.to_string(), filename.clone());
                            filename
                        }
                    };

                    lookup.insert(lineno as u32, (filename, cython_line));
                }
            }
            line_count += 1;
        }

        lookup.insert(line_count + 1, ("".to_owned(), 0));
        Ok(SourceMap { lookup })
    }

    pub fn lookup(&self, lineno: u32) -> Option<&(String, u32)> {
        match self.lookup.range(..lineno).next_back() {
            // handle EOF
            Some((_, (_, 0))) => None,
            Some((_, val)) => Some(val),
            None => None,
        }
    }
}

pub fn ignore_frame(name: &str) -> bool {
    let ignorable = [
        "__Pyx_PyFunction_FastCallDict",
        "__Pyx_PyObject_CallOneArg",
        "__Pyx_PyObject_Call",
        "__pyx_FusedFunction_call",
    ];

    ignorable.iter().any(|&f| f == name)
}

pub fn demangle(name: &str) -> &str {
    // slice off any leading cython prefix.
    let prefixes = [
        "__pyx_fuse_1_0__pyx_pw",
        "__pyx_fuse_0__pyx_f",
        "__pyx_fuse_1__pyx_f",
        "__pyx_pf",
        "__pyx_pw",
        "__pyx_f",
        "___pyx_f",
        "___pyx_pw",
    ];
    let mut current = match prefixes.iter().find(|&prefix| name.starts_with(prefix)) {
        Some(prefix) => &name[prefix.len()..],
        None => return name,
    };

    let mut next = current;

    // get the function name from the cython mangled string (removing module/file/class
    // prefixes)
    loop {
        let mut chars = next.chars();
        if chars.next() != Some('_') {
            break;
        }

        let mut digit_index = 1;
        for ch in chars {
            if !ch.is_ascii_digit() {
                break;
            }
            digit_index += 1;
        }

        if digit_index == 1 {
            break;
        }

        match &next[1..digit_index].parse::<usize>() {
            Ok(digits) => {
                current = &next[digit_index..];
                if digits + digit_index >= current.len() {
                    break;
                }
                next = &next[digits + digit_index..];
            }
            Err(_) => break,
        };
    }
    debug!("cython_demangle(\"{}\") -> \"{}\"", name, current);

    current
}

fn resolve_cython_file(
    cpp_filename: &str,
    cython_filename: &str,
    module: &Option<String>,
) -> String {
    let cython_path = std::path::PathBuf::from(cython_filename);
    if let Some(ext) = cython_path.extension() {
        let mut path_buf = std::path::PathBuf::from(cpp_filename);
        path_buf.set_extension(ext);
        if path_buf.ends_with(&cython_path) && path_buf.exists() {
            return path_buf.to_string_lossy().to_string();
        }
    }

    match module {
        Some(module) => {
            resolve_filename(cython_filename, module).unwrap_or_else(|| cython_filename.to_owned())
        }
        None => cython_filename.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_demangle() {
        // all of these were wrong at certain points when writing cython_demangle =(
        assert_eq!(
            demangle("__pyx_pf_8implicit_4_als_30_least_squares_cg"),
            "_least_squares_cg"
        );
        assert_eq!(
            demangle("__pyx_pw_8implicit_4_als_5least_squares_cg"),
            "least_squares_cg"
        );
        assert_eq!(
            demangle("__pyx_fuse_1_0__pyx_pw_8implicit_4_als_31_least_squares_cg"),
            "_least_squares_cg"
        );
        assert_eq!(
            demangle("__pyx_f_6mtrand_cont0_array"),
            "mtrand_cont0_array"
        );
        // in both of these cases we should ideally slice off the module (_als/bpr), but it gets tricky
        // implementation wise
        assert_eq!(
            demangle("__pyx_fuse_0__pyx_f_8implicit_4_als_axpy"),
            "_als_axpy"
        );
        assert_eq!(
            demangle("__pyx_fuse_1__pyx_f_8implicit_3bpr_has_non_zero"),
            "bpr_has_non_zero"
        );
    }

    #[test]
    fn test_source_map() {
        let map = SourceMap::from_contents(
            include_str!("../ci/testdata/cython_test.c"),
            "cython_test.c",
            &None,
        )
        .unwrap();

        // we don't have info on cython line numbers until line 1261
        assert_eq!(map.lookup(1000), None);
        // past the end of the file should also return none
        assert_eq!(map.lookup(10000), None);

        let lookup = |lineno: u32, cython_file: &str, cython_line: u32| match map.lookup(lineno) {
            Some((file, line)) => {
                assert_eq!(file, cython_file);
                assert_eq!(line, &cython_line);
            }
            None => {
                panic!(
                    "Failed to lookup line {} (expected {}:{})",
                    lineno, cython_file, cython_line
                );
            }
        };
        lookup(1298, "cython_test.pyx", 6);
        lookup(1647, "cython_test.pyx", 10);
        lookup(1763, "cython_test.pyx", 9);
    }
}
