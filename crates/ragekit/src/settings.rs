//! Browser settings saved in ~/.config/ragekit/settings.json.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub dark: bool,
    pub recent: Vec<PathBuf>,
    pub expand_nested: bool,
    pub convert_on_extract: bool,
    pub ui_scale: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { dark: true, recent: Vec::new(), expand_nested: true, convert_on_extract: false, ui_scale: 1.0 }
    }
}

fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("ragekit").join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = path() {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            if let Ok(b) = serde_json::to_vec_pretty(self) {
                let _ = std::fs::write(p, b);
            }
        }
    }

    /// Remember a folder or archive at the top of the recent list.
    pub fn touch_recent(&mut self, p: PathBuf) {
        self.recent.retain(|r| r != &p);
        self.recent.insert(0, p);
        self.recent.truncate(10);
        self.save();
    }
}
