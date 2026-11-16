use crate::model::AppConfig;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub fn config_path() -> PathBuf {
    if let Some(directory) = std::env::var_os("POINTORY_CONFIG_DIR").filter(|path| !path.is_empty())
    {
        return PathBuf::from(directory).join("settings.json");
    }
    let base = dirs::config_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Pointory").join("settings.json")
}

pub fn load() -> Result<AppConfig, String> {
    load_from(&config_path())
}

pub fn load_from(path: &Path) -> Result<AppConfig, String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(AppConfig::default()),
        Err(error) => return Err(format!("无法读取配置文件 {}：{error}", path.display())),
    };
    let config: AppConfig = serde_json::from_str(&contents)
        .map_err(|error| format!("配置文件格式错误 {}：{error}", path.display()))?;
    config
        .validate()
        .map_err(|error| format!("配置文件无效 {}：{error}", path.display()))?;
    Ok(config)
}

pub fn save(config: &AppConfig) -> Result<(), String> {
    save_to(&config_path(), config)
}

pub fn save_to(path: &Path, config: &AppConfig) -> Result<(), String> {
    config.validate()?;
    // Never replace an unreadable, damaged, or unsupported configuration with defaults.
    load_from(path).map_err(|error| format!("为保留原始配置，已取消保存。{error}"))?;
    let directory = path
        .parent()
        .ok_or_else(|| "无法确定配置目录。".to_string())?;
    fs::create_dir_all(directory)
        .map_err(|error| format!("无法创建配置目录 {}：{error}", directory.display()))?;

    let contents = serde_json::to_vec_pretty(config)
        .map_err(|error| format!("无法序列化任务设置：{error}"))?;
    let filename = path
        .file_name()
        .ok_or_else(|| "配置文件名无效。".to_string())?
        .to_string_lossy();
    let temporary = directory.join(format!(".{filename}.{}.tmp", Uuid::new_v4()));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&contents)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("无法保存配置文件 {}：{error}", path.display()))
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // The source is on the same volume. Windows replaces the destination in one operation.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Task;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("pointory-test-{}", Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn config_path(&self) -> PathBuf {
            self.0.join("settings.json")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_config_returns_only_default_clicker_without_creating_file() {
        let directory = TestDirectory::new();
        let path = directory.config_path();
        let config = load_from(&path).unwrap();
        assert_eq!(config.tasks.len(), 1);
        assert!(config.tasks[0].is_builtin());
        assert_eq!(config.tasks[0].hotkey, "F1");
        assert!(!path.exists());
    }

    #[test]
    fn task_changes_survive_atomic_replacement() {
        let directory = TestDirectory::new();
        let path = directory.config_path();
        let mut config = AppConfig::default();
        save_to(&path, &config).unwrap();
        config.tasks[0].min_interval_ms = 250;
        config.tasks[0].hotkey = "F3".into();
        config.tasks.push(Task::new(1));
        save_to(&path, &config).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.tasks.len(), 2);
        assert_eq!(loaded.tasks[0].min_interval_ms, 250);
        assert_eq!(loaded.tasks[0].hotkey, "F3");
        assert_eq!(loaded.tasks[1].id, config.tasks[1].id);
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn damaged_and_legacy_files_are_rejected_and_never_overwritten() {
        let directory = TestDirectory::new();
        let path = directory.config_path();
        for contents in [
            "not json",
            r#"{"tasks":[]}"#,
            r#"{"schema_version":1,"tasks":[]}"#,
        ] {
            fs::write(&path, contents).unwrap();
            assert!(load_from(&path).is_err());
            assert!(save_to(&path, &AppConfig::default()).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), contents);
            assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        }
    }

    #[test]
    fn invalid_config_does_not_change_existing_file() {
        let directory = TestDirectory::new();
        let path = directory.config_path();
        let mut config = AppConfig::default();
        save_to(&path, &config).unwrap();
        let before = fs::read(&path).unwrap();
        config.tasks.clear();
        assert!(save_to(&path, &config).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn replacement_failure_keeps_destination() {
        let directory = TestDirectory::new();
        let path = directory.config_path();
        fs::write(&path, "original").unwrap();
        assert!(replace_file(&directory.0.join("missing.tmp"), &path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    }

    #[test]
    fn old_tasks_json_is_not_loaded() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("tasks.json"), r#"{"tasks":[]}"#).unwrap();
        assert_eq!(load_from(&directory.config_path()).unwrap().tasks.len(), 1);
    }
}
