use std::{
    fs,
    path::{Path, PathBuf},
};

/// Resolve a configured capture path against the ALVR install root.
pub fn resolve_capture_path(program_root: &Path, configured: &str) -> PathBuf {
    let path = PathBuf::from(configured);
    if path.is_absolute() {
        path
    } else {
        program_root.join(path)
    }
}

pub fn ensure_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

pub fn program_root() -> PathBuf {
    crate::FILESYSTEM_LAYOUT
        .get()
        .map(|l| l.executables_dir.clone())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn recording_stem(now: chrono::DateTime<chrono::Local>) -> String {
    format!("recording.{}", now.format("%F.%H-%M-%S"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_path_unchanged() {
        let root = PathBuf::from("/opt/alvr");
        #[cfg(windows)]
        {
            let p = resolve_capture_path(&root, r"D:\out\recs");
            assert_eq!(p, PathBuf::from(r"D:\out\recs"));
        }
        #[cfg(not(windows))]
        {
            let p = resolve_capture_path(&root, "/tmp/recs");
            assert_eq!(p, PathBuf::from("/tmp/recs"));
        }
    }

    #[test]
    fn relative_joins_root() {
        let root = PathBuf::from("/opt/alvr");
        let p = resolve_capture_path(&root, "Captures/Records");
        assert_eq!(p, PathBuf::from("/opt/alvr/Captures/Records"));
    }
}
