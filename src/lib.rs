//! Horama: color-managed RAW and image viewer.
//!
//! The library holds everything that does not depend on the user interface toolkit: decoding,
//! color management, file operations, the browsing model and the view geometry. The program
//! `horama` adds the egui interface ([`egui_app`]).
//!
//! No `unsafe` code, except in the few modules that call the Windows API directly: each of them
//! says so with `#[expect(unsafe_code)]`, and each `unsafe` block states why it is sound.
#![deny(unsafe_code)]
#![deny(clippy::undocumented_unsafe_blocks)]

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
pub mod print;
pub mod ratings;
pub mod rename;
pub mod view;
