//! 测试用的图形界面：不带参数运行 `peerdesk`（例如双击）时打开。
//!
//! 正式的产品界面以后用 Tauri 做；这里是让两台自己的机器互相控制、验证整条链路的控制台。
//! `app.rs` 和 `viewer.rs` 只是 eframe 的接线（画控件、转发事件），其余模块都是可测试的逻辑：
//! 键位映射、坐标换算、输入翻译、状态机、密码、最近连接、语言和字体选择、后台会话。

mod app;
pub mod assistant;
mod background;
pub mod book;
pub mod coords;
pub mod files;
pub mod files_window;
pub mod fonts;
pub mod form;
pub mod guide_art;
pub mod home;
pub mod i18n;
pub mod input;
pub mod keys;
pub mod layout;
pub mod netinfo;
pub mod password;
pub mod recent;
pub mod session;
pub mod settings;
pub mod state;
pub mod stats;
pub mod theme;
pub mod transfers;
mod tray;
pub mod tray_logic;
mod viewer;
pub mod viewsize;
mod yuv;

pub use app::run;
