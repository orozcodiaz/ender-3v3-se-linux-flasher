use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const DEST_STEM_LEN: usize = 6;
const DEST_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// Creality bootloaders are picky: short 8.3-ish names, and a *new* name each attempt.
/// Returns something like `hfie93.bin` (6 random alphanumeric chars + `.bin`).
pub fn suggest_dest_name() -> String {
    let mut stem = String::with_capacity(DEST_STEM_LEN);
    let mut bytes = [0u8; DEST_STEM_LEN];
    fill_random(&mut bytes);
    for b in bytes {
        stem.push(DEST_ALPHABET[(b as usize) % DEST_ALPHABET.len()] as char);
    }
    format!("{stem}.bin")
}

fn fill_random(buf: &mut [u8]) {
    // Prefer OS entropy; fall back so the CLI still works in odd environments.
    if let Ok(mut urandom) = File::open("/dev/urandom") {
        if urandom.read_exact(buf).is_ok() {
            return;
        }
    }
    let mut state = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xC0FFEE);
    for slot in buf.iter_mut() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        *slot = (state >> 33) as u8;
    }
}

pub fn validate_firmware_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolve current directory")?
            .join(path)
    };

    if !path.exists() {
        bail!("firmware file does not exist: {}", path.display());
    }
    if !path.is_file() {
        bail!("path is not a file: {}", path.display());
    }

    let meta = fs::metadata(&path).with_context(|| format!("stat {}", path.display()))?;
    if meta.len() == 0 {
        bail!("firmware file is empty: {}", path.display());
    }

    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("bin") => Ok(path),
        other => bail!(
            "expected a .bin firmware file, got extension: {}",
            other.unwrap_or("(none)")
        ),
    }
}

pub fn validate_dest_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("destination filename cannot be empty");
    }
    if name.contains('/') || name.contains('\\') {
        bail!("destination must be a filename, not a path");
    }
    if !name.to_ascii_lowercase().ends_with(".bin") {
        bail!("destination filename must end with .bin");
    }
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if stem.is_empty() {
        bail!("invalid destination filename");
    }
    if stem.len() > 8 {
        bail!("destination stem '{stem}' is longer than 8 characters (printer bootloaders prefer 8.3 names)");
    }
    if !stem
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("destination name should use only ASCII letters, digits, '_' or '-'");
    }
    Ok(name.to_string())
}

pub fn copy_firmware(src: &Path, dest_dir: &Path, dest_name: &str) -> Result<PathBuf> {
    let dest = dest_dir.join(dest_name);
    if dest.exists() {
        bail!(
            "refusing to overwrite existing file: {}\nPick a new name (printers ignore previously seen .bin names).",
            dest.display()
        );
    }

    let meta = fs::metadata(src)?;
    let total = meta.len();
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .unwrap()
        .progress_chars("=>-"),
    );

    let mut input = File::open(src).with_context(|| format!("open {}", src.display()))?;
    let mut output = File::create(&dest).with_context(|| format!("create {}", dest.display()))?;

    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n])?;
        pb.inc(n as u64);
    }
    output.flush()?;
    output.sync_all().context("fsync destination file")?;
    // Best-effort: sync the directory entry too.
    if let Ok(dir) = File::open(dest_dir) {
        let _ = dir.sync_all();
    }
    pb.finish_and_clear();

    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dest_name_rules() {
        assert!(validate_dest_name("123.bin").is_ok());
        assert!(validate_dest_name("firmw.bin").is_ok());
        assert!(validate_dest_name("hfie93.bin").is_ok());
        assert!(validate_dest_name("toolongname.bin").is_err());
        assert!(validate_dest_name("nope.txt").is_err());
        assert!(validate_dest_name("../x.bin").is_err());
    }

    #[test]
    fn suggest_dest_name_shape() {
        let name = suggest_dest_name();
        assert!(name.ends_with(".bin"), "{name}");
        let stem = name.trim_end_matches(".bin");
        assert_eq!(stem.len(), DEST_STEM_LEN, "{name}");
        assert!(
            stem.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "{name}"
        );
        // Two draws should almost always differ; allow one retry on collision.
        let other = suggest_dest_name();
        if name == other {
            let third = suggest_dest_name();
            assert_ne!(name, third, "expected distinct random names");
        }
    }
}
