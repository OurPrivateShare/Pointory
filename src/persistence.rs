use crate::model::AppConfig;
use std::{fs, io, path::PathBuf};

pub fn config_path() -> PathBuf {
    let base = dirs::config_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Pointory").join("tasks.json")
}

pub fn load() -> Result<AppConfig, String> {
    let path = config_path();
    if !path.exists() {
        return Ok(AppConfig::default());
    }

    let contents = fs::read_to_string(&path)
        .map_err(|error| format!("无法读取配置文件 {}：{error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("配置文件格式错误 {}：{error}", path.display()))
}

pub fn save(config: &AppConfig) -> Result<(), String> {
    let path = config_path();
    let directory = path
        .parent()
        .ok_or_else(|| "无法确定配置目录。".to_string())?;
    fs::create_dir_all(directory)
        .map_err(|error| format!("无法创建配置目录 {}：{error}", directory.display()))?;

    let contents = serde_json::to_vec_pretty(config)
        .map_err(|error| format!("无法序列化任务设置：{error}"))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, contents).map_err(|error| format!("无法写入临时配置文件：{error}"))?;

    replace_file(&temporary, &path)
        .map_err(|error| format!("无法保存配置文件 {}：{error}", path.display()))
}

fn replace_file(source: &PathBuf, destination: &PathBuf) -> io::Result<()> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(first_error) if destination.exists() => {
            fs::remove_file(destination)?;
            fs::rename(source, destination).map_err(|_| first_error)
        }
        Err(error) => Err(error),
    }
}
