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

/// Horizontal FOV in degrees from one eye's left/right half-angles (radians).
pub fn horizontal_fov_deg(left_rad: f32, right_rad: f32) -> f32 {
    (left_rad.abs() + right_rad.abs()).to_degrees()
}

/// Base name without extension: `{kind}_{YYYYMMDD}_{HHMMSS}_FOV_{deg}`.
pub fn capture_stem(kind: &str, now: chrono::DateTime<chrono::Local>, fov_deg: f32) -> String {
    format!("{}_{}_FOV_{:.6}", kind, now.format("%Y%m%d_%H%M%S"), fov_deg)
}

pub fn recording_stem(now: chrono::DateTime<chrono::Local>, fov_deg: f32) -> String {
    capture_stem("recording", now, fov_deg)
}

pub fn screenshot_stem(now: chrono::DateTime<chrono::Local>, fov_deg: f32) -> String {
    capture_stem("screenshot", now, fov_deg)
}

/// Append a media extension without using `Path::with_extension` (safe with dotted names).
pub fn with_media_ext(path_no_ext: &Path, ext: &str) -> PathBuf {
    let ext = ext.trim_start_matches('.');
    let mut os = path_no_ext.as_os_str().to_owned();
    os.push(".");
    os.push(ext);
    PathBuf::from(os)
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

    #[test]
    fn horizontal_fov_sums_left_and_right_radians() {
        let left = 52_f32.to_radians();
        let right = 51.976959_f32.to_radians();
        let deg = horizontal_fov_deg(left, right);
        assert!((deg - 103.976959).abs() < 1e-4);
    }

    #[test]
    fn recording_name_keeps_seconds_when_adding_ext() {
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 7, 13, 14, 30, 45)
            .single()
            .expect("valid local time");
        let stem_name = recording_stem(now, 103.976959);
        assert_eq!(stem_name, "recording_20260713_143045_FOV_103.976959");

        let video = with_media_ext(Path::new(&stem_name), "mkv");
        assert_eq!(
            video.to_string_lossy(),
            "recording_20260713_143045_FOV_103.976959.mkv"
        );
        let shot = with_media_ext(Path::new(&screenshot_stem(now, 103.976959)), "jpg");
        assert_eq!(
            shot.to_string_lossy(),
            "screenshot_20260713_143045_FOV_103.976959.jpg"
        );
    }

    #[test]
    fn with_extension_bug_demo() {
        // Documents why we avoid Path::with_extension for timestamped stems.
        let bad = PathBuf::from("recording.2026-07-13.14-30-45").with_extension("h264");
        assert_eq!(
            bad.file_name().unwrap().to_string_lossy(),
            "recording.2026-07-13.h264"
        );
    }
}
