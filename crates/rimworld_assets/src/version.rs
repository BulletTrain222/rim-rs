use std::fmt;

/// Versions (`major.minor.build`) that this project has been tested against.
/// See `docs/compatibility.md`.
pub const SUPPORTED_VERSIONS: &[&str] = &["1.6.4871"];

/// A RimWorld version as written in `Version.txt`, e.g. `1.6.4871 rev590`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameVersion {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    pub revision: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
#[error("unrecognised RimWorld version string {0:?}")]
pub struct VersionParseError(pub String);

impl GameVersion {
    pub fn parse(text: &str) -> Result<Self, VersionParseError> {
        let err = || VersionParseError(text.to_owned());
        let text = text.trim();
        let mut parts = text.split_whitespace();
        let numbers = parts.next().ok_or_else(err)?;
        let mut nums = numbers.split('.').map(|n| n.parse::<u32>());
        let major = nums.next().ok_or_else(err)?.map_err(|_| err())?;
        let minor = nums.next().ok_or_else(err)?.map_err(|_| err())?;
        let build = nums.next().ok_or_else(err)?.map_err(|_| err())?;
        if nums.next().is_some() {
            return Err(err());
        }
        let revision = match parts.next() {
            Some(rev) => Some(
                rev.strip_prefix("rev")
                    .ok_or_else(err)?
                    .parse()
                    .map_err(|_| err())?,
            ),
            None => None,
        };
        Ok(Self {
            major,
            minor,
            build,
            revision,
        })
    }

    /// `major.minor.build`, the part compared against [`SUPPORTED_VERSIONS`].
    pub fn short(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.build)
    }

    pub fn is_supported(&self) -> bool {
        SUPPORTED_VERSIONS.contains(&self.short().as_str())
    }
}

impl fmt::Display for GameVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.short())?;
        if let Some(rev) = self.revision {
            write!(f, " rev{rev}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_version() {
        let v = GameVersion::parse("1.6.4871 rev590\r\n").unwrap();
        assert_eq!(
            (v.major, v.minor, v.build, v.revision),
            (1, 6, 4871, Some(590))
        );
        assert_eq!(v.to_string(), "1.6.4871 rev590");
        assert!(v.is_supported());
    }

    #[test]
    fn parses_without_revision() {
        let v = GameVersion::parse("1.5.4409").unwrap();
        assert_eq!(v.revision, None);
        assert!(!v.is_supported());
    }

    #[test]
    fn rejects_garbage() {
        for s in ["", "abc", "1.6", "1.6.x", "1.6.1 590", "1.6.1.2"] {
            assert!(GameVersion::parse(s).is_err(), "{s:?} should fail");
        }
    }
}
