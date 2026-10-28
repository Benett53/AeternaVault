//! Thin wrappers around a handful of Win32 calls.
//!
//! `windows-sys` is used instead of the higher-level `windows` crate because
//! only a few plain C functions are needed; it compiles faster and adds no
//! COM machinery.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, GetDiskFreeSpaceExW, GetDriveTypeW, GetFileAttributesW,
    GetLogicalDrives, INVALID_FILE_ATTRIBUTES, SetFileAttributesW,
};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, PROCESS_MODE_BACKGROUND_BEGIN, SetPriorityClass,
};
use windows_sys::Win32::System::WindowsProgramming::DRIVE_FIXED;

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn attach_parent_console() {
    // SAFETY: plain Win32 call without pointers; failure simply means there is
    // no parent console (e.g. started from Explorer or the Task Scheduler).
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// Turns off echo for a console input handle; returns the previous mode.
pub fn hide_console_input(console: &std::fs::File) -> Option<u32> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{ENABLE_ECHO_INPUT, GetConsoleMode, SetConsoleMode};
    let handle = console.as_raw_handle();
    let mut mode = 0u32;
    // SAFETY: the handle belongs to an open console input file.
    unsafe {
        if GetConsoleMode(handle, &mut mode) == 0 {
            return None;
        }
        SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT);
    }
    Some(mode)
}

pub fn set_console_mode(console: &std::fs::File, mode: u32) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::SetConsoleMode;
    // SAFETY: the handle belongs to an open console input file.
    unsafe {
        SetConsoleMode(console.as_raw_handle(), mode);
    }
}

/// The fixed drive or partition other than the system drive with the most free
/// space (removable and network drives are left out).
pub fn roomiest_secondary_fixed_drive() -> Option<PathBuf> {
    let system = std::env::var("SystemDrive")
        .unwrap_or_else(|_| "C:".to_string())
        .to_ascii_uppercase();

    // SAFETY: no arguments; returns a bit mask of available drive letters.
    let mask = unsafe { GetLogicalDrives() };
    let mut best: Option<(u64, PathBuf)> = None;
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        if format!("{letter}:") == system || letter < 'C' {
            continue;
        }
        let root = PathBuf::from(format!("{letter}:\\"));
        let wide_root = wide(&root);
        // SAFETY: `wide_root` is a valid, NUL-terminated UTF-16 string.
        let kind = unsafe { GetDriveTypeW(wide_root.as_ptr()) };
        if kind != DRIVE_FIXED {
            continue;
        }
        if let Some(free) = free_space(&root)
            && best.as_ref().is_none_or(|(most, _)| free > *most)
        {
            best = Some((free, root));
        }
    }
    best.map(|(_, root)| root)
}

pub fn free_space(path: &Path) -> Option<u64> {
    // Walk up to an existing ancestor: the destination folder may not exist yet.
    let mut probe = path;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    let wide_path = wide(probe);
    let mut available: u64 = 0;
    // SAFETY: valid NUL-terminated path; `available` outlives the call; the
    // two optional out-parameters are null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide_path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(available)
}

/// (free for this user, total) bytes of the drive holding `path`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let mut probe = path;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    let wide_path = wide(probe);
    let (mut available, mut total) = (0u64, 0u64);
    // SAFETY: valid NUL-terminated path; the out-parameters outlive the call.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide_path.as_ptr(),
            &mut available,
            &mut total,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some((available, total))
}

/// Marks a file or folder as hidden (used for AeternaVault's metadata folder).
pub fn set_hidden(path: &Path) -> io::Result<()> {
    let wide_path = wide(path);
    // SAFETY: valid NUL-terminated path.
    let attributes = unsafe { GetFileAttributesW(wide_path.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: valid NUL-terminated path; attributes come from the same file.
    let ok = unsafe { SetFileAttributesW(wide_path.as_ptr(), attributes | FILE_ATTRIBUTE_HIDDEN) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Lowers CPU and I/O priority of the whole process. Used for automatic
/// backups so they do not slow down the computer.
pub fn enter_background_mode() {
    // SAFETY: the current-process handle is always valid and needs no closing.
    unsafe {
        SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN);
    }
}

/// Lowers the priority of the current thread only (automatic backups run on
/// a thread of the window's process). Returns whether it worked.
pub fn thread_background_begin() -> bool {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };
    // SAFETY: the current-thread handle is always valid.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) != 0 }
}

pub fn thread_background_end() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_END,
    };
    // SAFETY: the current-thread handle is always valid.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_END);
    }
}

/// Moves a file or folder to the recycle bin of its drive. Fails if the drive
/// has no recycle bin (network shares, some removable drives), so nothing is
/// deleted permanently behind the user's back.
pub fn recycle(path: &Path) -> io::Result<()> {
    use windows_sys::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW,
        SHFileOperationW, SHQUERYRBINFO, SHQueryRecycleBinW,
    };
    let absolute = std::path::absolute(path)?;
    let root: PathBuf = absolute.components().take(2).collect();
    let root_wide = wide(&root);
    // SAFETY: plain query with a NUL-terminated path and an initialised struct.
    let has_bin = unsafe {
        let mut info: SHQUERYRBINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
        SHQueryRecycleBinW(root_wide.as_ptr(), &mut info) >= 0
    };
    if !has_bin {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    // SHFileOperation expects a list terminated by two NUL characters.
    let mut from = wide(&absolute);
    from.push(0);
    // SAFETY: `from` is double-NUL-terminated and outlives the call; all other
    // pointers are null as documented for a delete operation.
    unsafe {
        let mut operation: SHFILEOPSTRUCTW = std::mem::zeroed();
        operation.wFunc = FO_DELETE;
        operation.pFrom = from.as_ptr();
        operation.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16;
        let result = SHFileOperationW(&mut operation);
        if result != 0 || operation.fAnyOperationsAborted != 0 {
            return Err(io::Error::other(format!(
                "moving to the recycle bin failed (code {result})"
            )));
        }
    }
    Ok(())
}

/// `true` if the computer runs on battery right now.
pub fn on_battery() -> bool {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    // SAFETY: the struct is plain data, filled by the call.
    unsafe {
        let mut status: SYSTEM_POWER_STATUS = std::mem::zeroed();
        GetSystemPowerStatus(&mut status) != 0 && status.ACLineStatus == 0
    }
}

pub fn work_area_points() -> Option<(f32, f32)> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IsProcessDPIAware, SPI_GETWORKAREA, SystemParametersInfoW,
    };
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: SPI_GETWORKAREA writes one RECT into the provided buffer.
    let ok = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut rect).cast(), 0) };
    if ok == 0 {
        return None;
    }
    let width = (rect.right - rect.left) as f32;
    let height = (rect.bottom - rect.top) as f32;
    // A DPI-unaware process already receives scaled (logical) values.
    // SAFETY: plain queries without arguments.
    let scale = unsafe {
        if IsProcessDPIAware() != 0 {
            GetDpiForSystem() as f32 / 96.0
        } else {
            1.0
        }
    };
    (width > 0.0 && height > 0.0 && scale > 0.0).then(|| (width / scale, height / scale))
}

/// Encrypts data for the current Windows user (DPAPI). Only the same user on
/// the same computer can decrypt it — used to remember the vault key for
/// automatic backups without storing the passphrase.
pub fn dpapi_protect(data: &[u8]) -> io::Result<Vec<u8>> {
    dpapi(data, true)
}

pub fn dpapi_unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    dpapi(data, false)
}

fn dpapi(data: &[u8], protect: bool) -> io::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).map_err(io::Error::other)?,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: `input` points to `data`, which outlives the call and is only read.
    // On success `output.pbData` is allocated by the system and freed with
    // LocalFree after copying.
    unsafe {
        let ok = if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as _);
        Ok(result)
    }
}

/// Whether a process with this id runs. `None` if that cannot be told.
pub fn process_running(pid: u32) -> Option<bool> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, GetLastError, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: plain process query; the handle is closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return match GetLastError() {
                ERROR_INVALID_PARAMETER => Some(false),
                ERROR_ACCESS_DENIED => Some(true),
                _ => None,
            };
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok.then_some(code == STILL_ACTIVE as u32)
    }
}
