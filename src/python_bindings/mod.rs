pub mod v2_7_15;
pub mod v3_3_7;
pub mod v3_5_5;
pub mod v3_6_6;
pub mod v3_7_0;

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
    #[cfg(target_pointer_width = "32")]
    pub fn get_interp_head_offset(version: &Version) -> usize {
        match version {
             Version{major: 3, minor: 8, patch: 0, ..} => {
                 match version.release_flags.as_ref() {
                    "a3" | "a4" | "b1" => 20,
                    _ => 16
                }
             },
             _ => 16
        }
    }

    #[cfg(target_pointer_width = "64")]
    pub fn get_interp_head_offset(version: &Version) -> usize {
        match version {
             Version{major: 3, minor: 8, patch: 0, ..} => {
                 match version.release_flags.as_ref() {
                    "a3" | "a4" | "b1" => 32,
                    _ => 24
                }
             },
             _ => 24
        }
    }

    // getting gilstate.tstate_current is different for all OS
    // and is also different for each python version, and even
    // between v3.8.0a1 and v3.8.0a2 =(
    #[cfg(target_os="macos")]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        match version {
             Version{major: 3, minor: 7, ..} => Some(1440),
             Version{major: 3, minor: 8, patch: 0, ..} => {
                 match version.release_flags.as_ref() {
                    "a1" => Some(1432),
                    "a2" => Some(888),
                    "a3" | "a4" => Some(1448),
                    "b1" => Some(1416),
                    _ => None
                }
             },
             _ => None
        }
    }

    #[cfg(all(target_os="linux", target_pointer_width = "32"))]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        match version {
             Version{major: 3, minor: 7, ..} => Some(796),
             Version{major: 3, minor: 8, patch: 0, ..} => {
                 match version.release_flags.as_ref() {
                    "a1" => Some(792),
                    "a2" => Some(512),
                    "a3" | "a4" => Some(800),
                    "b1" => Some(784),
                    _ => None
                }
             },
             _ => None
        }
    }

    #[cfg(all(target_os="linux", target_pointer_width = "64"))]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        match version {
             Version{major: 3, minor: 7, ..} => Some(1392),
             Version{major: 3, minor: 8, patch: 0, ..} => {
                 match version.release_flags.as_ref() {
                    "a1" => Some(1384),
                    "a2" => Some(840),
                    "a3" | "a4" => Some(1400),
                    "b1" => Some(1368),
                    _ => None
                }
             },
             _ => None
        }
    }

    #[cfg(windows)]
    pub fn get_tstate_current_offset(version: &Version) -> Option<usize> {
        match version {
             Version{major: 3, minor: 7, ..} => Some(1320),
             Version{major: 3, minor: 8, patch: 0, ..} => {
                 match version.release_flags.as_ref() {
                    "a1" => Some(1312),
                    "a2" => Some(768),
                    "a3" | "a4" => Some(1328),
                    "b1" => Some(1296),
                    _ => None
                }
             },
             _ => None
        }
    }
}
