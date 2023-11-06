use lazy_static::lazy_static;
use regex::bytes::Regex;

use anyhow::Error;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub release_flags: String,
    pub build_metadata: Option<String>,
}

impl Version {
    pub fn scan_bytes(data: &[u8]) -> Result<Version, Error> {
        lazy_static! {
            static ref RE: Regex = Regex::new(
                r"((2|3)\.(3|4|5|6|7|8|9|10|11)\.(\d{1,2}))((a|b|c|rc)\d{1,2})?(\+(?:[0-9a-z-]+(?:[.][0-9a-z-]+)*)?)? (.{1,64})"
            )
            .unwrap();
        }

        if let Some(cap) = RE.captures_iter(data).next() {
            let release = match cap.get(5) {
                Some(x) => std::str::from_utf8(x.as_bytes())?,
                None => "",
            };
            let major = std::str::from_utf8(&cap[2])?.parse::<u64>()?;
            let minor = std::str::from_utf8(&cap[3])?.parse::<u64>()?;
            let patch = std::str::from_utf8(&cap[4])?.parse::<u64>()?;
            let build_metadata = if let Some(s) = cap.get(7) {
                Some(std::str::from_utf8(&s.as_bytes()[1..])?.to_owned())
            } else {
                None
            };

            let version = std::str::from_utf8(&cap[0])?;
            info!("Found matching version string '{}'", version);
            #[cfg(windows)]
            {
                if version.contains("32 bit") {
                    error!("32-bit python is not yet supported on windows! See https://github.com/benfred/py-spy/issues/31 for updates");
                    // we're panic'ing rather than returning an error, since we can't recover from this
                    // and returning an error would just get the calling code to fall back to other
                    // methods of trying to find the version
                    panic!("32-bit python is unsupported on windows");
                }
            }

            return Ok(Version {
                major,
                minor,
                patch,
                release_flags: release.to_owned(),
                build_metadata,
            });
        }
        Err(format_err!("failed to find version string"))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{}.{}.{}{}",
            self.major, self.minor, self.patch, self.release_flags
        )?;
        if let Some(build_metadata) = &self.build_metadata {
            write!(f, "+{}", build_metadata,)?
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_find_version() {
        let version = Version::scan_bytes(b"2.7.10 (default, Oct  6 2017, 22:29:07)").unwrap();
        assert_eq!(
            version,
            Version {
                major: 2,
                minor: 7,
                patch: 10,
                release_flags: "".to_owned(),
                build_metadata: None,
            }
        );

        let version = Version::scan_bytes(
            b"3.6.3 |Anaconda custom (64-bit)| (default, Oct  6 2017, 12:04:38)",
        )
        .unwrap();
        assert_eq!(
            version,
            Version {
                major: 3,
                minor: 6,
                patch: 3,
                release_flags: "".to_owned(),
                build_metadata: None,
            }
        );

        let version =
            Version::scan_bytes(b"Python 3.7.0rc1 (v3.7.0rc1:dfad352267, Jul 20 2018, 13:27:54)")
                .unwrap();
        assert_eq!(
            version,
            Version {
                major: 3,
                minor: 7,
                patch: 0,
                release_flags: "rc1".to_owned(),
                build_metadata: None,
            }
        );

        let version =
            Version::scan_bytes(b"Python 3.10.0rc1 (tags/v3.10.0rc1, Aug 28 2021, 18:25:40)")
                .unwrap();
        assert_eq!(
            version,
            Version {
                major: 3,
                minor: 10,
                patch: 0,
                release_flags: "rc1".to_owned(),
                build_metadata: None,
            }
        );

        let version =
            Version::scan_bytes(b"1.7.0rc1 (v1.7.0rc1:dfad352267, Jul 20 2018, 13:27:54)");
        assert!(version.is_err(), "don't match unsupported ");

        let version = Version::scan_bytes(b"3.7 10 ");
        assert!(version.is_err(), "needs dotted version");

        let version = Version::scan_bytes(b"3.7.10fooboo ");
        assert!(version.is_err(), "limit suffixes");

        // v2.7.15+ is a valid version string apparently: https://github.com/benfred/py-spy/issues/81
        let version = Version::scan_bytes(b"2.7.15+ (default, Oct  2 2018, 22:12:08)").unwrap();
        assert_eq!(
            version,
            Version {
                major: 2,
                minor: 7,
                patch: 15,
                release_flags: "".to_owned(),
                build_metadata: Some("".to_owned()),
            }
        );

        let version = Version::scan_bytes(b"2.7.10+dcba (default)").unwrap();
        assert_eq!(
            version,
            Version {
                major: 2,
                minor: 7,
                patch: 10,
                release_flags: "".to_owned(),
                build_metadata: Some("dcba".to_owned()),
            }
        );

        let version = Version::scan_bytes(b"2.7.10+5-4.abcd (default)").unwrap();
        assert_eq!(
            version,
            Version {
                major: 2,
                minor: 7,
                patch: 10,
                release_flags: "".to_owned(),
                build_metadata: Some("5-4.abcd".to_owned()),
            }
        );

        let version = Version::scan_bytes(b"2.8.5+cinder (default)").unwrap();
        assert_eq!(
            version,
            Version {
                major: 2,
                minor: 8,
                patch: 5,
                release_flags: "".to_owned(),
                build_metadata: Some("cinder".to_owned()),
            }
        );
    }
}
