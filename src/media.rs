use std::path::Path;
use walkdir::WalkDir;
use crate::domain::{MediaItem, ProbeStatus};
const EXTENSIONS: &[&str] = &["mov", "mp4", "mkv", "ts", "m2ts", "mpeg", "mpg", "webm", "avi", "mxf"];
pub fn scan(root: &Path) -> Vec<MediaItem> {
    WalkDir::new(root).follow_links(false).into_iter().filter_map(Result::ok).filter(|e| e.file_type().is_file()).filter_map(|entry| {
        let path = entry.path(); let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        if !EXTENSIONS.contains(&extension.as_str()) { return None; }
        let metadata = entry.metadata().ok()?;
        Some(MediaItem { path: path.display().to_string(), filename: entry.file_name().to_string_lossy().into_owned(), size_bytes: metadata.len(), extension: Some(extension), probe_status: ProbeStatus::Pending })
    }).collect()
}
