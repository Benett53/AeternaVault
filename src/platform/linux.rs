//! Linux counterparts of the Windows helpers, using `libc` directly.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}

/// Free space for unprivileged users on the file system that holds `path`.
/// (free for this user, total) bytes of the file system holding `path`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let mut probe = path;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    let path = c_path(probe).ok()?;
    // SAFETY: valid NUL-terminated string; `stat` is plain data filled by the call.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        let unit = stat.f_frsize as u64;
        Some((stat.f_bavail as u64 * unit, stat.f_blocks as u64 * unit))
    }
}

pub fn free_space(path: &Path) -> Option<u64> {
    let mut probe = path;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    let path = c_path(probe).ok()?;
    // SAFETY: `path` is a valid NUL-terminated string and `stat` is plain data
    // filled by the call.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        Some(stat.f_bavail as u64 * stat.f_frsize as u64)
    }
}

/// File systems that hold user data (not memory, kernel or snap mounts).
const DATA_FILE_SYSTEMS: &[&str] = &[
    "ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs", "jfs", "reiserfs", "zfs", "ntfs", "ntfs3",
    "fuseblk", "exfat", "vfat", "bcachefs",
];

/// The mounted data partition other than the root file system with the most
/// free space that the user can write to (e.g. a second disk under /mnt).
pub fn roomiest_data_mount() -> Option<PathBuf> {
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    let root_device = mounts
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some("/"))
        .and_then(|l| l.split_whitespace().next())
        .map(str::to_string);
    let mut best: Option<(u64, PathBuf)> = None;
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(device), Some(point), Some(kind)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let point = point.replace("\\040", " ");
        let system = point == "/"
            || [
                "/boot",
                "/proc",
                "/sys",
                "/dev",
                "/run/user",
                "/snap",
                "/var",
                "/usr",
                "/tmp",
            ]
            .iter()
            .any(|p| point == *p || point.starts_with(&format!("{p}/")));
        if system
            || !DATA_FILE_SYSTEMS.contains(&kind)
            || root_device.as_deref() == Some(device)
            || !device.starts_with('/')
        {
            continue;
        }
        let point = PathBuf::from(point);
        if !writable(&point) {
            continue;
        }
        if let Some(free) = free_space(&point)
            && best.as_ref().is_none_or(|(most, _)| free > *most)
        {
            best = Some((free, point));
        }
    }
    best.map(|(_, point)| point)
}

fn writable(path: &Path) -> bool {
    let Ok(path) = c_path(path) else {
        return false;
    };
    // SAFETY: valid NUL-terminated path.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// `true` if the computer has a battery and no mains power right now.
pub fn on_battery() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    let mut mains_online = false;
    let mut has_mains = false;
    let mut has_battery = false;
    for entry in entries.flatten() {
        let dir = entry.path();
        let kind = std::fs::read_to_string(dir.join("type")).unwrap_or_default();
        match kind.trim() {
            "Mains" => {
                has_mains = true;
                mains_online |=
                    std::fs::read_to_string(dir.join("online")).is_ok_and(|v| v.trim() == "1");
            }
            "Battery" => has_battery = true,
            _ => {}
        }
    }
    has_battery && has_mains && !mains_online
}

/// Lower CPU and I/O priority for the whole process.
pub fn enter_background_mode() {
    // SAFETY: plain system calls without pointers.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

/// Reads a line from the terminal without echo. `None` without a terminal.
pub fn read_secret_line() -> Option<String> {
    use std::io::BufRead;
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    use std::os::unix::io::AsRawFd;
    let fd = tty.as_raw_fd();
    // SAFETY: `fd` belongs to the open terminal; `termios` is plain data.
    let previous = unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut termios) != 0 {
            None
        } else {
            let saved = termios;
            termios.c_lflag &= !libc::ECHO;
            libc::tcsetattr(fd, libc::TCSANOW, &termios);
            Some(saved)
        }
    };
    let mut line = String::new();
    let result = std::io::BufReader::new(&tty).read_line(&mut line);
    if let Some(saved) = previous {
        // SAFETY: restores the settings read above on the same descriptor.
        unsafe {
            libc::tcsetattr(fd, libc::TCSANOW, &saved);
        }
    }
    eprintln!();
    result.ok()?;
    Some(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Holds an exclusive lock on a file for the lifetime of the process.
pub fn lock_file(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    // SAFETY: the descriptor belongs to the open file.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 };
    locked.then_some(file)
}

/// Whether a process with this id runs. `None` if that cannot be told.
pub fn process_running(pid: u32) -> Option<bool> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    // SAFETY: signal 0 only checks whether the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Some(true);
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Some(false),
        Some(libc::EPERM) => Some(true),
        _ => None,
    }
}
