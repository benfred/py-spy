pub mod v2_7_15;
pub mod v3_10_0;
pub mod v3_11_0;
pub mod v3_3_7;
pub mod v3_5_5;
pub mod v3_6_6;
pub mod v3_7_0;
pub mod v3_8_0;
pub mod v3_9_5;

// currently the PyRuntime struct used from Python 3.7 on really can't be
// exposed in a cross platform way using bindgen. PyRuntime has several mutex's
// as member variables, and these have different sizes depending on the operating
// system and system architecture.
// Instead we will define some constants here that define valid offsets for the
// member variables we care about here
// (note 'generate_bindings.py' has code to figure out these offsets)
pub mod pyruntime {
    use crate::version::Version;

    // There aren't any OS specific members of PyRuntime before pyinterpreters.head,
    // so these offsets should be valid for all OS'es
    #[cfg(target_arch = "x86")]
    pub fn get_interp_head_offset(version: &Version) -> usize {
        match version {
            Version {
                major: 3,
                minor: 8,
                patch: 0,
                ..
            } => match version.release_flags.as_ref() {
                "a1" | "a2" => 16,
                "a3" | "a4" => 20,
                _ => 24,
            },
            Version {
                major: 3,
                minor: 8..=10,
                ..
            } => 24,
            _ => 16,
        }
    }

    #[cfg(target_arch = "arm")]
    pub fn get_interp_head_offset(version: &Version) -> usize {
        match version {
            Version {
                major: 3, minor: 7, ..
            } => 20,
            _ => 28,
        }
    }

    #[cfg(target_pointer_width = "64")]
    pub fn get_interp_head_offset(version: &Version) -> usize {
        match version {
            Version {
                major: 3,
                minor: 8,
                patch: 0,
                ..
            } => match version.release_flags.as_ref() {
                "a1" | "a2" => 24,
                _ => 32,
            },
            Version {
                major: 3,
                minor: 8..=10,
                ..
            } => 32,
            Version {
                major: 3,
                minor: 11,
                ..
            } => 40,
            _ => 24,
        }
    }

    // getting gilstate.tstate_current is different for all OS
    // and is also different for each python version, and even
    // between v3.8.0a1 and v3.8.0a2 =(
    #[cfg(target_os = "macos")]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        match version {
            Version {
                major: 3,
                minor: 7,
                patch: 0..=3,
                ..
            } => Some(1440),
            Version {
                major: 3, minor: 7, ..
            } => Some(1528),
            Version {
                major: 3,
                minor: 8,
                patch: 0,
                ..
            } => match version.release_flags.as_ref() {
                "a1" => Some(1432),
                "a2" => Some(888),
                "a3" | "a4" => Some(1448),
                _ => Some(1416),
            },
            Version {
                major: 3, minor: 8, ..
            } => Some(1416),
            Version {
                major: 3,
                minor: 9..=10,
                ..
            } => Some(616),
            Version {
                major: 3,
                minor: 11,
                ..
            } => Some(624),
            _ => None,
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86"))]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        match version {
            Version {
                major: 3, minor: 7, ..
            } => Some(796),
            Version {
                major: 3,
                minor: 8,
                patch: 0,
                ..
            } => match version.release_flags.as_ref() {
                "a1" => Some(792),
                "a2" => Some(512),
                "a3" | "a4" => Some(800),
                _ => Some(788),
            },
            Version {
                major: 3, minor: 8, ..
            } => Some(788),
            Version {
                major: 3,
                minor: 9..=10,
                ..
            } => Some(352),
            _ => None,
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "arm"))]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        match version {
            Version {
                major: 3, minor: 7, ..
            } => Some(828),
            Version {
                major: 3, minor: 8, ..
            } => Some(804),
            Version {
                major: 3,
                minor: 9..=11,
                ..
            } => Some(364),
            _ => None,
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        match version {
            Version {
                major: 3,
                minor: 7,
                patch: 0..=3,
                ..
            } => Some(1408),
            Version {
                major: 3, minor: 7, ..
            } => Some(1496),
            Version {
                major: 3, minor: 8, ..
            } => Some(1384),
            Version {
                major: 3,
                minor: 9..=10,
                ..
            } => Some(584),
            Version {
                major: 3,
                minor: 11,
                ..
            } => Some(592),
            _ => None,
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        match version {
            Version {
                major: 3,
                minor: 7,
                patch: 0..=3,
                ..
            } => Some(1392),
            Version {
                major: 3, minor: 7, ..
            } => Some(1480),
            Version {
                major: 3,
                minor: 8,
                patch: 0,
                ..
            } => match version.release_flags.as_ref() {
                "a1" => Some(1384),
                "a2" => Some(840),
                "a3" | "a4" => Some(1400),
                _ => Some(1368),
            },
            Version {
                major: 3, minor: 8, ..
            } => match version.build_metadata.as_deref() {
                Some("cinder") => Some(1384),
                _ => Some(1368),
            },
            Version {
                major: 3,
                minor: 9..=10,
                ..
            } => Some(568),
            Version {
                major: 3,
                minor: 11,
                ..
            } => Some(576),
            _ => None,
        }
    }

    #[cfg(all(
        target_os = "linux",
        any(
            target_arch = "powerpc64",
            target_arch = "powerpc",
            target_arch = "mips"
        )
    ))]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        None
    }

    #[cfg(windows)]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        match version {
            Version {
                major: 3,
                minor: 7,
                patch: 0..=3,
                ..
            } => Some(1320),
            Version {
                major: 3,
                minor: 8,
                patch: 0,
                ..
            } => match version.release_flags.as_ref() {
                "a1" => Some(1312),
                "a2" => Some(768),
                "a3" | "a4" => Some(1328),
                _ => Some(1296),
            },
            Version {
                major: 3, minor: 8, ..
            } => Some(1296),
            Version {
                major: 3,
                minor: 9..=10,
                ..
            } => Some(496),
            Version {
                major: 3,
                minor: 11,
                ..
            } => Some(504),
            _ => None,
        }
    }

    #[cfg(target_os = "freebsd")]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        match version {
            Version {
                major: 3,
                minor: 7,
                patch: 0..=3,
                ..
            } => Some(1248),
            Version {
                major: 3,
                minor: 7,
                patch: 4..=7,
                ..
            } => Some(1336),
            Version {
                major: 3,
                minor: 8,
                patch: 0,
                ..
            } => match version.release_flags.as_ref() {
                "a1" => Some(1240),
                "a2" => Some(696),
                "a3" | "a4" => Some(1256),
                _ => Some(1224),
            },
            Version {
                major: 3, minor: 8, ..
            } => Some(1224),
            Version {
                major: 3,
                minor: 9..=10,
                ..
            } => Some(424),
            Version {
                major: 3,
                minor: 11,
                ..
            } => Some(432),
            _ => None,
        }
    }
}
