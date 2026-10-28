use std::ffi::CStr;

use anyhow::{Context, Error, Result};
use remoteprocess::ProcessMemory;

use crate::config::Config;
use crate::python_bindings::{
    v2_7_15, v3_10_0, v3_11_0, v3_12_0, v3_13_0, v3_14_0, v3_15_0, v3_3_7, v3_5_5, v3_6_6, v3_7_0,
    v3_8_0, v3_9_5,
};
use crate::python_interpreters::{CodeObject, InterpreterState, Object, TypeObject};
use crate::python_process_info::{dispatch_by_version, PythonProcessInfo};
use crate::version::Version;

/// Addresses of static python type objects, used to check the types of objects we read
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Types {
    pub code: usize,
    pub string: usize,
    pub dict: usize,
}

impl Types {
    pub fn new<P: ProcessMemory>(
        python_info: &PythonProcessInfo,
        process: &P,
        version: &Version,
        config: &Config,
    ) -> Result<Option<Types>, Error> {
        if !config.type_checks {
            return Ok(None);
        }
        Types::from_symbols(python_info, process, version).map(Some)
    }

    pub fn is_code<C: CodeObject>(&self, code: &C) -> bool {
        code.ob_type() as usize == self.code
    }

    fn from_symbols<P: ProcessMemory>(
        python_info: &PythonProcessInfo,
        process: &P,
        version: &Version,
    ) -> Result<Types, Error> {
        let string_symbol = if version.major == 2 {
            "PyString_Type"
        } else {
            "PyUnicode_Type"
        };
        Ok(Types {
            code: Types::get_type_address(python_info, process, version, "PyCode_Type", c"code")?,
            string: Types::get_type_address(python_info, process, version, string_symbol, c"str")?,
            dict: Types::get_type_address(python_info, process, version, "PyDict_Type", c"dict")?,
        })
    }

    /// Resolves a static python type, so objects read as that type can be checked against it.
    /// Fails unless `tp_name` matches, so a wrongly resolved address is never used.
    fn get_type_address<P: ProcessMemory>(
        python_info: &PythonProcessInfo,
        process: &P,
        version: &Version,
        symbol: &str,
        tp_name: &CStr,
    ) -> Result<usize, Error> {
        fn read_tp_name<I: InterpreterState, P: ProcessMemory>(
            process: &P,
            addr: usize,
        ) -> Result<usize, Error> {
            let type_object: <I::Object as Object>::TypeObject = process.copy_struct(addr)?;
            Ok(type_object.name() as usize)
        }

        let addr = *python_info
            .get_symbol(symbol)
            .ok_or_else(|| format_err!("Failed to find {} symbol", symbol))?
            as usize;

        let tp_name_ptr = dispatch_by_version!(version, read_tp_name, process, addr)
            .with_context(|| format!("Failed to read {} type object", symbol))?;

        let tp_name = tp_name.to_bytes_with_nul();
        let name = process
            .copy(tp_name_ptr, tp_name.len())
            .with_context(|| format!("Failed to read {} tp_name", symbol))?;
        if name != tp_name {
            return Err(format_err!("Unexpected {} tp_name", symbol));
        }
        Ok(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binary_parser::BinaryInfo;
    use proc_maps::MapRange;
    use remoteprocess::LocalProcess;

    fn python_info_with_symbols(
        symbols: std::collections::HashMap<String, u64>,
    ) -> PythonProcessInfo {
        PythonProcessInfo {
            python_binary: Some(BinaryInfo {
                symbols,
                bss_addr: 0,
                bss_size: 0,
                pyruntime_addr: 0,
                pyruntime_size: 0,
                addr: 0,
                size: 0,
            }),
            libpython_binary: None,
            maps: Box::new(Vec::<MapRange>::new()),
            python_filename: std::path::PathBuf::new(),
            #[cfg(target_os = "linux")]
            dockerized: false,
        }
    }

    fn version_3_7() -> Version {
        version(3)
    }

    fn version(major: u64) -> Version {
        Version {
            major,
            minor: 7,
            patch: 0,
            release_flags: String::new(),
            build_metadata: None,
        }
    }

    fn get_code_type_address(info: &PythonProcessInfo) -> Result<usize, Error> {
        Types::get_type_address(info, &LocalProcess, &version_3_7(), "PyCode_Type", c"code")
    }

    fn code_type_resolves(tp_name: &[u8]) -> bool {
        let py_type = v3_7_0::PyTypeObject {
            tp_name: tp_name.as_ptr() as *const std::os::raw::c_char,
            ..Default::default()
        };
        let addr = &py_type as *const v3_7_0::PyTypeObject as usize;
        let info = python_info_with_symbols([("PyCode_Type".to_owned(), addr as u64)].into());
        get_code_type_address(&info).is_ok_and(|a| a == addr)
    }

    #[test]
    fn test_get_code_type_address() {
        assert!(code_type_resolves(b"code\0"));
        for tp_name in [
            b"dict\0".as_slice(),
            b"codesomethingelse\0",
            b"cod\0",
            b"\0",
        ] {
            assert!(!code_type_resolves(tp_name));
        }
    }

    #[test]
    fn test_get_code_type_address_without_symbol() {
        let info = python_info_with_symbols(Default::default());
        assert!(get_code_type_address(&info).is_err());
    }

    macro_rules! type_objects {
        ($py: ident) => {
            [c"code", c"str", c"dict"].map(|name| $py::PyTypeObject {
                tp_name: name.as_ptr(),
                ..Default::default()
            })
        };
    }

    fn type_symbols<T>(objects: &[T; 3], string_symbol: &str) -> (PythonProcessInfo, Types) {
        let [code, string, dict] = objects.each_ref().map(|t| t as *const T as usize);
        let info = python_info_with_symbols(
            [
                ("PyCode_Type".to_owned(), code as u64),
                (string_symbol.to_owned(), string as u64),
                ("PyDict_Type".to_owned(), dict as u64),
            ]
            .into(),
        );
        (info, Types { code, string, dict })
    }

    #[test]
    fn test_types_from_symbols_python3() {
        let objects = type_objects!(v3_7_0);
        let (info, expected) = type_symbols(&objects, "PyUnicode_Type");
        let types = Types::from_symbols(&info, &LocalProcess, &version(3)).unwrap();
        assert_eq!(types, expected);
        assert!(Types::from_symbols(&info, &LocalProcess, &version(2)).is_err());
    }

    #[test]
    fn test_types_from_symbols_python2() {
        let objects = type_objects!(v2_7_15);
        let (info, expected) = type_symbols(&objects, "PyString_Type");
        let types = Types::from_symbols(&info, &LocalProcess, &version(2)).unwrap();
        assert_eq!(types, expected);
        assert!(Types::from_symbols(&info, &LocalProcess, &version(3)).is_err());
    }
}
