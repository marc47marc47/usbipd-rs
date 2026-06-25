use crate::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Os {
    Windows,
    Macos,
    Linux,
}

pub(crate) fn current_os() -> Os {
    match std::env::consts::OS {
        "windows" => Os::Windows,
        "macos" => Os::Macos,
        _ => Os::Linux,
    }
}

#[derive(Clone, Copy)]
pub(crate) enum InstallStep {
    /// Run a package-manager-style command (e.g., `cargo install foo`).
    Command {
        program: &'static str,
        args: &'static [&'static str],
    },
    /// Download a URL and act on the file.
    Download {
        url: &'static str,
        /// Override filename when URL doesn't yield a sensible one (e.g.,
        /// query-only URLs like `?id=65`).
        filename: Option<&'static str>,
        action: DownloadAction,
    },
    /// Print a download URL and manual steps without fetching anything.
    /// For vendors whose web server blocks automated downloads (e.g. FTDI
    /// returns HTTP 403 to any non-browser client), so an auto-download would
    /// just fail for every user.
    Manual {
        url: &'static str,
        instructions: &'static str,
    },
    /// Extract an archive that's already committed to `windows-driver/` in
    /// the repo — no network round-trip. Use for upstreams whose only
    /// distribution channel is awkward (e.g. SourceForge HTML redirects).
    /// The file lives at `<project>/windows-driver/<filename>` regardless of
    /// host OS (that path is the project's canonical bundled-binary dir).
    LocalArchive {
        filename: &'static str,
        action: DownloadAction,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum DownloadAction {
    /// Save and prompt user to run with given instructions (no execution).
    PromptToRun { instructions: &'static str },
    /// Extract a zip into `windows-driver/<tool_id>/`. Used for CLI bundles.
    ExtractToBundle { binary_hint: &'static str },
    /// Extract a zip, locate the inner binary, then prompt user to run it
    /// (typically as administrator). Used for driver installers shipped as zip.
    ExtractAndPrompt {
        binary_hint: &'static str,
        instructions: &'static str,
    },
}

pub(crate) struct ToolSpec {
    id: &'static str,
    name: &'static str,
    purpose: &'static str,
    /// Resolve install step for the given OS, or None if unsupported there.
    resolve: fn(Os) -> Option<InstallStep>,
    /// Optional: command to test if already installed (returns Some if found).
    check_command: Option<&'static str>,
}
pub(crate) fn cmd_list_tools() -> Result<()> {
    let os = current_os();
    println!("Detected OS: {os:?}\n");
    println!("{:<12}  {:<10}  {:<10}  {}", "ID", "STATUS", "PROVIDER", "PURPOSE");
    println!("{}", "-".repeat(80));
    for tool in TOOLS {
        let status = match tool.check_command {
            Some(cmd) => {
                if which_cmd(cmd).is_some() {
                    "installed"
                } else {
                    "missing"
                }
            }
            None => "—",
        };
        let provider = match (tool.resolve)(os) {
            Some(InstallStep::Command { program, .. }) => program,
            Some(InstallStep::Download { .. }) => "download",
            Some(InstallStep::Manual { .. }) => "manual",
            Some(InstallStep::LocalArchive { .. }) => "bundled",
            None => "(n/a on this OS)",
        };
        println!("{:<12}  {:<10}  {:<10}  {}", tool.id, status, provider, tool.purpose);
    }
    println!();
    println!("Run `usbipd-rs --install <ID>` to install.");
    Ok(())
}

pub(crate) fn which_cmd(cmd: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for ext in exts {
            let candidate = dir.join(format!("{cmd}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub(crate) fn cmd_install(tool_id: &str) -> Result<()> {
    let tool = TOOLS
        .iter()
        .find(|t| t.id == tool_id)
        .ok_or_else(|| anyhow::anyhow!("Unknown tool '{tool_id}'. Try `--list-tools`."))?;
    let os = current_os();
    let step = (tool.resolve)(os).ok_or_else(|| {
        anyhow::anyhow!("{} has no install path on {:?}", tool.name, os)
    })?;

    println!("=== Installing {} ===", tool.name);
    println!("Purpose: {}", tool.purpose);
    println!("OS:      {os:?}\n");

    match step {
        InstallStep::Command { program, args } => {
            run_command(program, args)?;
        }
        InstallStep::Download { url, filename, action } => {
            let dest_dir = bundle_dir()?;
            std::fs::create_dir_all(&dest_dir)
                .with_context(|| format!("Could not create {}", dest_dir.display()))?;
            let fname = filename.unwrap_or_else(|| {
                url.rsplit('/').next().unwrap_or("download.bin")
            });
            let dest = dest_dir.join(fname);
            download_file(url, &dest)?;
            apply_download_action(tool.id, &dest, &dest_dir, action)?;
        }
        InstallStep::LocalArchive { filename, action } => {
            let src = local_archive_path(filename)?;
            if !src.is_file() {
                anyhow::bail!(
                    "Bundled archive missing: {}\n\
                     Re-clone the repository or place the file there manually.",
                    src.display()
                );
            }
            let size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
            println!("  Source:      {} ({} bytes, bundled)", src.display(), size);
            let dest_dir = bundle_dir()?;
            std::fs::create_dir_all(&dest_dir)
                .with_context(|| format!("Could not create {}", dest_dir.display()))?;
            apply_download_action(tool.id, &src, &dest_dir, action)?;
        }
        InstallStep::Manual { url, instructions } => {
            println!("  Download URL: {url}");
            println!("\n  Manual steps:");
            for line in instructions.lines() {
                println!("    {line}");
            }
        }
    }
    println!("\n  Done.");
    Ok(())
}

pub(crate) fn apply_download_action(
    tool_id: &str,
    src: &Path,
    dest_dir: &Path,
    action: DownloadAction,
) -> Result<()> {
    match action {
        DownloadAction::ExtractToBundle { binary_hint } => {
            let extract_to = dest_dir.join(tool_id);
            std::fs::create_dir_all(&extract_to)?;
            extract_zip(src, &extract_to)?;
            println!("\n  Extracted to: {}", extract_to.display());
            match find_in_dir(&extract_to, binary_hint) {
                Some(p) => println!("  Binary:       {}", p.display()),
                None => println!(
                    "  Binary '{binary_hint}' not found inside archive — inspect the directory manually."
                ),
            }
        }
        DownloadAction::ExtractAndPrompt { binary_hint, instructions } => {
            let extract_to = dest_dir.join(tool_id);
            std::fs::create_dir_all(&extract_to)?;
            extract_zip(src, &extract_to)?;
            println!("\n  Extracted to: {}", extract_to.display());
            match find_in_dir(&extract_to, binary_hint) {
                Some(p) => println!("  Installer:    {}", p.display()),
                None => println!(
                    "  Installer '{binary_hint}' not found — inspect the directory manually."
                ),
            }
            println!("\n  Manual steps:");
            for line in instructions.lines() {
                println!("    {line}");
            }
        }
        DownloadAction::PromptToRun { instructions } => {
            println!("\n  Saved to: {}", src.display());
            println!("\n  Manual steps:");
            for line in instructions.lines() {
                println!("    {line}");
            }
        }
    }
    Ok(())
}

/// Bundled archives (committed to the repo) always live under
/// `<project>/windows-driver/` regardless of host OS — that's the project's
/// canonical home for binary blobs (see CLAUDE.md).
pub(crate) fn local_archive_path(filename: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let project = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("Could not derive project root from exe path"))?;
    Ok(project.join("windows-driver").join(filename))
}

pub(crate) fn bundle_dir() -> Result<PathBuf> {
    // Prefer <project>/windows-driver/ for Win (matches existing layout),
    // <project>/tools/ for Mac/Linux.
    let exe = std::env::current_exe()?;
    let project = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("Could not derive project root from exe path"))?;
    let dir = if current_os() == Os::Windows {
        project.join("windows-driver")
    } else {
        project.join("tools")
    };
    Ok(dir)
}

pub(crate) fn download_file(url: &str, dest: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::metadata(dest) {
        if meta.len() > 0 {
            println!("  Existing:    {} ({} bytes)", dest.display(), meta.len());
            println!("  (delete the file to force re-download)");
            return Ok(());
        }
    }
    println!("  Downloading: {url}");
    let response = ureq::get(url).call().context("HTTP request failed")?;
    let total: Option<u64> = response
        .header("Content-Length")
        .and_then(|s| s.parse().ok());
    if let Some(t) = total {
        println!("  Size:        {t} bytes");
    }

    // Stage to <dest>.partial, then atomic rename. This avoids the
    // ERROR_SHARING_VIOLATION on Windows when AV/Defender (or our previous
    // run) still holds a handle to the existing dest file.
    let staging = dest.with_extension(match dest.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.partial"),
        None => "partial".into(),
    });
    let _ = std::fs::remove_file(&staging);

    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(&staging)
        .with_context(|| format!("Could not create {}", staging.display()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut written: u64 = 0;
    let mut last_pct = -1i32;
    loop {
        let n = reader.read(&mut buf).context("Read from server failed")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("Disk write failed")?;
        written += n as u64;
        if let Some(t) = total {
            let pct = (written * 100 / t.max(1)) as i32;
            if pct != last_pct && pct % 10 == 0 {
                print!("  {pct}%... ");
                let _ = std::io::stdout().flush();
                last_pct = pct;
            }
        }
    }
    drop(file);
    println!();

    // Replace the (possibly locked) destination with the staged file.
    if dest.exists() {
        let _ = std::fs::remove_file(dest);
    }
    std::fs::rename(&staging, dest).with_context(|| {
        format!("Could not move {} → {}", staging.display(), dest.display())
    })?;

    println!("  Saved to:    {} ({} bytes)", dest.display(), written);
    Ok(())
}

pub(crate) fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    println!("  Extracting:  {}", zip_path.display());
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("Could not open {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("Not a valid zip file")?;
    archive.extract(dest).context("Zip extraction failed")?;
    Ok(())
}

pub(crate) fn find_in_dir(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_in_dir(&path, name) {
                return Some(found);
            }
        }
    }
    None
}

pub(crate) fn run_command(program: &str, args: &[&str]) -> Result<()> {
    println!("  Running:     {} {}", program, args.join(" "));
    let status = Command::new(program).args(args).status().with_context(|| {
        format!("Could not invoke `{program}` (is it on PATH?)")
    })?;
    if !status.success() {
        anyhow::bail!("{program} exited with {status}");
    }
    Ok(())
}


pub mod tools;
pub(crate) use tools::*;
