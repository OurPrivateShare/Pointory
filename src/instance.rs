use crate::persistence;
use std::path::Path;

#[cfg(windows)]
#[derive(Debug)]
pub struct InstanceLock {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for InstanceLock {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

#[cfg(not(windows))]
#[derive(Debug)]
pub struct InstanceLock {
    _file: std::fs::File,
}

// Hold this guard for the application lifetime. On Windows a kernel object
// avoids filesystem locking calls that can stall on virtual or portable drives.
pub fn acquire() -> Result<InstanceLock, String> {
    let configuration = persistence::config_path()?;
    let directory = configuration
        .parent()
        .ok_or_else(|| "无法确定程序所在目录。".to_string())?;
    acquire_at(directory)
}

#[cfg(windows)]
fn mutex_name(directory: &Path) -> Result<Vec<u16>, String> {
    let directory = std::fs::canonicalize(directory)
        .map_err(|error| format!("无法定位程序目录 {}：{error}", directory.display()))?;
    if !directory.is_dir() {
        return Err(format!("程序目录无效：{}。", directory.display()));
    }
    // Canonicalization merges path aliases; case folding matches ordinary
    // Windows directory lookup. FNV-1a gives a stable, compact object name.
    let normalized = directory.as_os_str().to_string_lossy().to_lowercase();
    let hash = normalized
        .bytes()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    Ok(format!("Local\\Pointory-{hash:016x}")
        .encode_utf16()
        .chain(Some(0))
        .collect())
}

#[cfg(windows)]
fn acquire_at(directory: &Path) -> Result<InstanceLock, String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, SetLastError},
        System::Threading::CreateMutexW,
    };

    let name = mutex_name(directory)?;
    // Ownership is unnecessary: the name remains reserved while its handle is
    // alive, and the OS removes it after all handles close, including on crashes.
    let (handle, status) = unsafe {
        SetLastError(0);
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        (handle, GetLastError())
    };
    if handle.is_null() {
        return Err(format!(
            "无法创建程序实例锁：{}",
            std::io::Error::from_raw_os_error(status as i32)
        ));
    }
    if status == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(handle) };
        return Err("Pointory 已在运行，请使用已打开的窗口。".into());
    }
    Ok(InstanceLock { handle })
}

#[cfg(not(windows))]
fn acquire_at(directory: &Path) -> Result<InstanceLock, String> {
    use std::{fs, fs::OpenOptions, fs::TryLockError};

    fs::create_dir_all(directory)
        .map_err(|error| format!("无法创建程序锁目录 {}：{error}", directory.display()))?;
    let path = directory.join("instance.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("无法打开程序锁 {}：{error}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(InstanceLock { _file: file }),
        Err(TryLockError::WouldBlock) => Err("Pointory 已在运行，请使用已打开的窗口。".into()),
        Err(TryLockError::Error(error)) => {
            Err(format!("无法锁定程序目录 {}：{error}", directory.display()))
        }
    }
    // Keep instance.lock stable on Linux: removing a locked inode would let a
    // second process acquire a different file. Closing the guard releases it.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};
    use uuid::Uuid;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("pointory-lock-test-{}", Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn same_directory_is_rejected_and_closing_owner_releases_lock() {
        let directory = TestDirectory::new();
        let owner = acquire_at(&directory.0).unwrap();
        assert!(acquire_at(&directory.0).unwrap_err().contains("已在运行"));
        drop(owner);
        let next_owner = acquire_at(&directory.0).unwrap();
        assert!(acquire_at(&directory.0).unwrap_err().contains("已在运行"));
        drop(next_owner);
    }

    #[test]
    fn different_software_directories_can_run_independently() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        let _first_owner = acquire_at(&first.0).unwrap();
        let _second_owner = acquire_at(&second.0).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_mutex_uses_directory_identity_without_creating_a_lock_file() {
        let directory = TestDirectory::new();
        let _owner = acquire_at(&directory.0).unwrap();
        assert!(acquire_at(&directory.0.join(".")).is_err());
        let uppercase = PathBuf::from(directory.0.to_string_lossy().to_uppercase());
        assert_eq!(
            mutex_name(&directory.0).unwrap(),
            mutex_name(&uppercase).unwrap()
        );
        assert!(acquire_at(&uppercase).unwrap_err().contains("已在运行"));
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
    }

    #[cfg(not(windows))]
    #[test]
    fn file_lock_is_retained_without_truncating_existing_contents() {
        let directory = TestDirectory::new();
        let nested = directory.0.join("Pointory");
        drop(acquire_at(&nested).unwrap());
        let path = nested.join("instance.lock");
        fs::write(&path, "existing lock file").unwrap();
        drop(acquire_at(&nested).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "existing lock file");
    }

    #[test]
    fn invalid_lock_directory_is_reported_without_changing_existing_file() {
        let directory = TestDirectory::new();
        let blocker = directory.0.join("file");
        fs::write(&blocker, "original").unwrap();
        assert!(acquire_at(&blocker).is_err());
        assert_eq!(fs::read_to_string(&blocker).unwrap(), "original");
    }
}
