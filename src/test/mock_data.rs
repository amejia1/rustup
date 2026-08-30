//! Data files for the mock distribution server and forward proxy.
//!
//! Both programs write a small `key=value` data file describing how to reach
//! them, so tests can discover the (possibly OS-assigned) port and any other
//! runtime configuration without parsing program output.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A data file describing a running mock distribution server or forward
/// proxy.
///
/// The file contains one `key=value` pair per line:
///
/// | Key          | Description                                             |
/// | ------------ | ------------------------------------------------------- |
/// | `addr`       | The address the process is listening on                 |
/// | `port`       | The port the process is listening on                    |
/// | `pid`        | The process id of the process                           |
/// | `credential` | The basic test credential in use (only if configured)   |
/// | `directory`  | The directory being served (mock server only)           |
///
/// The file is written after the process has bound its listener and removed
/// when the process exits.
pub struct MockDataFile {
    path: PathBuf,
}

impl MockDataFile {
    /// The default data file location for a mock program.
    ///
    /// On Unix this is `${HOME}/.local/share/<name>.data`, falling back to
    /// the system temporary directory when `HOME` is not set. On Windows it
    /// is `%LOCALAPPDATA%\<name>.data`, falling back to
    /// `C:\Users\%USER%\AppData\Local` and then to the system temporary
    /// directory.
    pub fn default_path(name: &str) -> PathBuf {
        Self::default_dir().join(format!("{name}.data"))
    }

    /// The directory data files are created in by default.
    fn default_dir() -> PathBuf {
        #[cfg(not(windows))]
        {
            Self::unix_default_dir(env::var_os("HOME").as_deref())
        }

        #[cfg(windows)]
        {
            Self::windows_default_dir(
                env::var_os("LOCALAPPDATA").as_deref(),
                env::var_os("USER").as_deref(),
            )
        }
    }

    /// The Unix default data directory: `${HOME}/.local/share`, or the
    /// system temporary directory when `HOME` is not set.
    #[cfg_attr(windows, allow(dead_code))]
    fn unix_default_dir(home: Option<&OsStr>) -> PathBuf {
        match home {
            Some(home) => Path::new(home).join(".local").join("share"),
            None => env::temp_dir(),
        }
    }

    /// The Windows default data directory: `LOCALAPPDATA`, falling back to
    /// `C:\Users\${USER}\AppData\Local` and then to the system temporary
    /// directory.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn windows_default_dir(localappdata: Option<&OsStr>, user: Option<&OsStr>) -> PathBuf {
        if let Some(localappdata) = localappdata {
            return PathBuf::from(localappdata);
        }
        if let Some(user) = user {
            return Path::new(r"C:\Users")
                .join(user)
                .join("AppData")
                .join("Local");
        }
        env::temp_dir()
    }

    /// Atomically writes the data file for a listening process, replacing any
    /// existing file at `path`.
    ///
    /// `credential` is recorded only when the process uses basic test
    /// credentials, and `directory` (mock server only) records the served
    /// directory.
    pub fn write(
        path: &Path,
        addr: &str,
        port: u16,
        pid: u32,
        credential: Option<&str>,
        directory: Option<&Path>,
    ) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut content = String::new();
        content.push_str(&format!("addr={addr}\nport={port}\npid={pid}\n"));
        if let Some(credential) = credential {
            content.push_str(&format!("credential={credential}\n"));
        }
        if let Some(directory) = directory {
            content.push_str(&format!("directory={}\n", directory.display()));
        }

        // Write to a sibling temporary file and rename, so readers never
        // observe a partially written file.
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("data");
        let tmp = path.with_file_name(format!(".{file_name}.tmp"));
        fs::write(&tmp, content)?;
        fs::rename(&tmp, path)
    }

    /// Wraps a data file at `path`; the file is removed when the
    /// `MockDataFile` is dropped.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The location of the data file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the value stored under `key`, if the file can be read and the
    /// key is present.
    pub fn get(&self, key: &str) -> Option<String> {
        Self::parse(&fs::read_to_string(&self.path).ok()?)
            .into_iter()
            .find_map(|(name, value)| (name == key).then_some(value))
    }

    /// Parses data file content into key/value pairs.
    pub fn parse(content: &str) -> BTreeMap<String, String> {
        content
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    /// Removes the data file, ignoring errors (e.g. it was already removed).
    pub fn remove(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for MockDataFile {
    fn drop(&mut self) {
        self.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_file_round_trip() {
        let tmp = tempfile::Builder::new()
            .prefix("mock-data-")
            .tempdir()
            .unwrap();
        let path = tmp.path().join("mock-server.data");
        let data = MockDataFile::new(path.clone());

        MockDataFile::write(
            &path,
            "127.0.0.1",
            43211,
            4242,
            Some("testuser:testpass"),
            Some(tmp.path()),
        )
        .unwrap();

        assert_eq!(data.get("addr").as_deref(), Some("127.0.0.1"));
        assert_eq!(data.get("port").as_deref(), Some("43211"));
        assert_eq!(data.get("pid").as_deref(), Some("4242"));
        assert_eq!(data.get("credential").as_deref(), Some("testuser:testpass"));
        assert_eq!(
            data.get("directory").as_deref(),
            Some(tmp.path().display().to_string().as_str())
        );
        assert_eq!(data.get("missing"), None);

        data.remove();
        assert!(!path.exists());
    }

    #[test]
    fn data_file_omits_unset_optionals() {
        let tmp = tempfile::Builder::new()
            .prefix("mock-data-")
            .tempdir()
            .unwrap();
        let path = tmp.path().join("mock-proxy.data");
        let data = MockDataFile::new(path.clone());

        MockDataFile::write(&path, "127.0.0.1", 43211, 4242, None, None).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("credential="));
        assert!(!content.contains("directory="));
        assert_eq!(data.get("port").as_deref(), Some("43211"));

        data.remove();
        assert!(!path.exists());
    }

    #[test]
    fn default_path_uses_platform_default_dir() {
        let dir = MockDataFile::default_dir();
        let path = MockDataFile::default_path("rustup-mock-server");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("rustup-mock-server.data")
        );
        assert_eq!(path.parent(), Some(dir.as_path()));
    }

    #[test]
    fn unix_default_dir_lives_under_local_share() {
        let dir = MockDataFile::unix_default_dir(Some(OsStr::new("/home/tester")));

        // Compare the final components rather than a rendered string, so the
        // assertion holds on every platform (path separators differ).
        let mut components = dir.components().rev().take(2).collect::<Vec<_>>();
        components.reverse();
        let names = components
            .iter()
            .map(|component| component.as_os_str().to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, [".local", "share"]);

        // Without `HOME` the directory is the system temporary directory.
        assert_eq!(MockDataFile::unix_default_dir(None), env::temp_dir());
    }

    #[test]
    fn windows_default_dir_uses_localappdata() {
        // `LOCALAPPDATA` is used as-is when set.
        assert_eq!(
            MockDataFile::windows_default_dir(
                Some(OsStr::new(r"C:\Users\tester\AppData\Local")),
                None
            ),
            PathBuf::from(r"C:\Users\tester\AppData\Local")
        );

        // Without `LOCALAPPDATA` the directory is built from `USER`.
        let dir = MockDataFile::windows_default_dir(None, Some(OsStr::new("tester")));
        assert!(dir.to_string_lossy().contains(r"C:\Users"));
        let mut components = dir.components().rev().take(3).collect::<Vec<_>>();
        components.reverse();
        let names = components
            .iter()
            .map(|component| component.as_os_str().to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, ["tester", "AppData", "Local"]);

        // Without either variable the system temporary directory is used.
        assert_eq!(
            MockDataFile::windows_default_dir(None, None),
            env::temp_dir()
        );
    }
}
