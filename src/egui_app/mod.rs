//! The egui interface, under construction next to the Slint one.
//!
//! Done so far: open a folder (menu, command line or drag and drop); folder tree with the
//! same rules as the Slint interface ([`crate::browse`]); filmstrip with thumbnails and
//! Ctrl/Shift selection; the current image color-managed, with fit / 100% / zoom around the
//! cursor / pan ([`crate::view`]); the neighbours of the current image preloaded into the
//! shared RAM cache; ratings (keys 0–5 or the stars in the status bar) and all the filters
//! ([`filters`]); the info panel with histogram and metadata. Editing and the other tools
//! come in later stages.
//!
//! Only `egui` is used here, not `eframe`: the `horama-egui` program puts [`HoramaApp`] in a
//! native window. Disk reads and decoding run on worker threads; their results come back
//! through a channel read at every frame, and stale results (an older folder or image) are
//! dropped by generation number.

mod files;
mod filters;
mod i18n;
mod loader;
mod profiles;
mod theme;
mod thumbs;
mod watch;

use crate::browse::{self, ClickMods, Filters};
use crate::cache_manager::PreloadController;
use crate::color_manager::ColorManager;
use crate::config::{is_supported_file, normalize_path, scan_folder_and_subfolders, AppSettings};
use crate::disk_ops::DiskQueue;
use crate::view;
use egui::{
    Align2, Color32, ColorImage, Key, KeyboardShortcut, Modifiers, Pos2, Rect, Sense, Stroke,
    TextureHandle, TextureOptions, Vec2,
};
use i18n::{tr, trf};
use loader::ImageLoader;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use thumbs::{ThumbResult, ThumbnailLoader};

use theme::ACCENT;
/// Filmstrip cell (thumbnail, name and spacing) and thumbnail box, in points.
const CELL_W: f32 = 132.0;
const THUMB_BOX: Vec2 = Vec2::new(120.0, 84.0);
const FILMSTRIP_H: f32 = 126.0;

/// Results of the worker threads.
enum Msg {
    Scanned {
        generation: u64,
        files: Vec<String>,
        subfolders: Vec<String>,
        select: Option<String>,
    },
    Aspects {
        generation: u64,
        results: Vec<(String, f32)>,
    },
    Loaded {
        generation: u64,
        path: String,
        image: ColorImage,
        details: String,
        info: loader::ImageInfo,
    },
    Failed {
        generation: u64,
        path: String,
        error: String,
    },
    /// Outcome of a file operation on the disk queue ([`files`]).
    File(files::FileResult),
    /// Changes on disk in the open folder ([`watch`]).
    Disk {
        generation: u64,
        actions: Vec<crate::file_watcher::FileSyncAction>,
    },
}

/// The image on screen.
struct Shown {
    path: String,
    texture: TextureHandle,
    size: (u32, u32),
    details: String,
    /// Metadata rows (label, value) for the info panel.
    metadata: Vec<(String, String)>,
    histogram: TextureHandle,
}

/// Zoom and pan of the image view; `zoom` in screen pixels per image pixel (see [`view`]).
struct ViewState {
    zoom: f32,
    pan: Vec2,
    fit: bool,
}

pub struct HoramaApp {
    ctx: egui::Context,
    color: ColorManager,
    /// File writes (rotation, Recycle Bin, rename, save), one at a time and in order.
    disk: DiskQueue,
    settings: AppSettings,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    scan_generation: u64,
    load_generation: u64,

    preloader: PreloadController,
    image_loader: ImageLoader,
    thumb_loader: ThumbnailLoader,
    thumb_rx: Receiver<ThumbResult>,
    /// Thumbnails by path; `None` = the file could not be read.
    thumbs: HashMap<String, Option<TextureHandle>>,
    /// Thumbnail results older than this generation belong to a previous folder.
    thumb_folder_generation: u64,
    thumb_generation: u64,
    /// Thumbnails are asked for and kept around this filmstrip index (the current image, or
    /// the middle of the strip when the user scrolls it).
    thumb_center: usize,
    /// Scroll the filmstrip to the current image on the next frame.
    scroll_to_current: bool,

    folder: Option<String>,
    /// Watches the open folder for changes made by other programs.
    watcher: Option<crate::file_watcher::FolderWatcher>,
    /// Changes reported for an older folder carry an older generation.
    watch_generation: u64,
    subfolders: Vec<String>,
    /// Expanded folders of the tree ([`browse::tree_rows`]).
    expanded: HashSet<String>,
    all_files: Vec<String>,
    filters: Filters,
    /// Aspect ratios measured for the aspect-ratio filter (NaN = unreadable).
    aspects: HashMap<String, f32>,
    aspect_job_running: bool,
    visible: Vec<String>,
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
    current: Option<usize>,

    shown: Option<Shown>,
    loading: Option<String>,
    view: ViewState,
    /// The info panel (histogram and metadata) on the right.
    show_info: bool,
    /// Color profiles chosen by the user, by [`crate::config::path_key`].
    profile_overrides: HashMap<String, String>,
    notice: Option<files::Notice>,
    rename: Option<files::RenameDialog>,
    quality: Option<files::QualityDialog>,
    status: String,
}

impl HoramaApp {
    /// Creates the interface; `ctx` is kept so worker threads can ask for a repaint.
    pub fn new(ctx: egui::Context) -> Self {
        let mut settings = AppSettings::load();
        ctx.set_theme(theme::preference(settings.theme));
        i18n::set_language(&settings.language);
        let (tx, rx) = channel();
        let mut color = ColorManager::new();
        profiles::register_saved(&mut color, &mut settings);
        let (thumb_tx, thumb_rx) = channel();
        let repaint = ctx.clone();
        let thumb_loader =
            ThumbnailLoader::start(color.clone(), thumb_tx, move || repaint.request_repaint());
        let preloader = PreloadController::new();
        let image_loader = ImageLoader::start(preloader.loader(), tx.clone(), ctx.clone());
        HoramaApp {
            ctx,
            color,
            disk: DiskQueue::new(),
            settings,
            tx,
            rx,
            scan_generation: 0,
            load_generation: 0,
            preloader,
            image_loader,
            thumb_loader,
            thumb_rx,
            thumbs: HashMap::new(),
            thumb_folder_generation: 0,
            thumb_generation: 0,
            thumb_center: 0,
            scroll_to_current: false,
            folder: None,
            watcher: None,
            watch_generation: 0,
            subfolders: Vec::new(),
            expanded: HashSet::new(),
            all_files: Vec::new(),
            filters: Filters::default(),
            aspects: HashMap::new(),
            aspect_job_running: false,
            visible: Vec::new(),
            selected: BTreeSet::new(),
            anchor: None,
            current: None,
            shown: None,
            loading: None,
            view: ViewState {
                zoom: 1.0,
                pan: Vec2::ZERO,
                fit: true,
            },
            show_info: true,
            profile_overrides: HashMap::new(),
            notice: None,
            rename: None,
            quality: None,
            status: tr("Open a folder to view images").to_string(),
        }
    }

    /// Opens a folder, or an image in its folder with that image selected.
    pub fn open_path(&mut self, path: &str) {
        let p = Path::new(path);
        if p.is_dir() {
            self.open_folder(&normalize_path(path), None);
        } else if p.is_file() && is_supported_file(p) {
            let file = normalize_path(path);
            let parent = p
                .parent()
                .map(|d| normalize_path(&d.to_string_lossy()))
                .unwrap_or_default();
            self.open_folder(&parent, Some(file));
        } else if p.is_file() {
            self.status = trf(
                "'{}' is not an image format Horama can open.",
                &[&browse::file_name_of(path)],
            );
        } else {
            self.status = trf("'{}' does not exist.", &[&path]);
        }
    }

    /// Opens the folder the user used last, if it still exists.
    pub fn open_last_folder(&mut self) {
        let last = self.settings.last_folder.clone();
        if !last.is_empty() && Path::new(&last).is_dir() {
            self.open_folder(&last, None);
        }
    }

    /// Scans `folder` (with subfolders) on a worker thread; `select` is shown when done.
    fn open_folder(&mut self, folder: &str, select: Option<String>) {
        self.close_notice();
        self.folder = Some(folder.to_string());
        // Shared with the Slint interface: last folder and recent folders.
        self.settings.add_recent_folder(folder);
        self.filters.tree = browse::TREE_ALL.to_string();
        self.expanded = HashSet::from([browse::TREE_ALL.to_string()]);
        self.aspects.clear();
        // Thumbnails of the previous folder are dropped, and its late results ignored.
        self.thumbs.clear();
        self.thumb_folder_generation = self.thumb_loader.request(Vec::new());
        if let Ok(mut cache) = self.preloader.cache.lock() {
            cache.clear();
        }
        self.status = trf("Scanning {}…", &[&folder]);
        self.watch_folder(folder);
        self.start_scan(folder, select);
    }

    /// Reads the open folder again, keeping filters, tree, thumbnails and the current image
    /// (used when folders change on disk).
    pub(super) fn rescan_folder(&mut self) {
        if let Some(folder) = self.folder.clone() {
            let current = self.current_path();
            self.start_scan(&folder, current);
        }
    }

    /// Lists the images of `folder` on a worker thread; `select` is shown when done.
    fn start_scan(&mut self, folder: &str, select: Option<String>) {
        self.scan_generation += 1;
        let generation = self.scan_generation;
        let (tx, ctx, folder) = (self.tx.clone(), self.ctx.clone(), folder.to_string());
        let spawned = std::thread::Builder::new()
            .name("egui-scan".into())
            .spawn(move || {
                let (files, subfolders) = scan_folder_and_subfolders(&folder);
                let _ = tx.send(Msg::Scanned {
                    generation,
                    files,
                    subfolders,
                    select,
                });
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.status = trf("Could not scan the folder: {}", &[&e]);
        }
    }

    /// Recomputes the visible files, keeping the current file if it is still visible.
    fn refilter(&mut self) {
        let previous = self.current_path();
        let root = self.folder.clone().unwrap_or_default();
        let filtered = {
            let cache = self.preloader.cache.lock().ok();
            let facts = filters::Facts {
                settings: &self.settings,
                overrides: &self.profile_overrides,
                aspects: &self.aspects,
                cache: cache.as_deref(),
            };
            browse::filter_files(
                &self.all_files,
                &root,
                &self.filters,
                &self.settings.saved_aspect_ratios,
                &facts,
            )
        };
        self.visible = filtered.visible;
        self.measure_aspects(filtered.unknown_ratio);
        self.selected.clear();
        self.anchor = None;
        self.current = None;
        if !self.visible.is_empty() {
            let i = browse::index_after_refilter(previous.as_deref(), &self.visible);
            self.select_only(i);
        }
        self.request_thumbs();
    }

    /// Asks for the missing thumbnails around [`Self::thumb_center`], closest first.
    fn request_thumbs(&mut self) {
        let order = thumbs::priority_order(
            &self.visible,
            Some(self.thumb_center),
            thumbs::RADIUS,
            |p| self.thumbs.contains_key(p),
        );
        self.thumb_generation = self.thumb_loader.request(order);
    }

    /// Drops the thumbnail textures far from [`Self::thumb_center`] once there are too many,
    /// so a folder with thousands of images does not keep all of them in memory.
    fn limit_thumbs(&mut self) {
        if self.thumbs.len() <= thumbs::MAX_KEPT {
            return;
        }
        let lo = self.thumb_center.saturating_sub(thumbs::RADIUS);
        let hi = (self.thumb_center + thumbs::RADIUS + 1).min(self.visible.len());
        let near: HashSet<&str> = self
            .visible
            .get(lo..hi)
            .unwrap_or(&[])
            .iter()
            .map(String::as_str)
            .collect();
        self.thumbs.retain(|p, _| near.contains(p.as_str()));
    }

    fn current_path(&self) -> Option<String> {
        self.current.and_then(|i| self.visible.get(i).cloned())
    }

    fn select_only(&mut self, i: usize) {
        self.selected.clear();
        self.selected.insert(i);
        self.anchor = Some(i);
        self.show_index(i);
    }

    /// Makes item `i` the current image and starts loading it.
    fn show_index(&mut self, i: usize) {
        let Some(path) = self.visible.get(i).cloned() else {
            return;
        };
        let moved = self.current != Some(i);
        self.current = Some(i);
        self.scroll_to_current = true;
        if !self.selected.contains(&i) {
            self.selected.clear();
            self.selected.insert(i);
            self.anchor = Some(i);
        }
        if moved {
            self.thumb_center = i;
            self.request_thumbs();
        }
        if self.shown.as_ref().map(|s| s.path == path).unwrap_or(false) {
            return;
        }
        self.load(path);
        // Neighbours are decoded into the RAM cache, so the next arrow press is instant.
        self.preloader.preload_neighborhood(i, &self.visible, || {});
    }

    /// Decodes and color-manages `path` on the loading thread ([`loader`]); the previous
    /// image stays on screen until the new one is ready.
    fn load(&mut self, path: String) {
        self.load_generation += 1;
        self.loading = Some(path.clone());
        self.image_loader.request(loader::LoadJob {
            generation: self.load_generation,
            profile: self.profile_of(&path),
            color: self.color.clone(),
            edited: None,
            path,
        });
    }

    /// Applies the results the workers sent since the last frame.
    fn receive(&mut self) {
        while let Ok(t) = self.thumb_rx.try_recv() {
            if t.generation <= self.thumb_folder_generation {
                continue; // requested for a previous folder
            }
            let texture = t.image.map(|img| {
                self.ctx
                    .load_texture(format!("thumb:{}", t.path), img, TextureOptions::LINEAR)
            });
            self.thumbs.insert(t.path, texture);
        }
        self.limit_thumbs();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Scanned {
                    generation,
                    files,
                    subfolders,
                    select,
                } if generation == self.scan_generation => {
                    self.all_files = files;
                    self.subfolders = subfolders;
                    self.current = None;
                    self.refilter();
                    if let Some(file) = select {
                        match self
                            .visible
                            .iter()
                            .position(|f| crate::config::same_path(f, &file))
                        {
                            Some(i) => self.select_only(i),
                            None => {
                                self.status = trf(
                                    "'{}' was not found in its folder.",
                                    &[&browse::file_name_of(&file)],
                                )
                            }
                        }
                    }
                    if self.visible.is_empty() {
                        self.shown = None;
                        self.status = tr("No supported images in this folder.").to_string();
                    }
                }
                Msg::Aspects {
                    generation,
                    results,
                } => {
                    self.aspect_job_running = false;
                    if generation == self.scan_generation {
                        self.aspects.extend(results);
                        self.refilter();
                    }
                }
                Msg::Loaded {
                    generation,
                    path,
                    image,
                    details,
                    info,
                } if generation == self.load_generation => {
                    let size = (image.size[0] as u32, image.size[1] as u32);
                    let options = TextureOptions {
                        // Mipmaps: zoomed-out views are filtered instead of aliased.
                        mipmap_mode: Some(egui::TextureFilter::Linear),
                        // Above 100% every image pixel is shown as a sharp square.
                        magnification: egui::TextureFilter::Nearest,
                        ..TextureOptions::LINEAR
                    };
                    let texture = self.ctx.load_texture(path.clone(), image, options);
                    // The same image again (another profile, a file changed on disk) keeps
                    // the zoom and position.
                    let same = self.shown.as_ref().map(|s| s.path == path).unwrap_or(false);
                    let histogram =
                        self.ctx
                            .load_texture("histogram", info.histogram, TextureOptions::LINEAR);
                    self.shown = Some(Shown {
                        path,
                        texture,
                        size,
                        details,
                        metadata: info.metadata,
                        histogram,
                    });
                    self.loading = None;
                    if !same {
                        self.view.fit = true;
                        self.view.pan = Vec2::ZERO;
                    }
                }
                Msg::Failed {
                    generation,
                    path,
                    error,
                } if generation == self.load_generation => {
                    self.loading = None;
                    self.status = trf(
                        "Cannot display {}: {}",
                        &[&browse::file_name_of(&path), &error],
                    );
                }
                Msg::File(result) => self.file_result(result),
                Msg::Disk {
                    generation,
                    actions,
                } if generation == self.watch_generation => self.disk_changed(actions),
                _ => {} // result of an older request
            }
        }
    }

    /// Draws the whole interface. Call once per frame with the window's root `Ui`.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        // Textures belong to the context that draws them (the window, or a test harness).
        self.ctx = ui.ctx().clone();
        self.receive();
        self.expire_notice();
        self.handle_keys(ui);
        self.handle_dropped_files(ui);

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(self.status_text());
                if let Some(path) = self.current_path() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let current = self.settings.get_rating(&path);
                        if let Some(stars) = filters::star_picker(ui, current) {
                            self.set_rating(stars);
                        }
                    });
                }
            });
        });
        egui::Panel::bottom("filmstrip")
            .resizable(false)
            .exact_size(FILMSTRIP_H)
            .show(ui, |ui| self.filmstrip(ui));
        egui::Panel::left("folders")
            .resizable(true)
            .default_size(280.0)
            .size_range(180.0..=600.0)
            .show(ui, |ui| self.folder_tree(ui));
        if self.show_info {
            egui::Panel::right("info")
                .resizable(true)
                .default_size(300.0)
                .size_range(240.0..=520.0)
                .show(ui, |ui| self.info_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::canvas(&ui.ctx().global_style().visuals)))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                self.image_view(ui);
                self.notice_overlay(ui, rect);
            });
        let ctx = ui.ctx().clone();
        self.rename_window(&ctx);
        self.quality_window(&ctx);
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add(egui::Button::new(tr("Open Folder…")).shortcut_text("Ctrl+O"))
                    .clicked()
                {
                    self.pick_folder();
                }
                let recent = self.settings.recent_folders.clone();
                ui.add_enabled_ui(!recent.is_empty(), |ui| {
                    ui.menu_button(tr("Recent Folders"), |ui| {
                        for folder in &recent {
                            if ui.button(folder.as_str()).clicked() {
                                self.open_path(folder);
                            }
                        }
                        ui.separator();
                        if ui.button(tr("Clear List")).clicked() {
                            self.settings.clear_recent_folders();
                        }
                    });
                });
                ui.separator();
                let has_image = self.current.is_some();
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new(tr("Save a Copy As…")).shortcut_text("Ctrl+Shift+S"),
                    )
                    .clicked()
                {
                    self.save_as();
                }
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new(tr("Rename…")).shortcut_text("F2"),
                    )
                    .clicked()
                {
                    self.start_rename();
                }
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new(tr("Move to Recycle Bin")).shortcut_text("Del"),
                    )
                    .clicked()
                {
                    self.trash_selected();
                }
                ui.separator();
                if ui.button(tr("Exit")).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(tr("Image"), |ui| {
                let has_image = self.current.is_some();
                self.profile_menu(ui);
                ui.separator();
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new(tr("Rotate Right")).shortcut_text("R"),
                    )
                    .clicked()
                {
                    self.rotate(true);
                }
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new(tr("Rotate Left")).shortcut_text("L"),
                    )
                    .clicked()
                {
                    self.rotate(false);
                }
            });
            ui.menu_button(tr("View"), |ui| {
                let has_image = self.shown.is_some();
                let mut info = self.show_info;
                if ui
                    .add(egui::Checkbox::new(&mut info, tr("Image Info")))
                    .on_hover_text(tr("Histogram and metadata (I)"))
                    .changed()
                {
                    self.show_info = info;
                }
                ui.menu_button(tr("Theme"), |ui| {
                    for choice in crate::config::ThemeChoice::ALL {
                        if ui
                            .radio(self.settings.theme == choice, tr(choice.label()))
                            .clicked()
                        {
                            self.settings.theme = choice;
                            self.settings.save();
                            ui.ctx().set_theme(theme::preference(choice));
                        }
                    }
                });
                ui.menu_button(tr("Language"), |ui| {
                    for (value, name) in i18n::CHOICES {
                        let label = if value.is_empty() { tr(name) } else { name };
                        if ui.radio(self.settings.language == value, label).clicked() {
                            self.settings.language = value.to_string();
                            self.settings.save();
                            i18n::set_language(value);
                        }
                    }
                });
                ui.separator();
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new(tr("Fit to Window")).shortcut_text("F"),
                    )
                    .clicked()
                {
                    self.view.fit = true;
                    self.view.pan = Vec2::ZERO;
                }
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new(tr("Actual Size (100%)")).shortcut_text("Ctrl+1"),
                    )
                    .clicked()
                {
                    self.set_zoom(1.0);
                }
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new(tr("Zoom In")).shortcut_text("+"),
                    )
                    .clicked()
                {
                    self.set_zoom(self.view.zoom * view::ZOOM_STEP);
                }
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new(tr("Zoom Out")).shortcut_text("−"),
                    )
                    .clicked()
                {
                    self.set_zoom(self.view.zoom / view::ZOOM_STEP);
                }
            });
        });
    }

    fn pick_folder(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Open Folder");
        if let Some(f) = &self.folder {
            dialog = dialog.set_directory(f);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.open_folder(&normalize_path(&dir.to_string_lossy()), None);
        }
    }

    fn set_zoom(&mut self, zoom: f32) {
        self.view.zoom = view::clamp_zoom(zoom);
        self.view.fit = false;
    }

    fn handle_keys(&mut self, ui: &mut egui::Ui) {
        // Keys go to text fields when one has the focus (the search box), and to the windows.
        if ui.ctx().egui_wants_keyboard_input() || self.rename.is_some() || self.quality.is_some() {
            return;
        }
        let commands = ui.input_mut(|i| {
            (
                i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)),
                i.consume_shortcut(&KeyboardShortcut::new(
                    Modifiers::COMMAND | Modifiers::SHIFT,
                    Key::S,
                )),
                i.key_pressed(Key::Delete),
                i.key_pressed(Key::F2),
                i.key_pressed(Key::R) && !i.modifiers.any(),
                i.key_pressed(Key::L) && !i.modifiers.any(),
                i.key_pressed(Key::P) && !i.modifiers.any(),
            )
        });
        let (undo, save_as, delete, rename, rotate_right, rotate_left, cycle) = commands;
        if cycle && self.current.is_some() {
            self.cycle_profile();
        }
        if undo && !self.undo_trash() {
            self.status = tr("Nothing to undo.").to_string();
        }
        if self.current.is_some() {
            if save_as {
                self.save_as();
            }
            if delete {
                self.trash_selected();
            }
            if rename {
                self.start_rename();
            }
            if rotate_right {
                self.rotate(true);
            }
            if rotate_left {
                self.rotate(false);
            }
        }
        let open = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
        const RATING_KEYS: [Key; 6] = [
            Key::Num0,
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
        ];
        let rating = ui.input(|i| {
            if i.modifiers.any() {
                return None;
            }
            RATING_KEYS.iter().position(|k| i.key_pressed(*k))
        });
        let (open, next, prev, fit, actual, zoom_in, zoom_out, info) = ui.input_mut(|i| {
            (
                i.consume_shortcut(&open),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::F),
                i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Num1)),
                i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals),
                i.key_pressed(Key::Minus),
                i.key_pressed(Key::I) && !i.modifiers.any(),
            )
        });
        if info {
            self.show_info = !self.show_info;
        }
        if open {
            self.pick_folder();
        }
        if let Some(stars) = rating {
            self.set_rating(stars as u8);
        }
        if !self.visible.is_empty() {
            let last = self.visible.len() - 1;
            let cur = self.current.unwrap_or(0);
            if next && cur < last {
                self.select_only(cur + 1);
            }
            if prev && cur > 0 {
                self.select_only(cur - 1);
            }
        }
        if self.shown.is_some() {
            if fit {
                self.view.fit = true;
                self.view.pan = Vec2::ZERO;
            }
            if actual {
                self.set_zoom(1.0);
            }
            if zoom_in {
                self.set_zoom(self.view.zoom * view::ZOOM_STEP);
            }
            if zoom_out {
                self.set_zoom(self.view.zoom / view::ZOOM_STEP);
            }
        }
    }

    /// Files or folders dragged from the file manager: the first one is opened.
    fn handle_dropped_files(&mut self, ui: &egui::Ui) {
        let dropped = ui.input(|i| i.raw.dropped_files.clone());
        if let Some(first) = dropped.first() {
            let path = first.path().to_string_lossy().to_string();
            if !path.is_empty() {
                self.open_path(&path);
            }
        }
    }

    fn folder_tree(&mut self, ui: &mut egui::Ui) {
        let Some(root) = self.folder.clone() else {
            ui.label(tr("No folder open"));
            return;
        };
        ui.label(egui::RichText::new(browse::file_name_of(&root)).strong())
            .on_hover_text(root.as_str());
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.filters.search)
                .hint_text(tr("Search file names"))
                .desired_width(f32::INFINITY),
        );
        if search.changed() {
            self.refilter();
        }
        self.filters_panel(ui);
        ui.label(trf("{} images shown", &[&self.visible.len()]));
        ui.separator();

        let rows = browse::tree_rows(
            &root,
            &self.subfolders,
            &self.all_files,
            &self.expanded,
            &self.filters.tree,
        );
        let mut chosen: Option<String> = None;
        let mut toggled: Option<String> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in &rows {
                    ui.horizontal(|ui| {
                        ui.add_space(row.depth as f32 * 14.0);
                        if expander(ui, row.has_children, row.expanded).clicked() {
                            toggled = Some(row.key.clone());
                        }
                        let label = format!("{} ({})", row.name, row.count);
                        if ui.selectable_label(row.selected, label).clicked() {
                            chosen = Some(row.key.clone());
                        }
                    });
                }
            });
        // Changes are applied after the rows are drawn, never while iterating them.
        if let Some(key) = toggled {
            browse::toggle_expanded(&mut self.expanded, &root, &key);
        }
        if let Some(key) = chosen {
            if key != self.filters.tree {
                self.filters.tree = key;
                self.refilter();
            }
        }
    }

    /// Horizontal strip of thumbnails; only the cells in view are drawn.
    fn filmstrip(&mut self, ui: &mut egui::Ui) {
        let n = self.visible.len();
        let cell = Vec2::new(CELL_W, ui.available_height());
        let scroll_target = if std::mem::take(&mut self.scroll_to_current) {
            self.current
        } else {
            None
        };
        let mut clicked: Option<(usize, ClickMods)> = None;
        let mut strip_mid: Option<usize> = None;
        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                let origin = ui.max_rect().min;
                ui.set_min_size(Vec2::new(n as f32 * CELL_W, cell.y));
                let cell_rect = |i: usize| {
                    Rect::from_min_size(origin + Vec2::new(i as f32 * CELL_W, 0.0), cell)
                };
                let first = (viewport.min.x / CELL_W).floor().max(0.0) as usize;
                let last = ((viewport.max.x / CELL_W).ceil() as usize + 1).min(n);
                strip_mid = Some((first + last) / 2);
                for i in first..last {
                    let rect = cell_rect(i).shrink(3.0);
                    let response = ui.interact(rect, ui.id().with(("cell", i)), Sense::click());
                    let name = browse::file_name_of(&self.visible[i]);
                    let selected = self.selected.contains(&i);
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            trf("Thumbnail of {}", &[&name]),
                        )
                    });
                    self.paint_cell(ui, i, rect, response.hovered());
                    if response.clicked() {
                        let m = ui.input(|inp| inp.modifiers);
                        clicked = Some((
                            i,
                            ClickMods {
                                toggle: m.command,
                                range: m.shift,
                            },
                        ));
                    }
                }
                if let Some(c) = scroll_target.filter(|c| *c < n) {
                    ui.scroll_to_rect(cell_rect(c), Some(egui::Align::Center));
                }
            });
        // Scrolling the strip far from the current image moves where thumbnails are made.
        if let Some(mid) = strip_mid.filter(|_| scroll_target.is_none()) {
            if mid.abs_diff(self.thumb_center) > thumbs::RECENTER_AFTER {
                self.thumb_center = mid;
                self.request_thumbs();
            }
        }
        if let Some((i, mods)) = clicked {
            let show = browse::apply_click(
                &mut self.selected,
                &mut self.anchor,
                self.current,
                i,
                n,
                mods,
            );
            if let Some(i) = show {
                self.show_index(i);
            }
        }
    }

    fn paint_cell(&self, ui: &mut egui::Ui, i: usize, rect: Rect, hovered: bool) {
        let path = &self.visible[i];
        let painter = ui.painter_at(rect);
        let selected = self.selected.contains(&i);
        let visuals = ui.visuals();
        painter.rect_filled(rect, 4.0, theme::cell(visuals, hovered));
        let thumb_area = Rect::from_min_size(
            rect.min + Vec2::new((rect.width() - THUMB_BOX.x) / 2.0, 4.0),
            THUMB_BOX,
        );
        match self.thumbs.get(path) {
            Some(Some(tex)) => {
                let size = tex.size_vec2();
                let k = (THUMB_BOX.x / size.x).min(THUMB_BOX.y / size.y).min(1.0);
                let r = Rect::from_center_size(thumb_area.center(), size * k);
                let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                painter.image(tex.id(), r, uv, Color32::WHITE);
            }
            Some(None) => {
                painter.text(
                    thumb_area.center(),
                    Align2::CENTER_CENTER,
                    "⚠",
                    egui::FontId::proportional(18.0),
                    theme::canvas_text(visuals),
                );
            }
            None => {
                painter.rect_filled(thumb_area.shrink(8.0), 2.0, theme::placeholder(visuals));
            }
        }
        let stars = self.settings.get_rating(path);
        if stars > 0 {
            let size = 9.0;
            let left = Pos2::new(thumb_area.min.x + 2.0, thumb_area.max.y - size / 2.0 - 2.0);
            painter.rect_filled(
                Rect::from_min_size(
                    left - Vec2::new(1.0, size / 2.0 + 1.0),
                    Vec2::new(size * 5.0 + 2.0, size + 2.0),
                ),
                2.0,
                Color32::from_black_alpha(160),
            );
            filters::paint_stars(&painter, left, size, stars);
        }
        let label_rect = Rect::from_min_max(
            Pos2::new(rect.min.x + 4.0, thumb_area.max.y + 2.0),
            Pos2::new(rect.max.x - 4.0, rect.max.y - 2.0),
        );
        ui.put(
            label_rect,
            egui::Label::new(egui::RichText::new(browse::file_name_of(path)).small())
                .truncate()
                .selectable(false),
        );
        if selected {
            let width = if self.current == Some(i) { 2.5 } else { 1.5 };
            painter.rect_stroke(
                rect,
                4.0,
                Stroke::new(width, ACCENT),
                egui::StrokeKind::Inside,
            );
        }
    }

    fn image_view(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let label = match &self.shown {
            Some(s) => format!("{}: {}", tr("Image view"), browse::file_name_of(&s.path)),
            None => tr("Image view").to_string(),
        };
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, &label));
        let Some(shown) = &self.shown else {
            let text = if self.loading.is_some() {
                tr("Loading…")
            } else {
                tr("Open a folder (Ctrl+O) or drop an image here")
            };
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(14.0),
                theme::canvas_text(ui.visuals()),
            );
            return;
        };
        let scale = ui.ctx().pixels_per_point();
        let view_size = (rect.width(), rect.height());
        if self.view.fit {
            self.view.zoom =
                view::fit_zoom(view_size.0, view_size.1, shown.size.0, shown.size.1, scale);
        }

        // Wheel: zoom around the cursor. Drag: pan. Double click: fit.
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                if let Some(pos) = response.hover_pos() {
                    // One wheel notch (about 50 points) = one WHEEL_STEP.
                    let factor = view::WHEEL_STEP.powf(scroll / 50.0);
                    let cursor = (pos.x - rect.min.x, pos.y - rect.min.y);
                    let (zoom, pan) = view::zoom_at_cursor(
                        self.view.zoom,
                        (self.view.pan.x, self.view.pan.y),
                        factor,
                        cursor,
                        view_size,
                    );
                    self.view.zoom = zoom;
                    self.view.pan = Vec2::new(pan.0, pan.1);
                    self.view.fit = false;
                }
            }
        }
        if response.dragged() {
            self.view.pan += response.drag_delta();
            self.view.fit = false;
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if response.double_clicked() {
            self.view.fit = true;
            self.view.pan = Vec2::ZERO;
        }

        let lz = view::logical_zoom(self.view.zoom, scale);
        let size = Vec2::new(shown.size.0 as f32 * lz, shown.size.1 as f32 * lz);
        let center = rect.center() + self.view.pan;
        let image_rect = Rect::from_center_size(center, size);
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        ui.painter_at(rect)
            .image(shown.texture.id(), image_rect, uv, Color32::WHITE);
    }

    /// Histogram and metadata of the image on screen.
    fn info_panel(&self, ui: &mut egui::Ui) {
        let Some(shown) = &self.shown else {
            ui.label(egui::RichText::new(tr("No image")).weak());
            return;
        };
        ui.label(egui::RichText::new(tr("Histogram")).strong());
        let size = shown.histogram.size_vec2();
        let width = ui.available_width();
        ui.add(egui::Image::new((
            shown.histogram.id(),
            Vec2::new(width, width * size.y / size.x),
        )));
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("metadata")
                    .num_columns(2)
                    .striped(true)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for (label, value) in &shown.metadata {
                            ui.label(egui::RichText::new(tr(label)).weak());
                            ui.add(egui::Label::new(value).wrap());
                            ui.end_row();
                        }
                    });
            });
    }

    fn status_text(&self) -> String {
        let Some(shown) = &self.shown else {
            return self.status.clone();
        };
        let position = match self.current {
            Some(i) => format!("[{}/{}] ", i + 1, self.visible.len()),
            None => String::new(),
        };
        let loading = match &self.loading {
            Some(p) => trf(" — loading {}…", &[&browse::file_name_of(p)]),
            None => String::new(),
        };
        format!(
            "{}{} — {}×{} px — {} — {:.0}%{}",
            position,
            browse::file_name_of(&shown.path),
            shown.size.0,
            shown.size.1,
            shown.details,
            self.view.zoom * 100.0,
            loading
        )
    }
}

/// Expand / collapse arrow of a tree row (an empty space of the same width if `enabled` is
/// false, so names stay aligned).
fn expander(ui: &mut egui::Ui, enabled: bool, expanded: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::click());
    if enabled {
        let c = rect.center();
        let color = if response.hovered() {
            ui.visuals().strong_text_color()
        } else {
            ui.visuals().text_color()
        };
        let state = if expanded {
            tr("Collapse")
        } else {
            tr("Expand")
        };
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, state));
        let points = if expanded {
            vec![
                c + Vec2::new(-4.0, -2.0),
                c + Vec2::new(4.0, -2.0),
                c + Vec2::new(0.0, 3.0),
            ]
        } else {
            vec![
                c + Vec2::new(-2.0, -4.0),
                c + Vec2::new(3.0, 0.0),
                c + Vec2::new(-2.0, 4.0),
            ]
        };
        ui.painter()
            .add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        response.on_hover_text(if expanded {
            tr("Collapse")
        } else {
            tr("Expand")
        })
    } else {
        response
    }
}
