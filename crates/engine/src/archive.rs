//! Workstream A2: deterministic archives (5.2): sorted entries, fixed mtime and permissions,
//! fixed deflate level; already-compressed formats (png, ogg, jpg, LevelDB `.ldb`) stored.

use crate::canonical;
use crate::model::Body;
use crate::path::{RelPath, Vfs};
use std::io::Write;
use std::path::Path;
use zip::{CompressionMethod, DateTime, System, ZipWriter, write::SimpleFileOptions};

const STORED: &[&str] = &["png", "ogg", "jpg", "jpeg", "wav", "ldb"];

/// Writes a zip at `out` with `entries` (zip path -> body). `.mcpack` = one pack at the zip
/// root; `.mcaddon` = each pack in its own top-level folder (caller builds the paths).
pub fn write_zip(out: &Path, entries: &[(RelPath, &Body)], vfs: &dyn Vfs) -> std::io::Result<()> {
    let io = |e: zip::result::ZipError| std::io::Error::other(e);
    let mut sorted: Vec<_> = entries.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut tmp = out.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let res = (|| {
        let mut zip = ZipWriter::new(std::io::BufWriter::new(crate::emit::create_fresh(&tmp)?));
        for (path, body) in sorted {
            let ext = path.as_str().rsplit('.').next().unwrap_or("");
            let stored = STORED.iter().any(|s| ext.eq_ignore_ascii_case(s));
            let opts = SimpleFileOptions::default()
                .compression_method(if stored {
                    CompressionMethod::Stored
                } else {
                    CompressionMethod::Deflated
                })
                .compression_level(if stored { None } else { Some(6) })
                .last_modified_time(DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).unwrap())
                .system(System::Unix) // not the host OS: identical bytes on every platform
                .unix_permissions(0o644)
                .large_file(false);
            zip.start_file(path.as_str(), opts).map_err(io)?;
            match body {
                Body::Passthrough(src) => {
                    vfs.stream(src, &mut zip)?;
                }
                Body::Json(v) => zip.write_all(&canonical::to_vec(v))?,
                Body::Text(t) => zip.write_all(t.as_bytes())?,
                Body::Bytes(b) => zip.write_all(b)?,
            }
        }
        zip.finish().map_err(io)?.flush()
    })();
    match res {
        Ok(()) => std::fs::rename(&tmp, out),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}
