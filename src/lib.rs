//! Horama: color-managed RAW and image viewer.
//!
//! The library holds everything that does not depend on the user interface toolkit: decoding,
//! color management, file operations, the browsing model and the view geometry. Two programs
//! use it during the move to egui: `horama` (Slint, the current interface) and `horama-egui`
//! (the new interface, see [`egui_app`]).

pub mod browse;
pub mod cache_manager;
pub mod color_manager;
pub mod config;
pub mod cr3_parser;
pub mod disk_ops;
pub mod egui_app;
pub mod exif_orient;
pub mod file_watcher;
pub mod image_loader;
pub mod image_saver;
pub mod logging;
pub mod mcp_server;
pub mod metadata;
pub mod open_with;
pub mod persist;
pub mod ratings;
pub mod rename;
pub mod view;
