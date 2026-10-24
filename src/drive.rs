use anyhow::{bail, Context, Result};
use fs2::{available_space, total_space};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct RemovableDrive {
    pub name: String,
    pub device: PathBuf,
    pub mountpoint: PathBuf,
    pub size_bytes: u64,
    pub fstype: String,
    pub label: Option<String>,
    pub transport: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DriveStats {
    pub file_count: u64,
    pub dir_count: u64,
    pub occupied_bytes: u64,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub fstype: String,
}

#[derive(Debug, Clone)]
pub struct DirEntryView {
    pub name: String,
    pub is_dir: bool,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct LsblkOutput {
    blockdevices: Vec<LsblkNode>,
}

#[derive(Debug, Deserialize)]
struct LsblkNode {
    name: String,
    path: Option<String>,
    size: Option<serde_json::Value>,
    #[serde(rename = "type")]
    kind: Option<String>,
    mountpoint: Option<String>,
    fstype: Option<String>,
    rm: Option<bool>,
    hotplug: Option<bool>,
    label: Option<String>,
    tran: Option<String>,
    children: Option<Vec<LsblkNode>>,
}

impl RemovableDrive {
    pub fn display_label(&self) -> String {
        let label = self
            .label
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("(no label)");
        let tran = self
            .transport
            .as_deref()
            .unwrap_or("?")
            .to_uppercase();
        format!(
            "{:<14}  {}  {}  {}  {}",
            label,
            human_bytes(self.size_bytes),
            if self.fstype.is_empty() {
                "?"
            } else {
                &self.fstype
            },
            tran,
            self.mountpoint.display()
        )
    }
}

pub fn list_removable_drives() -> Result<Vec<RemovableDrive>> {
    let output = Command::new("lsblk")
        .args([
            "-J",
            "-b",
            "-o",
            "NAME,PATH,SIZE,TYPE,MOUNTPOINT,FSTYPE,RM,HOTPLUG,LABEL,TRAN",
        ])
        .output()
        .context("failed to run lsblk (is util-linux installed?)")?;

    if !output.status.success() {
        bail!(
            "lsblk failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let parsed: LsblkOutput =
        serde_json::from_slice(&output.stdout).context("failed to parse lsblk JSON")?;

    let mut drives = Vec::new();
    collect_drives(&parsed.blockdevices, false, None, &mut drives);

    // Prefer unique mountpoints; keep partitions over raw disks when both appear.
    drives.sort_by(|a, b| a.mountpoint.cmp(&b.mountpoint));
    drives.dedup_by(|a, b| a.mountpoint == b.mountpoint);
    Ok(drives)
}

fn collect_drives(
    nodes: &[LsblkNode],
    parent_removable: bool,
    parent_tran: Option<&str>,
    out: &mut Vec<RemovableDrive>,
) {
    for node in nodes {
        let removable = node.rm.unwrap_or(false)
            || node.hotplug.unwrap_or(false)
            || parent_removable;
        let tran = node.tran.as_deref().or(parent_tran);
        let kind = node.kind.as_deref().unwrap_or("");

        if removable && kind == "part" {
            if let Some(mount) = node.mountpoint.as_deref().filter(|m| !m.is_empty()) {
                let device = node
                    .path
                    .clone()
                    .unwrap_or_else(|| format!("/dev/{}", node.name));
                out.push(RemovableDrive {
                    name: node.name.clone(),
                    device: PathBuf::from(device),
                    mountpoint: PathBuf::from(mount),
                    size_bytes: parse_size(&node.size),
                    fstype: node.fstype.clone().unwrap_or_default(),
                    label: node.label.clone().filter(|s| !s.is_empty()),
                    transport: tran.map(str::to_string),
                });
            }
        }

        if let Some(children) = &node.children {
            collect_drives(children, removable, tran, out);
        }
    }
}

fn parse_size(value: &Option<serde_json::Value>) -> u64 {
    match value {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn resolve_drive(mount_or_device: &str) -> Result<RemovableDrive> {
    let path = PathBuf::from(mount_or_device);
    let drives = list_removable_drives()?;

    if let Some(found) = drives.iter().find(|d| {
        d.mountpoint == path || d.device == path || d.name == mount_or_device
    }) {
        return Ok(found.clone());
    }

    // Allow an explicitly mounted path even if lsblk did not mark it removable
    // (some USB readers report RM=0).
    if path.is_dir() {
        let meta_fstype = fstype_from_findmnt(&path).unwrap_or_default();
        let total = total_space(&path).unwrap_or(0);
        return Ok(RemovableDrive {
            name: path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| mount_or_device.to_string()),
            device: path.clone(),
            mountpoint: path,
            size_bytes: total,
            fstype: meta_fstype,
            label: None,
            transport: None,
        });
    }

    bail!(
        "drive not found or not mounted: {mount_or_device}\nTip: insert the SD card and wait for it to mount under /media or /run/media."
    );
}

fn fstype_from_findmnt(path: &Path) -> Option<String> {
    let output = Command::new("findmnt")
        .args(["-n", "-o", "FSTYPE", "--target"])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub fn collect_stats(drive: &RemovableDrive) -> Result<DriveStats> {
    let root = &drive.mountpoint;
    let total = total_space(root).with_context(|| format!("statvfs {}", root.display()))?;
    let free = available_space(root).with_context(|| format!("available {}", root.display()))?;

    let mut file_count = 0u64;
    let mut dir_count = 0u64;
    let mut occupied = 0u64;

    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        let ft = entry.file_type();
        if ft.is_dir() {
            // Don't count the root itself.
            if entry.path() != root {
                dir_count += 1;
            }
        } else if ft.is_file() {
            file_count += 1;
            if let Ok(meta) = entry.metadata() {
                occupied += meta.len();
            }
        }
    }

    let fstype = if drive.fstype.is_empty() {
        fstype_from_findmnt(root).unwrap_or_else(|| "unknown".into())
    } else {
        drive.fstype.clone()
    };

    Ok(DriveStats {
        file_count,
        dir_count,
        occupied_bytes: occupied,
        total_bytes: total,
        free_bytes: free,
        fstype,
    })
}

pub fn list_top_level(drive: &RemovableDrive) -> Result<Vec<DirEntryView>> {
    let mut entries = Vec::new();
    let mut rd = fs::read_dir(&drive.mountpoint)
        .with_context(|| format!("read {}", drive.mountpoint.display()))?;

    while let Some(entry) = rd.next() {
        let entry = entry?;
        let meta = entry.metadata()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        entries.push(DirEntryView {
            name,
            is_dir: meta.is_dir(),
            size_bytes: if meta.is_file() {
                Some(meta.len())
            } else {
                None
            },
        });
    }

    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(entries)
}

pub fn existing_bin_files(drive: &RemovableDrive) -> Result<Vec<String>> {
    let mut bins = Vec::new();
    for entry in fs::read_dir(&drive.mountpoint)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.to_lowercase().ends_with(".bin") && entry.file_type()?.is_file() {
            bins.push(name);
        }
    }
    bins.sort();
    Ok(bins)
}

pub fn free_percent(stats: &DriveStats) -> f64 {
    if stats.total_bytes == 0 {
        0.0
    } else {
        (stats.free_bytes as f64 / stats.total_bytes as f64) * 100.0
    }
}

pub fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

pub fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

pub fn is_fat_family(fstype: &str) -> bool {
    matches!(
        fstype.to_ascii_lowercase().as_str(),
        "vfat" | "fat" | "fat16" | "fat32" | "msdos"
    )
}
