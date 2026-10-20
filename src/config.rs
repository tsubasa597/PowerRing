//! 配置：%APPDATA%\PowerRing\config.json
//! 原则（PRD F11.3）：缺字段或字段非法时回退默认值并继续运行，绝不因配置问题崩溃。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 轮询间隔（秒）
    pub poll_interval_secs: u32,
    /// 低电量警告阈值
    pub low_warn: u8,
    /// 低电量严重阈值
    pub low_critical: u8,
    pub language: String,
    pub theme: String,
    pub autostart: bool,
    pub update_channel: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval_secs: 60,
            low_warn: 20,
            low_critical: 10,
            language: "zh-CN".into(),
            theme: "system".into(),
            autostart: false,
            update_channel: "stable".into(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        let base = std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        base.join("PowerRing").join("config.json")
    }

    pub fn load() -> Self {
        let p = Self::path();
        let cfg = fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str::<Config>(&s).ok())
            .unwrap_or_default();

        // 回写一次完整配置，用户照着改即可（手改后重启生效，F11.2）
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(&cfg) {
            let _ = fs::write(&p, s);
        }
        cfg
    }
}

// 设备协议声明已迁到 src/profile.rs（M1 起由它驱动扫描，不再是只读的摆设）
