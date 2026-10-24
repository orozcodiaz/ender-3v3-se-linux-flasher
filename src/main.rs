mod drive;
mod flash;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use drive::{
    bytes_to_mb, collect_stats, existing_bin_files, free_percent, human_bytes, is_fat_family,
    list_removable_drives, list_top_level, resolve_drive, DriveStats, RemovableDrive,
};
use flash::{copy_firmware, suggest_dest_name, validate_dest_name, validate_firmware_path};
use inquire::{Confirm, CustomType, Select, Text};
use std::path::PathBuf;

/// Inspect a removable SD card and flash Ender 3 V3 SE firmware (.bin).
///
/// Recommended flow (drive-first): pick the card → review health/contents →
/// choose firmware → confirm copy. That matches “is this card OK?” before writing.
#[derive(Debug, Parser)]
#[command(name = "ender-flash")]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Firmware .bin path (skips the file prompt in the wizard)
    #[arg(long, global = true)]
    bin: Option<PathBuf>,

    /// Mountpoint or device of the SD card (skips drive selection)
    #[arg(long, global = true)]
    drive: Option<String>,

    /// Destination filename on the card (e.g. hfie93.bin; new name each attempt)
    #[arg(long, global = true)]
    as_name: Option<String>,

    /// Skip the final confirmation prompt
    #[arg(long, global = true)]
    yes: bool,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Interactive flash wizard (default)
    Flash,
    /// Only select a drive and print contents + stats
    Inspect,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{} {:#}", "error:".red().bold(), err);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.as_ref().unwrap_or(&Commands::Flash) {
        Commands::Flash => run_flash_wizard(&cli),
        Commands::Inspect => run_inspect(&cli),
    }
}

fn run_inspect(cli: &Cli) -> Result<()> {
    print_banner();
    let drive = select_drive(cli.drive.as_deref())?;
    print_drive_report(&drive)?;
    Ok(())
}

fn run_flash_wizard(cli: &Cli) -> Result<()> {
    print_banner();
    println!(
        "{}",
        "Flow: select SD card → inspect → choose .bin → confirm → copy"
            .dimmed()
    );
    println!();

    // 1) Drive first — primary job is validating the card is usable.
    let drive = select_drive(cli.drive.as_deref())?;
    print_drive_report(&drive)?;

    let stats = collect_stats(&drive)?;
    print_readiness(&stats);

    let proceed = if cli.yes {
        true
    } else {
        Confirm::new("Flash a firmware .bin to this drive?")
            .with_default(true)
            .prompt()?
    };
    if !proceed {
        println!("{}", "Aborted.".yellow());
        return Ok(());
    }

    // 2) Firmware path with existence / extension checks.
    let firmware = select_firmware(cli.bin.as_ref())?;
    let fw_size = std::fs::metadata(&firmware)?.len();
    println!(
        "{} {} ({})",
        "Firmware:".bold(),
        firmware.display(),
        human_bytes(fw_size)
    );

    if fw_size > stats.free_bytes {
        bail!(
            "not enough free space: need {}, free {}",
            human_bytes(fw_size),
            human_bytes(stats.free_bytes)
        );
    }

    let bins = existing_bin_files(&drive)?;
    if !bins.is_empty() {
        println!(
            "{} {}",
            "Existing .bin on card:".yellow(),
            bins.join(", ")
        );
        println!(
            "{}",
            "Tip: use a *new* short filename each flash attempt — printers ignore names they already saw."
                .dimmed()
        );
    }

    // 3) Destination name (unique short name by default).
    let dest_name = select_dest_name(cli.as_name.as_deref())?;

    println!();
    println!("{}", "Summary".bold().underline());
    println!("  source : {}", firmware.display());
    println!(
        "  dest   : {}/{}",
        drive.mountpoint.display(),
        dest_name
    );
    println!("  size   : {}", human_bytes(fw_size));
    println!();

    let confirm = if cli.yes {
        true
    } else {
        Confirm::new("Copy firmware to the SD card?")
            .with_default(true)
            .prompt()?
    };
    if !confirm {
        println!("{}", "Aborted.".yellow());
        return Ok(());
    }

    // 4) Copy + fsync.
    let dest = copy_firmware(&firmware, &drive.mountpoint, &dest_name)?;
    println!("{} {}", "Wrote".green().bold(), dest.display());

    print_next_steps(&dest_name);
    Ok(())
}

fn print_banner() {
    println!(
        "{}",
        "ender-flash — Ender 3 V3 SE SD firmware helper"
            .bold()
            .cyan()
    );
}

fn select_drive(preset: Option<&str>) -> Result<RemovableDrive> {
    if let Some(preset) = preset {
        let drive = resolve_drive(preset)?;
        println!(
            "{} {}",
            "Drive:".bold(),
            drive.display_label()
        );
        return Ok(drive);
    }

    let drives = list_removable_drives().context("discover removable drives")?;
    if drives.is_empty() {
        bail!(
            "no mounted removable drives found.\nInsert the SD card (or USB reader), wait for it to mount, then retry."
        );
    }

    let labels: Vec<String> = drives.iter().map(|d| d.display_label()).collect();
    let choice = Select::new("Select removable drive (↑/↓, Enter):", labels)
        .with_page_size(10)
        .prompt()?;

    let idx = drives
        .iter()
        .position(|d| d.display_label() == choice)
        .context("selected drive disappeared")?;
    Ok(drives[idx].clone())
}

fn select_firmware(preset: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(path) = preset {
        return validate_firmware_path(path);
    }

    let input = Text::new("Path to firmware .bin file:")
        .with_help_message("Absolute or relative path; must exist and end with .bin")
        .with_validator(|s: &str| {
            if s.trim().is_empty() {
                return Ok(inquire::validator::Validation::Invalid(
                    "path is required".into(),
                ));
            }
            match validate_firmware_path(std::path::Path::new(s.trim())) {
                Ok(_) => Ok(inquire::validator::Validation::Valid),
                Err(e) => Ok(inquire::validator::Validation::Invalid(
                    format!("{e:#}").into(),
                )),
            }
        })
        .prompt()?;

    validate_firmware_path(std::path::Path::new(input.trim()))
}

fn select_dest_name(preset: Option<&str>) -> Result<String> {
    if let Some(name) = preset {
        return validate_dest_name(name);
    }

    let suggested = suggest_dest_name();
    let name = CustomType::<String>::new("Destination filename on SD card:")
        .with_default(suggested)
        .with_help_message("Random short name each attempt (e.g. hfie93.bin). Stem ≤8 chars, ends with .bin.")
        .with_parser(&|s| validate_dest_name(s).map_err(|_| ()))
        .with_error_message("Use a short ≤8-char alphanumeric stem ending in .bin (e.g. hfie93.bin)")
        .prompt()?;
    Ok(name)
}

fn print_drive_report(drive: &RemovableDrive) -> Result<()> {
    let stats = collect_stats(drive)?;
    let entries = list_top_level(drive)?;

    println!();
    println!("{}", "Drive".bold().underline());
    println!("  mount  : {}", drive.mountpoint.display());
    println!("  device : {}", drive.device.display());
    if let Some(label) = &drive.label {
        println!("  label  : {label}");
    }
    if let Some(tran) = &drive.transport {
        println!("  bus    : {tran}");
    }

    println!();
    println!("{}", "Stats".bold().underline());
    println!("  filesystem : {}", stats.fstype);
    println!(
        "  contents   : {} files, {} folders",
        stats.file_count, stats.dir_count
    );
    println!(
        "  occupied   : {} ({:.1} MB)",
        human_bytes(stats.occupied_bytes),
        bytes_to_mb(stats.occupied_bytes)
    );
    println!(
        "  free       : {:.1} MB ({:.1}%)",
        bytes_to_mb(stats.free_bytes),
        free_percent(&stats)
    );
    println!(
        "  capacity   : {} ({:.1} MB)",
        human_bytes(stats.total_bytes),
        bytes_to_mb(stats.total_bytes)
    );

    println!();
    println!("{}", "Top-level contents".bold().underline());
    if entries.is_empty() {
        println!("  {}", "(empty)".dimmed());
    } else {
        let show = entries.iter().take(30);
        for e in show {
            if e.is_dir {
                println!("  {} {}/", "dir ".blue(), e.name);
            } else {
                let size = e
                    .size_bytes
                    .map(human_bytes)
                    .unwrap_or_else(|| "?".into());
                println!("  {} {:<24} {}", "file".green(), e.name, size);
            }
        }
        if entries.len() > 30 {
            println!(
                "  {}",
                format!("… and {} more", entries.len() - 30).dimmed()
            );
        }
    }
    println!();
    Ok(())
}

fn print_readiness(stats: &DriveStats) {
    println!("{}", "Readiness".bold().underline());
    if is_fat_family(&stats.fstype) {
        println!(
            "  {} filesystem looks like FAT/FAT32 ({})",
            "OK".green().bold(),
            stats.fstype
        );
    } else {
        println!(
            "  {} filesystem is '{}' — Ender boards usually need FAT32 (4096 allocation)",
            "WARN".yellow().bold(),
            stats.fstype
        );
    }

    let free_pct = free_percent(stats);
    if stats.free_bytes < 256 * 1024 {
        println!(
            "  {} very little free space ({:.1} MB)",
            "WARN".yellow().bold(),
            bytes_to_mb(stats.free_bytes)
        );
    } else {
        println!(
            "  {} {:.1} MB free ({:.1}%)",
            "OK".green().bold(),
            bytes_to_mb(stats.free_bytes),
            free_pct
        );
    }
    println!();
}

fn print_next_steps(dest_name: &str) {
    println!();
    println!("{}", "Next steps (Ender 3 V3 SE)".bold().underline());
    println!("  1. Safely eject/unmount the SD card.");
    println!("  2. Power off the printer.");
    println!("  3. Insert the card into the motherboard slot (not only the screen).");
    println!("  4. Power on and wait ~10–15 seconds (screen may stay black).");
    println!("  5. Power off, remove the card, delete or rename `{dest_name}` before the next attempt.");
    println!();
    println!(
        "{}",
        "Note: C14 boards flashing Klipper sometimes need the file under STM32F4_UPDATE/ — check your board guide."
            .dimmed()
    );
}
