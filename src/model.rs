use rand::Rng;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MIN_INTERVAL_FLOOR_MS: u32 = 20;
pub const MAX_INTERVAL_CEILING_MS: u32 = 86_400_000;
pub const MAX_CLICK_LIMIT: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
        if value.len() != 6 {
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
            "右键" | "Right" => Self::Right,
            "中键" | "Middle" => Self::Middle,
            _ => Self::Left,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub x: Option<i32>,
    pub y: Option<i32>,
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
            name: format!("任务 {number}"),
            x: None,
            y: None,
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

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("请为任务填写名称。".into());
        }
        if self.x.is_some() != self.y.is_some() {
            return Err("位置坐标不完整，请重新取点。".into());
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
        Ok(())
    }

    pub fn target(&self) -> Option<(i32, i32)> {
        self.x.zip(self.y)
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AppConfig {
    pub tasks: Vec<Task>,
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
    }
}
