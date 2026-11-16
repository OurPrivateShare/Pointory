use crate::model::AppConfig;
use global_hotkey::{GlobalHotKeyManager, hotkey::HotKey};
use std::collections::BTreeMap;

pub const STOP_ALL: &str = "CTRL+ALT+ESC";
pub const PICK: &str = "F8";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    ToggleTask(String),
    Pick,
    StopAll,
}

#[derive(Clone)]
struct Binding {
    hotkey: HotKey,
    action: Action,
    label: String,
}

type Bindings = BTreeMap<u32, Binding>;

pub fn validate(config: &AppConfig) -> Result<(), String> {
    bindings(config).map(|_| ())
}

fn parse(value: &str) -> Result<HotKey, String> {
    value
        .trim()
        .parse::<HotKey>()
        .map_err(|_| format!("快捷键“{value}”无效，请使用 F1 或 CTRL+ALT+F6 等格式。"))
}

fn safety_bindings() -> Bindings {
    let mut result = BTreeMap::new();
    for (label, action) in [(STOP_ALL, Action::StopAll), (PICK, Action::Pick)] {
        let hotkey = parse(label).expect("built-in shortcut is valid");
        result.insert(
            hotkey.id(),
            Binding {
                hotkey,
                action,
                label: if label == STOP_ALL {
                    "全部停止".into()
                } else {
                    "取点 / 吸色".into()
                },
            },
        );
    }
    result
}

fn bindings(config: &AppConfig) -> Result<Bindings, String> {
    config.validate()?;
    let mut result = safety_bindings();
    for task in &config.tasks {
        if task.hotkey.trim().is_empty() {
            continue;
        }
        let hotkey = parse(&task.hotkey)?;
        if let Some(existing) = result.get(&hotkey.id()) {
            return Err(format!(
                "快捷键“{}”与“{}”冲突，请为“{}”选择其他快捷键。",
                task.hotkey, existing.label, task.name
            ));
        }
        result.insert(
            hotkey.id(),
            Binding {
                hotkey,
                action: Action::ToggleTask(task.id.clone()),
                label: task.name.clone(),
            },
        );
    }
    Ok(result)
}

fn startup_bindings(config: &AppConfig) -> (Bindings, String) {
    match bindings(config) {
        Ok(requested) => (requested, String::new()),
        Err(error) => (safety_bindings(), error),
    }
}

trait Registrar {
    fn register(&self, hotkey: HotKey) -> Result<(), String>;
    fn unregister(&self, hotkey: HotKey) -> Result<(), String>;
}

impl Registrar for GlobalHotKeyManager {
    fn register(&self, hotkey: HotKey) -> Result<(), String> {
        GlobalHotKeyManager::register(self, hotkey).map_err(|error| error.to_string())
    }

    fn unregister(&self, hotkey: HotKey) -> Result<(), String> {
        GlobalHotKeyManager::unregister(self, hotkey).map_err(|error| error.to_string())
    }
}

#[derive(Default)]
struct RegistrationState {
    requested: Bindings,
    active: BTreeMap<u32, HotKey>,
    failures: BTreeMap<u32, String>,
}

impl RegistrationState {
    fn start(registrar: &impl Registrar, requested: Bindings) -> Self {
        let mut state = Self {
            requested,
            ..Self::default()
        };
        for (&id, binding) in &state.requested {
            match registrar.register(binding.hotkey) {
                Ok(()) => {
                    state.active.insert(id, binding.hotkey);
                }
                Err(error) => {
                    state.failures.insert(id, error);
                }
            }
        }
        state
    }

    fn action(&self, id: u32) -> Option<Action> {
        self.active.get(&id)?;
        self.requested
            .get(&id)
            .map(|binding| binding.action.clone())
    }

    fn align_actions(&mut self, requested: Bindings) {
        // Dispatch follows saved configuration even if the OS could not roll back registrations.
        // Do not add entries to `active`: a missing old shortcut remains unavailable.
        self.requested = requested;
        self.failures
            .retain(|id, _| self.requested.contains_key(id));
    }

    fn warning(&self) -> String {
        let mut warnings = Vec::new();
        for (&id, binding) in &self.requested {
            if !self.active.contains_key(&id) {
                let error = self
                    .failures
                    .get(&id)
                    .map(String::as_str)
                    .unwrap_or("注册失败");
                warnings.push(format!(
                    "“{}”的快捷键 {} 不可用：{error}",
                    binding.label, binding.hotkey
                ));
            }
        }
        for (&id, hotkey) in &self.active {
            if !self.requested.contains_key(&id) {
                warnings.push(format!("快捷键 {hotkey} 未能释放，暂不响应。"));
            }
        }
        warnings.join("；")
    }

    fn replace(&mut self, registrar: &impl Registrar, next: Bindings) -> Result<(), String> {
        // Keep unavailable but unchanged shortcuts as warnings. Editing an interval must still
        // work if another application held F1 when Pointory started.
        let additions = next
            .iter()
            .filter(|(id, _)| !self.active.contains_key(*id) && !self.requested.contains_key(*id))
            .map(|(&id, binding)| (id, binding.clone()))
            .collect::<Vec<_>>();
        let removals = self
            .active
            .iter()
            .filter(|(id, _)| !next.contains_key(*id))
            .map(|(&id, &hotkey)| (id, hotkey))
            .collect::<Vec<_>>();
        let mut added = Vec::new();
        for (id, binding) in additions {
            if let Err(error) = registrar.register(binding.hotkey) {
                let mut errors = vec![format!(
                    "无法注册“{}”的快捷键 {}：{error}",
                    binding.label, binding.hotkey
                )];
                self.rollback_additions(registrar, &added, &mut errors);
                return Err(errors.join("；"));
            }
            self.active.insert(id, binding.hotkey);
            added.push((id, binding.hotkey));
        }

        let mut removed = Vec::new();
        for (id, hotkey) in removals {
            if let Err(error) = registrar.unregister(hotkey) {
                let mut errors = vec![format!("无法注销旧快捷键 {hotkey}：{error}")];
                // Restore the old registrations before abandoning the new configuration.
                for &(old_id, old_hotkey) in &removed {
                    match registrar.register(old_hotkey) {
                        Ok(()) => {
                            self.active.insert(old_id, old_hotkey);
                        }
                        Err(error) => {
                            self.failures.insert(old_id, error.clone());
                            errors.push(format!("恢复快捷键 {old_hotkey} 失败：{error}"));
                        }
                    }
                }
                self.rollback_additions(registrar, &added, &mut errors);
                return Err(errors.join("；"));
            }
            self.active.remove(&id);
            removed.push((id, hotkey));
        }
        self.requested = next;
        self.failures
            .retain(|id, _| self.requested.contains_key(id) && !self.active.contains_key(id));
        Ok(())
    }

    fn rollback_additions(
        &mut self,
        registrar: &impl Registrar,
        added: &[(u32, HotKey)],
        errors: &mut Vec<String>,
    ) {
        for &(id, hotkey) in added.iter().rev() {
            match registrar.unregister(hotkey) {
                Ok(()) => {
                    self.active.remove(&id);
                }
                Err(error) => errors.push(format!("撤销新增快捷键 {hotkey} 失败：{error}")),
            }
        }
    }
}

pub struct Hotkeys {
    manager: Option<GlobalHotKeyManager>,
    state: RegistrationState,
    unavailable: String,
    config_warning: String,
    pub warning: String,
}

impl Hotkeys {
    pub fn new(config: &AppConfig) -> Self {
        let manager = GlobalHotKeyManager::new();
        let (requested, config_warning) = startup_bindings(config);
        let mut hotkeys = match manager {
            Ok(manager) => {
                let state = RegistrationState::start(&manager, requested);
                let warning = state.warning();
                Self {
                    manager: Some(manager),
                    state,
                    unavailable: String::new(),
                    config_warning,
                    warning,
                }
            }
            Err(error) => {
                let warning = format!("全局快捷键不可用：{error}。可使用列表按钮运行和停止。");
                Self {
                    manager: None,
                    state: RegistrationState {
                        requested,
                        ..RegistrationState::default()
                    },
                    unavailable: warning.clone(),
                    config_warning,
                    warning,
                }
            }
        };
        hotkeys.refresh_warning();
        hotkeys
    }

    pub fn replace(&mut self, config: &AppConfig) -> Result<(), String> {
        let next = bindings(config)?;
        let result = if let Some(manager) = self.manager.as_ref() {
            self.state.replace(manager, next)
        } else {
            self.state.requested = next;
            Ok(())
        };
        if result.is_ok() {
            self.config_warning.clear();
        }
        self.refresh_warning();
        result
    }

    pub fn align_actions(&mut self, config: &AppConfig) {
        let (requested, config_warning) = startup_bindings(config);
        self.state.align_actions(requested);
        self.config_warning = config_warning;
        self.refresh_warning();
    }

    fn refresh_warning(&mut self) {
        let registration_warning = if self.manager.is_some() {
            self.state.warning()
        } else {
            self.unavailable.clone()
        };
        self.warning = [self.config_warning.as_str(), registration_warning.as_str()]
            .into_iter()
            .filter(|warning| !warning.is_empty())
            .collect::<Vec<_>>()
            .join("；");
    }

    pub fn action(&self, id: u32) -> Option<Action> {
        self.state.action(id)
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        if let Some(manager) = &self.manager {
            for &hotkey in self.state.active.values() {
                let _ = manager.unregister(hotkey);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Task;
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    #[derive(Default)]
    struct FakeRegistrar {
        active: RefCell<BTreeSet<u32>>,
        fail_register: RefCell<BTreeSet<u32>>,
        fail_unregister: RefCell<BTreeSet<u32>>,
    }

    impl Registrar for FakeRegistrar {
        fn register(&self, hotkey: HotKey) -> Result<(), String> {
            if self.fail_register.borrow().contains(&hotkey.id()) {
                return Err("被占用".into());
            }
            if !self.active.borrow_mut().insert(hotkey.id()) {
                return Err("已注册".into());
            }
            Ok(())
        }
        fn unregister(&self, hotkey: HotKey) -> Result<(), String> {
            if self.fail_unregister.borrow().contains(&hotkey.id()) {
                return Err("注销失败".into());
            }
            self.active.borrow_mut().remove(&hotkey.id());
            Ok(())
        }
    }

    fn config_with_hotkeys(values: &[&str]) -> AppConfig {
        let mut config = AppConfig::default();
        for (index, value) in values.iter().enumerate() {
            let mut task = Task::new(index + 1);
            task.hotkey = (*value).into();
            config.tasks.push(task);
        }
        config
    }

    #[test]
    fn aliases_and_modifier_order_are_duplicate_shortcuts() {
        let config = config_with_hotkeys(&["CTRL+ALT+F6", "Alt + Control + F6"]);
        assert!(validate(&config).unwrap_err().contains("冲突"));
    }

    #[test]
    fn reserved_shortcuts_cannot_be_assigned_to_tasks() {
        for value in ["f8", "Alt+Control+Escape"] {
            assert!(
                validate(&config_with_hotkeys(&[value]))
                    .unwrap_err()
                    .contains("冲突")
            );
        }
    }

    #[test]
    fn empty_hotkeys_are_allowed_and_bad_syntax_is_rejected() {
        validate(&config_with_hotkeys(&["", "   ", "Ctrl+Alt+F6"])).unwrap();
        for value in ["CTRL", "CTRL++F6", "F6+CTRL", "不是快捷键"] {
            assert!(validate(&config_with_hotkeys(&[value])).is_err());
        }
    }

    #[test]
    fn failed_addition_preserves_old_keys_and_rolls_back_other_additions() {
        let registrar = FakeRegistrar::default();
        let old = AppConfig::default();
        let mut state = RegistrationState::start(&registrar, bindings(&old).unwrap());
        let before = registrar.active.borrow().clone();
        let next = config_with_hotkeys(&["F2", "F3"]);
        registrar
            .fail_register
            .borrow_mut()
            .insert(parse("F3").unwrap().id());
        assert!(state.replace(&registrar, bindings(&next).unwrap()).is_err());
        assert_eq!(*registrar.active.borrow(), before);
        assert_eq!(
            state.action(parse("F1").unwrap().id()),
            Some(Action::ToggleTask(old.tasks[0].id.clone()))
        );
        assert_eq!(state.action(parse("F2").unwrap().id()), None);
    }

    #[test]
    fn failed_removal_restores_previously_removed_keys() {
        let registrar = FakeRegistrar::default();
        let old = config_with_hotkeys(&["F2", "F3"]);
        let mut state = RegistrationState::start(&registrar, bindings(&old).unwrap());
        let before = registrar.active.borrow().clone();
        let mut next = AppConfig::default();
        next.tasks[0].hotkey = "F4".into();
        registrar
            .fail_unregister
            .borrow_mut()
            .insert(parse("F3").unwrap().id());
        assert!(state.replace(&registrar, bindings(&next).unwrap()).is_err());
        assert_eq!(*registrar.active.borrow(), before);
        assert_eq!(state.action(parse("F4").unwrap().id()), None);
        assert!(state.action(parse("F1").unwrap().id()).is_some());
    }

    #[test]
    fn swapping_shortcuts_reuses_registrations_and_changes_actions() {
        let registrar = FakeRegistrar::default();
        let old = config_with_hotkeys(&["F2"]);
        let mut state = RegistrationState::start(&registrar, bindings(&old).unwrap());
        let before = registrar.active.borrow().clone();
        let mut next = old.clone();
        next.tasks[0].hotkey = "F2".into();
        next.tasks[1].hotkey = "F1".into();
        state.replace(&registrar, bindings(&next).unwrap()).unwrap();
        assert_eq!(*registrar.active.borrow(), before);
        assert_eq!(
            state.action(parse("F1").unwrap().id()),
            Some(Action::ToggleTask(old.tasks[1].id.clone()))
        );
    }

    #[test]
    fn unavailable_unchanged_shortcut_does_not_block_interval_changes() {
        let registrar = FakeRegistrar::default();
        registrar
            .fail_register
            .borrow_mut()
            .insert(parse("F1").unwrap().id());
        let mut config = AppConfig::default();
        let mut state = RegistrationState::start(&registrar, bindings(&config).unwrap());
        assert!(state.warning().contains("不可用"));
        config.tasks[0].min_interval_ms = 200;
        state
            .replace(&registrar, bindings(&config).unwrap())
            .unwrap();
        assert_eq!(state.action(parse("F1").unwrap().id()), None);
        assert!(state.action(parse(STOP_ALL).unwrap().id()).is_some());
    }

    #[test]
    fn rollback_failure_is_reported_and_unsaved_shortcut_does_not_dispatch() {
        let registrar = FakeRegistrar::default();
        let mut state =
            RegistrationState::start(&registrar, bindings(&AppConfig::default()).unwrap());
        let next = config_with_hotkeys(&["F2", "F3"]);
        registrar
            .fail_register
            .borrow_mut()
            .insert(parse("F3").unwrap().id());
        registrar
            .fail_unregister
            .borrow_mut()
            .insert(parse("F2").unwrap().id());
        let error = state
            .replace(&registrar, bindings(&next).unwrap())
            .unwrap_err();
        assert!(error.contains("撤销新增"));
        assert!(state.warning().contains("未能释放"));
        assert_eq!(state.action(parse("F2").unwrap().id()), None);
    }

    #[test]
    fn invalid_startup_keeps_safety_keys_and_accepts_corrected_task_shortcuts() {
        let registrar = FakeRegistrar::default();
        let mut config = AppConfig::default();
        config.tasks[0].hotkey = "invalid shortcut".into();
        let (requested, warning) = startup_bindings(&config);
        assert!(warning.contains("无效"));
        let mut state = RegistrationState::start(&registrar, requested);
        assert_eq!(
            state.action(parse(STOP_ALL).unwrap().id()),
            Some(Action::StopAll)
        );
        assert_eq!(state.action(parse(PICK).unwrap().id()), Some(Action::Pick));
        assert_eq!(state.action(parse("F1").unwrap().id()), None);

        config.tasks[0].hotkey = "F1".into();
        state
            .replace(&registrar, bindings(&config).unwrap())
            .unwrap();
        assert_eq!(
            state.action(parse("F1").unwrap().id()),
            Some(Action::ToggleTask(config.tasks[0].id.clone()))
        );
        assert!(state.warning().is_empty());
    }

    #[test]
    fn failed_disk_rollback_dispatches_only_saved_configuration_after_alignment() {
        let registrar = FakeRegistrar::default();
        let saved = AppConfig::default();
        let mut state = RegistrationState::start(&registrar, bindings(&saved).unwrap());
        let mut next = saved.clone();
        next.tasks[0].hotkey = "F2".into();
        state.replace(&registrar, bindings(&next).unwrap()).unwrap();
        assert!(state.action(parse("F2").unwrap().id()).is_some());

        // The settings write failed, then the OS refused to register the old F1 again.
        registrar
            .fail_register
            .borrow_mut()
            .insert(parse("F1").unwrap().id());
        assert!(
            state
                .replace(&registrar, bindings(&saved).unwrap())
                .is_err()
        );
        state.align_actions(bindings(&saved).unwrap());
        assert_eq!(state.action(parse("F2").unwrap().id()), None);
        assert_eq!(state.action(parse("F1").unwrap().id()), None);
        assert_eq!(
            state.action(parse(STOP_ALL).unwrap().id()),
            Some(Action::StopAll)
        );
        assert!(state.warning().contains("F1"));
        assert!(state.warning().contains("不可用"));
        assert!(
            registrar
                .active
                .borrow()
                .contains(&parse("F2").unwrap().id())
        );
    }
}
