//! Operating-system specifics, kept behind small functions so the engine and
//! the interface stay portable. Windows and Linux are supported; features that
//! only exist on one of them (notification area, Explorer menu, file
//! association, systemd timers) have no-op fallbacks on the other.

pub mod apps;
pub mod autostart;
pub mod context_menu;
pub mod file_association;
pub mod instance;
pub mod known_paths;
pub mod systemd;
pub mod tray;
pub mod vss;

use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

/// Lower priority for the current thread while the guard lives.
pub struct BackgroundThread {
    #[cfg(windows)]
    active: bool,
}

impl BackgroundThread {
    pub fn enter() -> Self {
        Self {
            #[cfg(windows)]
            active: windows::thread_background_begin(),
        }
    }
}

impl Drop for BackgroundThread {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.active {
            windows::thread_background_end();
        }
    }
}

/// `true` if the computer runs on battery power right now.
pub fn on_battery() -> bool {
    #[cfg(windows)]
    {
        windows::on_battery()
    }
    #[cfg(target_os = "linux")]
    {
        linux::on_battery()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        false
    }
}

/// Asks for a secret on the console without showing it. `None` without a console.
pub fn read_secret(prompt: &str) -> Option<String> {
    use std::io::Write;
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    #[cfg(windows)]
    {
        use std::io::BufRead;
        let console = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONIN$")
            .ok()?;
        let restore = windows::hide_console_input(&console);
        let mut line = String::new();
        let result = std::io::BufReader::new(&console).read_line(&mut line);
        if let Some(mode) = restore {
            windows::set_console_mode(&console, mode);
        }
        eprintln!();
        result.ok()?;
        let secret = line.trim_end_matches(['\r', '\n']).to_string();
        (!secret.is_empty()).then_some(secret)
    }
    #[cfg(target_os = "linux")]
    {
        linux::read_secret_line().filter(|s| !s.is_empty())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        use std::io::BufRead;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).ok()?;
        let secret = line.trim_end_matches(['\r', '\n']).to_string();
        (!secret.is_empty()).then_some(secret)
    }
}

/// Moves a folder to the recycle bin. Fails if that is not possible (for
/// example on network shares, or on Linux), so the caller can decide what to do.
pub fn delete_to_recycle_bin(path: &Path) -> std::io::Result<()> {
    // Tests and demo runs must not fill the user's recycle bin.
    if !system_changes_allowed() {
        return Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
    }
    #[cfg(windows)]
    {
        windows::recycle(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

/// Demo and test runs must not change anything outside their folders.
pub fn system_changes_allowed() -> bool {
    !cfg!(test) && known_paths::profile_override().is_none()
}

/// Marks a folder as hidden. Failures are ignored: hiding is cosmetic. On
/// Linux, names starting with a dot are hidden already.
pub fn set_hidden(path: &Path) {
    #[cfg(windows)]
    if let Err(err) = windows::set_hidden(path) {
        tracing::debug!("could not hide {}: {err}", path.display());
    }
    #[cfg(not(windows))]
    let _ = path;
}

/// Lower CPU and I/O priority for unattended runs.
pub fn enter_background_mode() {
    #[cfg(windows)]
    windows::enter_background_mode();
    #[cfg(target_os = "linux")]
    linux::enter_background_mode();
}

/// Protects secret bytes for the current user: Windows DPAPI. On Linux the
/// bytes are returned unchanged and [`write_private_file`] makes the file
/// readable by its owner only (like SSH keys).
pub fn protect_for_user(data: &[u8]) -> std::io::Result<Vec<u8>> {
    #[cfg(windows)]
    {
        windows::dpapi_protect(data)
    }
    #[cfg(not(windows))]
    {
        Ok(data.to_vec())
    }
}

pub fn unprotect_for_user(data: &[u8]) -> std::io::Result<Vec<u8>> {
    #[cfg(windows)]
    {
        windows::dpapi_unprotect(data)
    }
    #[cfg(not(windows))]
    {
        Ok(data.to_vec())
    }
}

/// Writes a file that only the current user may read.
pub fn write_private_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(data)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)
    }
}

/// Attach to the console of the parent process (for command-line use of a
/// GUI-subsystem executable). Does nothing if there is no parent console.
pub fn attach_parent_console() {
    #[cfg(windows)]
    windows::attach_parent_console();
}

/// Size of the primary screen's work area (without the taskbar) in logical
/// points, i.e. already divided by the display scaling.
pub fn work_area_points() -> Option<(f32, f32)> {
    #[cfg(windows)]
    {
        windows::work_area_points()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Show a last-resort error message when the window cannot be created.
#[cfg(feature = "gui")]
pub fn show_fatal_error(message: &str) {
    let _ = rfd::MessageDialog::new()
        .set_title("AeternaVault")
        .set_description(message)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

/// The default destination: an `AeternaVault` folder on the drive or partition
/// with the most free space other than the system drive, otherwise in the
/// user's home folder (`C:\Users\<name>\AeternaVault`, `~/AeternaVault`).
pub fn default_destination() -> PathBuf {
    #[cfg(windows)]
    if known_paths::profile_override().is_none()
        && let Some(root) = windows::roomiest_secondary_fixed_drive()
    {
        return root.join("AeternaVault");
    }
    #[cfg(target_os = "linux")]
    if known_paths::profile_override().is_none()
        && let Some(root) = linux::roomiest_data_mount()
    {
        return root.join("AeternaVault");
    }

    known_paths::KnownPaths::current()
        .get("HOME")
        .map(|home| home.join("AeternaVault"))
        .unwrap_or_else(|| PathBuf::from("AeternaVault"))
}

/// Free space on the volume that contains `path`, if it can be determined.
pub fn free_space(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        windows::free_space(path)
    }
    #[cfg(target_os = "linux")]
    {
        linux::free_space(path)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = path;
        None
    }
}

/// (free, total) bytes of the drive holding `path`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        windows::disk_space(path)
    }
    #[cfg(target_os = "linux")]
    {
        linux::disk_space(path)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = path;
        None
    }
}

/// The window program (`aeternavault`), next to the running executable.
/// Autostart, the Explorer menu and the file association start it.
pub fn gui_executable() -> std::io::Result<PathBuf> {
    sibling_executable("aeternavault")
}

/// The command-line program (`aeternavault-cli`), next to the running executable.
pub fn cli_executable() -> std::io::Result<PathBuf> {
    sibling_executable("aeternavault-cli")
}

fn sibling_executable(stem: &str) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let name = format!("{stem}{}", std::env::consts::EXE_SUFFIX);
    if exe
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&name))
    {
        return Ok(exe);
    }
    let sibling = exe.with_file_name(&name);
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} was not found", sibling.display()),
        ))
    }
}

/// Exclusive lock on a file, held until the returned value is dropped (Linux).
#[cfg(target_os = "linux")]
pub fn lock_file(path: &Path) -> Option<std::fs::File> {
    linux::lock_file(path)
}

/// Whether two paths are on the same drive (Windows: drive letter or network
/// share; Linux: file system). Paths that do not exist yet are judged by
/// their nearest existing parent.
pub fn same_drive(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::Component;
        let prefix = |p: &Path| match p.components().next() {
            Some(Component::Prefix(prefix)) => {
                Some(prefix.as_os_str().to_string_lossy().to_lowercase())
            }
            _ => None,
        };
        matches!((prefix(a), prefix(b)), (Some(x), Some(y)) if x == y)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let device = |p: &Path| {
            p.ancestors()
                .find_map(|a| std::fs::metadata(a).ok())
                .map(|m| m.dev())
        };
        matches!((device(a), device(b)), (Some(x), Some(y)) if x == y)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (a, b);
        false
    }
}

/// Whether a process with this id runs on this computer. `None` if that
/// cannot be told (other systems, no permission to ask).
pub fn process_running(pid: u32) -> Option<bool> {
    #[cfg(windows)]
    {
        windows::process_running(pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux::process_running(pid)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

pub fn computer_name() -> String {
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        return name;
    }
    if let Ok(name) = std::env::var("HOSTNAME") {
        return name;
    }
    std::fs::read_to_string("/etc/hostname")
        .map(|n| n.trim().to_string())
        .ok()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "computer".to_string())
}

pub fn user_name() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
}

/// Open a folder (or the folder containing a file) in the system file manager.
pub fn open_in_file_manager(path: &Path) {
    let target = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    #[cfg(windows)]
    let result = std::process::Command::new("explorer").arg(target).spawn();
    #[cfg(not(windows))]
    let result = std::process::Command::new("xdg-open").arg(target).spawn();
    if let Err(err) = result {
        tracing::warn!("could not open {}: {err}", target.display());
    }
}

/// Opens a file with the program the system associates with it.
pub fn open_with_default_program(path: &Path) {
    #[cfg(windows)]
    let result = std::process::Command::new("rundll32.exe")
        .arg("shell32.dll,ShellExec_RunDLL")
        .arg(path)
        .spawn();
    #[cfg(not(windows))]
    let result = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(err) = result {
        tracing::warn!("could not open {}: {err}", path.display());
    }
}

/// Open a text file in the default editor (Notepad on Windows).
pub fn open_in_editor(path: &Path) {
    #[cfg(windows)]
    let result = std::process::Command::new("notepad.exe").arg(path).spawn();
    #[cfg(not(windows))]
    let result = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(err) = result {
        tracing::warn!("could not open {}: {err}", path.display());
    }
}
