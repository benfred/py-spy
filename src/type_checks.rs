use std::ffi::CStr;

use anyhow::{Context, Error};
use remoteprocess::ProcessMemory;

use crate::python_bindings::{
    v2_7_15, v3_10_0, v3_11_0, v3_12_0, v3_13_0, v3_14_0, v3_15_0, v3_3_7, v3_5_5, v3_6_6, v3_7_0,
    v3_8_0, v3_9_5,
};
use crate::python_interpreters::{InterpreterState, Object, TypeObject};
use crate::python_process_info::{dispatch_by_version, PythonProcessInfo};
use crate::version::Version;

/// Type checks guard against reading garbage from torn or reused memory.
/// By default they compare against the exact type, so subclasses of `str` and `dict` are rejected.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TypeChecksConfig {
    /// Skip frames whose code object is not a `PyCode_Type`
    pub code: bool,
    /// Skip frames whose filename or function name is not a `PyUnicode_Type`
    pub frame_strings: bool,
    /// Fail thread name lookup on strings that are not a `PyUnicode_Type`
    pub thread_names: bool,
    /// Fail on local variable names and string values that are not a `PyUnicode_Type`
    pub locals_strings: bool,
    /// Fail on dicts that are not a `PyDict_Type`, when looking up thread names or formatting locals
    pub dicts: bool,
    /// How many `tp_base` links to follow when an object's type is not an exact match
    pub max_subclass_depth: usize,
}

impl TypeChecksConfig {
    pub const fn disabled() -> TypeChecksConfig {
        TypeChecksConfig {
            code: false,
            frame_strings: false,
            thread_names: false,
            locals_strings: false,
            dicts: false,
            max_subclass_depth: 0,
        }
    }

    #[allow(dead_code)]
    pub const fn enabled() -> TypeChecksConfig {
        TypeChecksConfig {
            code: true,
            frame_strings: true,
            thread_names: true,
            locals_strings: true,
            dicts: true,
            max_subclass_depth: 0,
        }
    }
}

#[derive(Debug)]
pub struct TypeCheckError(&'static CStr);

impl std::fmt::Display for TypeCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Object is not a {}", self.0.to_string_lossy())
    }
}

impl std::error::Error for TypeCheckError {}

/// Checks objects against a resolved python type, unless disabled
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeCheck {
    Disabled,
    Enabled {
        address: usize,
        name: &'static CStr,
        max_subclass_depth: usize,
    },
}

impl TypeCheck {
    /// Accepts `ob_type` if it is the checked type, or derives from it within `max_subclass_depth` bases
    pub fn type_check<P: ProcessMemory>(
        &self,
        process: &P,
        version: &Version,
        ob_type: usize,
    ) -> Result<(), Error> {
        fn read_base<I: InterpreterState, P: ProcessMemory>(
            process: &P,
            addr: usize,
        ) -> Result<usize, Error> {
            let type_object: <I::Object as Object>::TypeObject = process.copy_struct(addr)?;
            Ok(type_object.base())
        }

        let TypeCheck::Enabled {
            address,
            name,
            max_subclass_depth,
        } = *self
        else {
            return Ok(());
        };
        let mut current = ob_type;
        for _ in 0..max_subclass_depth {
            if current == address || current == 0 {
                break;
            }
            current = dispatch_by_version!(version, read_base, process, current)
                .context("Failed to read tp_base")?;
        }
        if current != address {
            return Err(TypeCheckError(name).into());
        }
        Ok(())
    }
}

/// One `TypeCheck` for each field of `TypeChecksConfig`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeChecks {
    pub code: TypeCheck,
    pub frame_strings: TypeCheck,
    pub thread_names: TypeCheck,
    pub locals_strings: TypeCheck,
    pub dicts: TypeCheck,
}

impl TypeChecks {
    /// Resolves the python types that enabled checks compare against, failing if any is missing
    pub fn new<P: ProcessMemory>(
        python_info: &PythonProcessInfo,
        process: &P,
        version: &Version,
        config: &TypeChecksConfig,
    ) -> Result<TypeChecks, Error> {
        let check =
            |enabled: bool, symbol: &str, name: &'static CStr| -> Result<TypeCheck, Error> {
                if !enabled {
                    return Ok(TypeCheck::Disabled);
                }
                let address = Self::get_type_address(python_info, process, version, symbol, name)
                    .with_context(|| format!("Failed to resolve {}", symbol))?;
                Ok(TypeCheck::Enabled {
                    address,
                    name,
                    max_subclass_depth: config.max_subclass_depth,
                })
            };
        let string_symbol = if version.major == 2 {
            "PyString_Type"
        } else {
            "PyUnicode_Type"
        };
        Ok(TypeChecks {
            code: check(config.code, "PyCode_Type", c"code")?,
            frame_strings: check(config.frame_strings, string_symbol, c"str")?,
            thread_names: check(config.thread_names, string_symbol, c"str")?,
            locals_strings: check(config.locals_strings, string_symbol, c"str")?,
            dicts: check(config.dicts, "PyDict_Type", c"dict")?,
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
            .ok_or_else(|| format_err!("Failed to find type symbol"))? as usize;

        let tp_name_ptr = dispatch_by_version!(version, read_tp_name, process, addr)
            .context("Failed to read type object")?;

        let tp_name = tp_name.to_bytes_with_nul();
        let name = process
            .copy(tp_name_ptr, tp_name.len())
            .context("Failed to read tp_name")?;
        if name != tp_name {
            return Err(format_err!("Unexpected tp_name"));
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

    fn symbol_resolves(symbol: &str, tp_name: &[u8], expected: &CStr) -> bool {
        let py_type = v3_7_0::PyTypeObject {
            tp_name: tp_name.as_ptr() as *const std::os::raw::c_char,
            ..Default::default()
        };
        let addr = &py_type as *const v3_7_0::PyTypeObject as usize;
        let info = python_info_with_symbols([(symbol.to_owned(), addr as u64)].into());
        TypeChecks::get_type_address(&info, &LocalProcess, &version(3), symbol, expected)
            .is_ok_and(|a| a == addr)
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

    #[test]
    fn test_get_type_address() {
        assert!(symbol_resolves("PyCode_Type", b"code\0", c"code"));
        assert!(symbol_resolves("PyUnicode_Type", b"str\0", c"str"));
        assert!(symbol_resolves("PyString_Type", b"str\0", c"str"));
    }

    #[test]
    fn test_get_type_address_rejects_other_type() {
        for tp_name in [
            b"dict\0".as_slice(),
            b"codesomethingelse\0",
            b"cod\0",
            b"\0",
        ] {
            assert!(!symbol_resolves("PyCode_Type", tp_name, c"code"));
        }
        for tp_name in [b"unicode\0".as_slice(), b"strx\0", b"st\0", b"\0"] {
            assert!(!symbol_resolves("PyUnicode_Type", tp_name, c"str"));
        }
    }

    #[test]
    fn test_get_type_address_without_symbol() {
        let info = python_info_with_symbols(Default::default());
        assert!(TypeChecks::get_type_address(
            &info,
            &LocalProcess,
            &version(3),
            "PyCode_Type",
            c"code"
        )
        .is_err());
    }

    macro_rules! type_objects {
        ($py: ident) => {
            [c"code", c"str", c"dict"].map(|name| $py::PyTypeObject {
                tp_name: name.as_ptr(),
                ..Default::default()
            })
        };
    }

    fn type_addresses<T>(objects: &[T; 3]) -> [usize; 3] {
        objects.each_ref().map(|t| t as *const T as usize)
    }

    fn type_symbols([code, string, dict]: [usize; 3], string_symbol: &str) -> PythonProcessInfo {
        python_info_with_symbols(
            [
                ("PyCode_Type".to_owned(), code as u64),
                (string_symbol.to_owned(), string as u64),
                ("PyDict_Type".to_owned(), dict as u64),
            ]
            .into(),
        )
    }

    #[test]
    fn test_type_check_follows_bases_up_to_depth() {
        let base = v3_7_0::PyTypeObject::default();
        let child = v3_7_0::PyTypeObject {
            tp_base: &base as *const _ as *mut _,
            ..Default::default()
        };
        let grandchild = v3_7_0::PyTypeObject {
            tp_base: &child as *const _ as *mut _,
            ..Default::default()
        };
        let addr = |t: &v3_7_0::PyTypeObject| t as *const _ as usize;
        let check = |max_subclass_depth| TypeCheck::Enabled {
            address: addr(&base),
            name: c"str",
            max_subclass_depth,
        };

        for depth in 0..4 {
            let check = check(depth);
            assert!(check
                .type_check(&LocalProcess, &version(3), addr(&base))
                .is_ok());
            assert_eq!(
                check
                    .type_check(&LocalProcess, &version(3), addr(&child))
                    .is_ok(),
                depth >= 1
            );
            assert_eq!(
                check
                    .type_check(&LocalProcess, &version(3), addr(&grandchild))
                    .is_ok(),
                depth >= 2
            );
        }
        assert!(TypeCheck::Disabled
            .type_check(&LocalProcess, &version(3), 0x1234)
            .is_ok());
    }

    fn resolve_checks(
        info: &PythonProcessInfo,
        major: u64,
        config: &TypeChecksConfig,
    ) -> Result<TypeChecks, Error> {
        TypeChecks::new(info, &LocalProcess, &version(major), config)
    }

    fn enabled_check(address: usize, name: &'static CStr) -> TypeCheck {
        TypeCheck::Enabled {
            address,
            name,
            max_subclass_depth: 0,
        }
    }

    fn all_enabled([code, string, dict]: [usize; 3]) -> TypeChecks {
        TypeChecks {
            code: enabled_check(code, c"code"),
            frame_strings: enabled_check(string, c"str"),
            thread_names: enabled_check(string, c"str"),
            locals_strings: enabled_check(string, c"str"),
            dicts: enabled_check(dict, c"dict"),
        }
    }

    #[test]
    fn test_type_checks_resolve_python3() {
        let objects = type_objects!(v3_7_0);
        let addresses = type_addresses(&objects);
        let info = type_symbols(addresses, "PyUnicode_Type");
        let config = TypeChecksConfig::enabled();
        assert_eq!(
            resolve_checks(&info, 3, &config).unwrap(),
            all_enabled(addresses)
        );
        assert!(resolve_checks(&info, 2, &config).is_err());
    }

    #[test]
    fn test_type_checks_resolve_python2() {
        let objects = type_objects!(v2_7_15);
        let addresses = type_addresses(&objects);
        let info = type_symbols(addresses, "PyString_Type");
        let config = TypeChecksConfig::enabled();
        assert_eq!(
            resolve_checks(&info, 2, &config).unwrap(),
            all_enabled(addresses)
        );
        assert!(resolve_checks(&info, 3, &config).is_err());
    }

    #[test]
    fn test_type_checks_resolve_only_needed_types() {
        let objects = type_objects!(v3_7_0);
        let [code, _, _] = type_addresses(&objects);
        let code_only = python_info_with_symbols([("PyCode_Type".to_owned(), code as u64)].into());

        let disabled = resolve_checks(
            &python_info_with_symbols(Default::default()),
            3,
            &TypeChecksConfig::disabled(),
        )
        .unwrap();
        for check in [
            disabled.code,
            disabled.frame_strings,
            disabled.thread_names,
            disabled.locals_strings,
            disabled.dicts,
        ] {
            assert_eq!(check, TypeCheck::Disabled);
        }

        let config = TypeChecksConfig {
            code: true,
            ..TypeChecksConfig::disabled()
        };
        assert_eq!(
            resolve_checks(&code_only, 3, &config).unwrap(),
            TypeChecks {
                code: enabled_check(code, c"code"),
                frame_strings: TypeCheck::Disabled,
                thread_names: TypeCheck::Disabled,
                locals_strings: TypeCheck::Disabled,
                dicts: TypeCheck::Disabled,
            }
        );
        for config in [
            TypeChecksConfig {
                frame_strings: true,
                ..TypeChecksConfig::disabled()
            },
            TypeChecksConfig {
                thread_names: true,
                ..TypeChecksConfig::disabled()
            },
            TypeChecksConfig {
                locals_strings: true,
                ..TypeChecksConfig::disabled()
            },
            TypeChecksConfig {
                dicts: true,
                ..TypeChecksConfig::disabled()
            },
        ] {
            assert!(resolve_checks(&code_only, 3, &config).is_err());
        }
    }
}
