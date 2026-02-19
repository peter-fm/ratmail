use std::io::{Cursor, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use image::DynamicImage;
use zip::{ZipWriter, write::FileOptions};

use super::PICKER_PREVIEW_MAX_LINES;

pub(crate) fn picker_meta_lines(path: &Path, mime: Option<&str>) -> Vec<String> {
    let mut lines = Vec::new();
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("(unknown)");
    lines.push(format!("Name: {}", name));

    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let ftype = meta.file_type();
        let kind = if ftype.is_dir() {
            "Directory"
        } else if ftype.is_symlink() {
            "Symlink"
        } else if ftype.is_file() {
            "File"
        } else {
            "Other"
        };
        lines.push(format!("Type: {}", kind));

        if ftype.is_dir() {
            if let Ok(read_dir) = std::fs::read_dir(path) {
                let count = read_dir.take(1000).count();
                lines.push(format!("Entries: {}", count));
            }
        }

        if ftype.is_file() {
            let size = (meta.len().min(usize::MAX as u64)) as usize;
            lines.push(format!("Size: {}", format_size(size)));
        }

        if let Ok(modified) = meta.modified() {
            lines.push(format!("Modified: {}", format_system_time(modified)));
        }
    }

    if let Some(mime) = mime {
        lines.push(format!("MIME: {}", mime));
    }

    lines
}

pub(crate) fn text_preview_from_bytes(bytes: &[u8], truncated_bytes: bool) -> Option<String> {
    if bytes.iter().any(|b| *b == 0) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if text.is_empty() {
        return Some("(empty file)".to_string());
    }
    let mut out = String::new();
    let mut lines = 0usize;
    for line in text.lines() {
        if lines >= PICKER_PREVIEW_MAX_LINES {
            break;
        }
        out.push_str(line);
        out.push('\n');
        lines += 1;
    }
    if truncated_bytes || lines >= PICKER_PREVIEW_MAX_LINES {
        out.push_str("...\n(truncated)\n");
    }
    Some(out)
}

pub(crate) fn render_pdf_first_page(path: &Path) -> Result<DynamicImage> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let prefix = format!("ratmail-pdf-preview-{}-{}", std::process::id(), stamp);
    let prefix_path = std::env::temp_dir().join(prefix);

    let status = Command::new("pdftoppm")
        .arg("-f")
        .arg("1")
        .arg("-l")
        .arg("1")
        .arg("-singlefile")
        .arg("-png")
        .arg(path)
        .arg(&prefix_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(anyhow::anyhow!("pdftoppm failed"));
    }

    let png_path = prefix_path.with_extension("png");
    let bytes = std::fs::read(&png_path)?;
    let _ = std::fs::remove_file(&png_path);
    let img = image::load_from_memory(&bytes)?;
    Ok(img)
}

pub(crate) fn format_size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MB", (bytes as f64 / (1024.0 * 1024.0)).round() as usize)
    } else if bytes >= 1024 {
        format!("{} KB", (bytes as f64 / 1024.0).round() as usize)
    } else {
        format!("{} B", bytes)
    }
}

pub(crate) fn safe_filename(input: &str) -> String {
    Path::new(input)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment")
        .to_string()
}

pub(crate) fn zip_directory(dir: &Path) -> Result<Vec<u8>> {
    let base_parent = dir.parent().unwrap_or(dir);
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let root = dir
            .strip_prefix(base_parent)
            .unwrap_or(dir)
            .to_string_lossy()
            .replace('\\', "/");
        if !root.is_empty() {
            let name = if root.ends_with('/') {
                root
            } else {
                format!("{}/", root)
            };
            zip.add_directory(name, options)?;
        }
        add_dir_to_zip(&mut zip, base_parent, dir, options)?;
        zip.finish()?;
    }
    Ok(cursor.into_inner())
}

fn format_system_time(time: SystemTime) -> String {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn add_dir_to_zip(
    zip: &mut ZipWriter<&mut Cursor<Vec<u8>>>,
    base_parent: &Path,
    dir: &Path,
    options: FileOptions,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(base_parent)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            let name = if rel.ends_with('/') {
                rel
            } else {
                format!("{}/", rel)
            };
            if !name.is_empty() {
                zip.add_directory(name, options)?;
            }
            add_dir_to_zip(zip, base_parent, &path, options)?;
        } else {
            zip.start_file(rel, options)?;
            let data = std::fs::read(&path)?;
            zip.write_all(&data)?;
        }
    }
    Ok(())
}
