use crate::persistence;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    path::Path,
};

// Keep the returned handle alive for the entire application lifetime. A separate
// lock file remains stable while settings.json is replaced atomically.
pub fn acquire() -> Result<File, String> {
    acquire_at(&persistence::config_path().with_file_name("instance.lock"))
}

fn acquire_at(path: &Path) -> Result<File, String> {
    let directory = path
        .parent()
        .ok_or_else(|| "无法确定程序锁目录。".to_string())?;
    fs::create_dir_all(directory)
        .map_err(|error| format!("无法创建程序锁目录 {}：{error}", directory.display()))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| format!("无法打开程序锁 {}：{error}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err("Pointory 已在运行，请使用已打开的窗口。".into()),
        Err(TryLockError::Error(error)) => Err(format!(
            "无法锁定程序配置目录 {}：{error}",
            directory.display()
        )),
    }
    // Do not remove instance.lock: removing a locked file would permit a second
    // process to lock a different inode on Linux. Closing the handle releases it.
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    struct TestDirectory(std::path::PathBuf);

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
    fn second_handle_is_rejected_and_closing_owner_releases_lock() {
        let directory = TestDirectory::new();
        let path = directory.0.join("instance.lock");
        let owner = acquire_at(&path).unwrap();
        assert!(acquire_at(&path).unwrap_err().contains("已在运行"));
        drop(owner);
        assert!(path.exists());
        let next_owner = acquire_at(&path).unwrap();
        assert!(acquire_at(&path).unwrap_err().contains("已在运行"));
        drop(next_owner);
        assert!(path.exists());
    }

    #[test]
    fn creates_configuration_directory_and_preserves_existing_lock_contents() {
        let directory = TestDirectory::new();
        let path = directory.0.join("Pointory").join("instance.lock");
        drop(acquire_at(&path).unwrap());
        fs::write(&path, "existing lock file").unwrap();
        drop(acquire_at(&path).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "existing lock file");
    }

    #[test]
    fn unwritable_lock_location_is_reported_without_changing_existing_file() {
        let directory = TestDirectory::new();
        let blocker = directory.0.join("file");
        fs::write(&blocker, "original").unwrap();
        assert!(acquire_at(&blocker.join("instance.lock")).is_err());
        assert_eq!(fs::read_to_string(&blocker).unwrap(), "original");
    }
}
