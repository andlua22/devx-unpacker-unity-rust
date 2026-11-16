use crate::types::{AssetRef, AssetSummary};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
}

/// Host services exposed to plugins, mirroring `DevXCControl` APIs
/// (`warning`, `error`, `FindPPtr`, `FindAssets`, `EnumByAssetType`, variables, status).
pub trait PluginHostContext {
    fn log(&mut self, level: LogLevel, message: String);
    fn set_status(&mut self, status: String);

    fn log_info(&mut self, message: &str) {
        self.log(LogLevel::Info, message.to_owned());
    }
    fn log_warn(&mut self, message: &str) {
        self.log(LogLevel::Warn, message.to_owned());
    }
    fn log_error(&mut self, message: &str) {
        self.log(LogLevel::Error, message.to_owned());
    }

    fn find_pptr(&self, asset_name: &str, path_id: i64) -> Option<AssetRef>;
    fn find_assets(&self, name_contains: &str, class_name: Option<&str>) -> Vec<AssetSummary>;
    fn enum_by_class(&self, class_name: &str) -> Vec<AssetSummary>;

    fn variable(&self, key: &str) -> Option<&str>;
    fn set_variable(&mut self, key: String, value: String);
}

/// Default thread-local or test-friendly host context implementation.
#[derive(Debug, Default)]
pub struct InMemoryHostContext {
    pub logs: Vec<LogEntry>,
    pub status: String,
    pub assets: Vec<AssetSummary>,
    pub variables: HashMap<String, String>,
}

impl InMemoryHostContext {
    pub fn with_assets(assets: Vec<AssetSummary>) -> Self {
        Self {
            assets,
            ..Default::default()
        }
    }
}

impl PluginHostContext for InMemoryHostContext {
    fn log(&mut self, level: LogLevel, message: String) {
        self.logs.push(LogEntry { level, message });
    }

    fn set_status(&mut self, status: String) {
        self.status = status;
    }

    fn find_pptr(&self, asset_name: &str, path_id: i64) -> Option<AssetRef> {
        self.assets
            .iter()
            .find(|a| {
                a.asset_ref.path_id == path_id && (asset_name.is_empty() || a.name == asset_name)
            })
            .map(|a| a.asset_ref)
    }

    fn find_assets(&self, name_contains: &str, class_name: Option<&str>) -> Vec<AssetSummary> {
        let needle = name_contains.to_lowercase();
        self.assets
            .iter()
            .filter(|a| {
                (needle.is_empty() || a.name.to_lowercase().contains(&needle))
                    && class_name.is_none_or(|c| a.class_name.eq_ignore_ascii_case(c))
            })
            .cloned()
            .collect()
    }

    fn enum_by_class(&self, class_name: &str) -> Vec<AssetSummary> {
        self.assets
            .iter()
            .filter(|a| a.class_name.eq_ignore_ascii_case(class_name))
            .cloned()
            .collect()
    }

    fn variable(&self, key: &str) -> Option<&str> {
        self.variables.get(key).map(String::as_str)
    }

    fn set_variable(&mut self, key: String, value: String) {
        self.variables.insert(key, value);
    }
}
