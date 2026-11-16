use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

pub const MIN_INTERVAL_FLOOR_MS: u32 = 20;
pub const MAX_INTERVAL_CEILING_MS: u32 = 86_400_000;
pub const MAX_CLICK_LIMIT: u64 = 1_000_000;
pub const CONFIG_SCHEMA_VERSION: u32 = 3;
const CLICKER_ID: &str = "builtin-clicker";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RgbColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl RgbColor {
    pub fn matches(self, other: Self, tolerance: u8) -> bool {
        self.red.abs_diff(other.red) <= tolerance
            && self.green.abs_diff(other.green) <= tolerance
            && self.blue.abs_diff(other.blue) <= tolerance
    }

    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.red, self.green, self.blue)
    }

    pub fn from_hex(value: &str) -> Option<Self> {
        let value = value.trim().strip_prefix('#').unwrap_or(value.trim());
        if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }

        Some(Self {
            red: u8::from_str_radix(&value[0..2], 16).ok()?,
            green: u8::from_str_radix(&value[2..4], 16).ok()?,
            blue: u8::from_str_radix(&value[4..6], 16).ok()?,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

impl MouseButton {
    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "左键",
            Self::Right => "右键",
            Self::Middle => "中键",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Middle,
            Self::Middle => Self::Left,
        }
    }

    pub fn from_label(value: &str) -> Self {
        match value {
            "右键" => Self::Right,
            "中键" => Self::Middle,
            _ => Self::Left,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Clicker,
    Custom,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub kind: TaskKind,
    pub name: String,
    pub position: Option<Position>,
    pub color: Option<RgbColor>,
    pub color_tolerance: u8,
    pub min_interval_ms: u32,
    pub max_interval_ms: u32,
    pub hotkey: String,
    pub include_in_batch: bool,
    pub mouse_button: MouseButton,
    pub max_clicks: u64,
}

impl Task {
    pub fn new(number: usize) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            kind: TaskKind::Custom,
            name: format!("任务 {number}"),
            position: None,
            color: None,
            color_tolerance: 12,
            min_interval_ms: 1_000,
            max_interval_ms: 0,
            hotkey: String::new(),
            include_in_batch: false,
            mouse_button: MouseButton::Left,
            max_clicks: 0,
        }
    }

    pub fn clicker() -> Self {
        Self {
            id: CLICKER_ID.into(),
            kind: TaskKind::Clicker,
            name: "鼠标连点器".into(),
            hotkey: "F1".into(),
            ..Self::new(1)
        }
    }

    pub fn is_builtin(&self) -> bool {
        self.kind == TaskKind::Clicker
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.id.trim() != self.id {
            return Err("任务标识无效。".into());
        }
        if self.name.trim().is_empty() {
            return Err("请为任务填写名称。".into());
        }
        if !self.is_builtin() && self.position.is_none() {
            return Err("请设置点击位置，可使用位置取点或填写 X、Y 坐标。".into());
        }
        if self.min_interval_ms < MIN_INTERVAL_FLOOR_MS {
            return Err(format!("最短间隔不能小于 {MIN_INTERVAL_FLOOR_MS} 毫秒。"));
        }
        if self.min_interval_ms > MAX_INTERVAL_CEILING_MS
            || self.max_interval_ms > MAX_INTERVAL_CEILING_MS
        {
            return Err("间隔不能超过 24 小时。".into());
        }
        if self.max_interval_ms != 0 && self.max_interval_ms < self.min_interval_ms {
            return Err("最长间隔必须大于或等于最短间隔。".into());
        }
        if self.max_clicks > MAX_CLICK_LIMIT {
            return Err(format!("点击次数不能超过 {MAX_CLICK_LIMIT}。"));
        }
        if self.is_builtin() {
            if self.id != CLICKER_ID
                || self.name != "鼠标连点器"
                || self.position.is_some()
                || self.color.is_some()
                || self.color_tolerance != 12
                || self.max_interval_ms != 0
                || self.include_in_batch
                || self.mouse_button != MouseButton::Left
                || self.max_clicks != 0
            {
                return Err("鼠标连点器只能修改点击间隔和快捷键。".into());
            }
        } else if self.id == CLICKER_ID {
            return Err("自定义任务不能使用内置任务标识。".into());
        }
        Ok(())
    }

    pub fn target(&self) -> Option<(i32, i32)> {
        self.position.map(|position| (position.x, position.y))
    }

    pub fn random_interval_ms<R: Rng + ?Sized>(&self, rng: &mut R) -> u32 {
        if self.max_interval_ms == 0 || self.max_interval_ms == self.min_interval_ms {
            self.min_interval_ms
        } else {
            rng.gen_range(self.min_interval_ms..=self.max_interval_ms)
        }
    }
}

impl Default for Task {
    fn default() -> Self {
        Self::new(1)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub schema_version: u32,
    pub tasks: Vec<Task>,
}

impl AppConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(format!("不支持的配置版本：{}。", self.schema_version));
        }
        if !self.tasks.first().is_some_and(Task::is_builtin)
            || self.tasks.iter().filter(|task| task.is_builtin()).count() != 1
        {
            return Err("任务列表必须以唯一的鼠标连点器任务开始。".into());
        }
        let mut ids = HashSet::new();
        for task in &self.tasks {
            task.validate()?;
            if !ids.insert(&task.id) {
                return Err("存在重复的任务标识。".into());
            }
        }
        Ok(())
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            tasks: vec![Task::clicker()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_task_uses_requested_defaults() {
        let task = Task::new(1);
        assert_eq!(task.min_interval_ms, 1_000);
        assert_eq!(task.max_interval_ms, 0);
        assert!(task.color.is_none());
        assert!(task.target().is_none());
        assert!(!task.include_in_batch);
        assert_eq!(task.max_clicks, 0);
        assert_eq!(task.kind, TaskKind::Custom);
        assert!(task.validate().unwrap_err().contains("请设置点击位置"));
    }

    #[test]
    fn color_tolerance_is_applied_per_channel() {
        let expected = RgbColor {
            red: 100,
            green: 150,
            blue: 200,
        };
        assert!(expected.matches(
            RgbColor {
                red: 108,
                green: 143,
                blue: 208
            },
            8
        ));
        assert!(!expected.matches(
            RgbColor {
                red: 109,
                green: 150,
                blue: 200
            },
            8
        ));
    }

    #[test]
    fn interval_bounds_are_validated_and_randomized_inclusively() {
        let mut task = Task::new(1);
        task.position = Some(Position { x: 100, y: 200 });
        task.min_interval_ms = 200;
        task.max_interval_ms = 450;
        task.validate().unwrap();

        let mut rng = rand::thread_rng();
        for _ in 0..100 {
            let interval = task.random_interval_ms(&mut rng);
            assert!((200..=450).contains(&interval));
        }

        task.max_interval_ms = 199;
        assert!(task.validate().is_err());
    }

    #[test]
    fn hex_color_round_trips() {
        let color = RgbColor {
            red: 16,
            green: 170,
            blue: 255,
        };
        assert_eq!(RgbColor::from_hex(&color.to_hex()), Some(color));
        assert_eq!(RgbColor::from_hex("nope"), None);
        assert_eq!(RgbColor::from_hex("中123"), None);
        assert_eq!(RgbColor::from_hex("00中1"), None);
        assert_eq!(RgbColor::from_hex("é1234"), None);
        assert_eq!(RgbColor::from_hex("+1+2+3"), None);
        assert_eq!(
            RgbColor::from_hex("#aAbBcC"),
            Some(RgbColor {
                red: 170,
                green: 187,
                blue: 204
            })
        );
    }

    #[test]
    fn fresh_config_has_one_fixed_clicker() {
        let config = AppConfig::default();
        config.validate().unwrap();
        assert_eq!(config.tasks.len(), 1);
        let task = &config.tasks[0];
        assert!(task.is_builtin());
        assert_eq!(task.id, CLICKER_ID);
        assert_eq!(task.name, "鼠标连点器");
        assert_eq!(task.hotkey, "F1");
        assert_eq!(task.min_interval_ms, 1000);
        assert_eq!(task.mouse_button, MouseButton::Left);
        assert!(task.target().is_none());
        assert_eq!(task.max_clicks, 0);
    }

    #[test]
    fn clicker_only_allows_interval_and_hotkey_changes() {
        let mut task = Task::clicker();
        task.min_interval_ms = 250;
        task.hotkey = "F2".into();
        task.validate().unwrap();
        task.mouse_button = MouseButton::Right;
        assert!(task.validate().is_err());
        task.mouse_button = MouseButton::Left;
        task.max_interval_ms = 500;
        assert!(task.validate().is_err());
        task.max_interval_ms = 0;
        task.name = "改名".into();
        assert!(task.validate().is_err());
    }

    #[test]
    fn invalid_schema_order_and_duplicate_ids_are_rejected() {
        let mut config = AppConfig {
            schema_version: 1,
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
        config.schema_version = CONFIG_SCHEMA_VERSION;
        let mut custom = Task::new(1);
        custom.position = Some(Position { x: 10, y: 20 });
        config.tasks.insert(0, custom);
        assert!(config.validate().is_err());
        config.tasks.swap(0, 1);
        config.validate().unwrap();
        config.tasks.push(config.tasks[1].clone());
        assert!(config.validate().is_err());
        config.tasks.pop();
        config.tasks.push(Task::clicker());
        assert!(config.validate().is_err());
    }

    #[test]
    fn custom_position_is_required_and_invalid_limits_and_reserved_id_are_rejected() {
        let mut task = Task::new(1);
        assert!(task.validate().is_err());
        task.position = Some(Position { x: -5, y: 10 });
        task.validate().unwrap();
        assert_eq!(task.target(), Some((-5, 10)));
        task.min_interval_ms = MIN_INTERVAL_FLOOR_MS - 1;
        assert!(task.validate().is_err());
        task.min_interval_ms = MAX_INTERVAL_CEILING_MS + 1;
        assert!(task.validate().is_err());
        task.min_interval_ms = 1000;
        task.max_clicks = MAX_CLICK_LIMIT + 1;
        assert!(task.validate().is_err());
        task.max_clicks = 0;
        task.id = CLICKER_ID.into();
        assert!(task.validate().is_err());
    }

    #[test]
    fn missing_and_unknown_configuration_fields_are_rejected() {
        assert!(serde_json::from_str::<AppConfig>(r#"{"tasks":[]}"#).is_err());
        let mut value = serde_json::to_value(AppConfig::default()).unwrap();
        value["unsupported"] = serde_json::json!(true);
        assert!(serde_json::from_value::<AppConfig>(value).is_err());
    }

    #[test]
    fn saved_custom_tasks_require_complete_fixed_positions() {
        let mut config = AppConfig::default();
        config.tasks.push(Task::new(1));
        assert!(config.validate().is_err());
        config.tasks[1].position = Some(Position { x: -1920, y: 721 });
        config.validate().unwrap();
        let serialized = serde_json::to_value(&config).unwrap();
        assert_eq!(serialized["tasks"][1]["position"]["x"], -1920);
        let loaded: AppConfig = serde_json::from_value(serialized).unwrap();
        loaded.validate().unwrap();
        assert_eq!(loaded.tasks[1].target(), Some((-1920, 721)));
        config.tasks[0].position = Some(Position { x: 0, y: 0 });
        assert!(config.validate().is_err());
    }
}
