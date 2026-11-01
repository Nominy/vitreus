//! The photo viewer window: a dimmed overlay with the photo, a title bar and a
//! film strip, in the spirit of Picasa Photo Viewer.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use futures::StreamExt as _;
use gpui::{
    AnyElement, ClickEvent, Context, CursorStyle, ExternalPaths, FocusHandle, Hsla, Image,
    ImageFormat, ImageSource, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, NavigationDirection, PathPromptOptions, PromptLevel, Render, RenderImage,
    Pixels, Point, Resource, ScrollDelta, ScrollWheelEvent, SharedString, Stateful, Task,
    TouchPhase, Window, WindowControlArea, actions, anchored, deferred, div, img, prelude::*, px,
    rgba, svg,
};

use crate::{
    catalog,
    decode::{self, Decoded},
    info,
    loader::{Kind, Loaded, Loader, Request},
    settings::{Scaling, Settings},
};

actions!(
    viewer,
    [
        Next,
        Previous,
        First,
        Last,
        ZoomIn,
        ZoomOut,
        ZoomFit,
        ZoomActual,
        RotateLeft,
        RotateRight,
        ToggleFullscreen,
        ToggleInfo,
        ToggleSettings,
        ToggleChrome,
        DeleteFile,
        RevealFile,
        CopyImage,
        OpenFile,
        Close,
    ]
);

pub const KEY_CONTEXT: &str = "Viewer";

const TOP_BAR: f32 = 40.0;
const BOTTOM_BAR: f32 = 96.0;
/// Width reserved on each side of the film strip for buttons (kept equal so the strip is centered).
const SIDE_CLUSTER: f32 = 250.0;
const CELL: f32 = 56.0;
const CELL_CURRENT: f32 = 72.0;
const CELL_GAP: f32 = 6.0;

/// Screen-sized decodes kept around the current photo.
const PREVIEW_AHEAD: usize = 2;
const PREVIEW_BEHIND: usize = 1;
/// Thumbnails kept in memory on either side of the current photo.
const THUMB_KEEP: usize = 120;

const ZOOM_STEP: f32 = 1.25;
/// Mouse wheels report three lines per notch on Windows, one elsewhere.
const LINES_PER_NOTCH: f32 = if cfg!(windows) { 3.0 } else { 1.0 };
/// Horizontal trackpad travel that flips to the next photo.
const SWIPE_DISTANCE: f32 = 120.0;

const OVERLAY: u32 = 0x0a0a0ad9;
const BAR: u32 = 0x00000059;
const TEXT: u32 = 0xf2f2f2ff;
const MUTED: u32 = 0xa0a0a0ff;
const ICON: u32 = 0xffffffbf;
/// Brand celeste from the aperture logo.
const ACCENT: u32 = 0x8fd3fbff;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn contains(&self, (px, py): (f32, f32)) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

struct Drag {
    start: (f32, f32),
    offset: (f32, f32),
    on_photo: bool,
    moved: bool,
}

pub struct Viewer {
    focus: FocusHandle,
    loader: Loader,
    _receive: Task<()>,
    scan: Option<Task<()>>,

    files: Vec<PathBuf>,
    positions: HashMap<PathBuf, usize>,
    index: usize,
    /// +1 when moving forward, -1 backward; decides which neighbours preload first.
    direction: isize,

    previews: HashMap<PathBuf, Decoded>,
    thumbs: HashMap<PathBuf, Decoded>,
    full: Option<(PathBuf, Decoded)>,
    errors: HashMap<PathBuf, String>,
    thumb_errors: HashSet<PathBuf>,
    /// User rotation per photo, quarter turns clockwise. Never written to the file.
    rotations: HashMap<PathBuf, u8>,
    /// Images no longer referenced; their GPU textures are freed on the next frame.
    pending_drops: Vec<Arc<RenderImage>>,

    /// `None` = fit to window (follows window resizes).
    zoom: Option<f32>,
    /// Photo center relative to the stage center, logical pixels.
    offset: (f32, f32),
    drag: Option<Drag>,
    swipe: f32,
    swipe_done: bool,

    viewport: (f32, f32),
    scale_factor: f32,
    show_chrome: bool,
    show_info: bool,
    info: Option<(PathBuf, Option<(u32, u32)>, Vec<(&'static str, String)>)>,
    file_size: Option<(PathBuf, Option<u64>)>,
    notice: Option<String>,
    /// A newer release the user hasn't skipped, found by the background check.
    update: Option<String>,
    /// Right-click menu, open at this window position.
    menu: Option<Point<Pixels>>,
    settings: Settings,
    show_settings: bool,
    /// Pixel-perfect mode: the visible part of the photo, resampled by nearest
    /// neighbour at exactly the screen's resolution (gpui only filters smoothly).
    pixel_view: Option<(PixelKey, Arc<RenderImage>)>,
    title: String,
}

impl Viewer {
    pub fn new(path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let scale_factor = window.scale_factor();
        let screen = window
            .display(cx)
            .map(|d| d.bounds().size)
            .unwrap_or_else(|| gpui::size(px(1920.0), px(1080.0)));
        let preview_size = (
            (f32::from(screen.width) * scale_factor).ceil() as u32,
            (f32::from(screen.height) * scale_factor).ceil() as u32,
        );
        let settings = Settings::load();
        decode::set_scaling(settings.scaling);
        let (loader, mut results) = Loader::new(preview_size);
        let receive = cx.spawn(async move |this, cx| {
            while let Some(first) = results.next().await {
                let mut batch = vec![first];
                while let Ok(more) = results.try_recv() {
                    batch.push(more);
                }
                let updated = this.update(cx, |this, cx| {
                    for loaded in batch {
                        this.on_loaded(loaded);
                    }
                    this.request_work();
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });

        let check = cx.background_spawn(async { crate::update::check() });
        cx.spawn(async move |this, cx| {
            if let Some(version) = check.await {
                this.update(cx, |this, cx| {
                    this.update = Some(version);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();

        let focus = cx.focus_handle();
        window.focus(&focus);
        let viewport = window.viewport_size();
        let mut this = Self {
            focus,
            loader,
            _receive: receive,
            scan: None,
            files: Vec::new(),
            positions: HashMap::new(),
            index: 0,
            direction: 1,
            previews: HashMap::new(),
            thumbs: HashMap::new(),
            full: None,
            errors: HashMap::new(),
            thumb_errors: HashSet::new(),
            rotations: HashMap::new(),
            pending_drops: Vec::new(),
            zoom: None,
            offset: (0.0, 0.0),
            drag: None,
            swipe: 0.0,
            swipe_done: false,
            viewport: (viewport.width.into(), viewport.height.into()),
            scale_factor,
            show_chrome: true,
            show_info: false,
            info: None,
            file_size: None,
            notice: None,
            update: None,
            menu: None,
            settings,
            show_settings: false,
            pixel_view: None,
            title: String::new(),
        };
        if let Some(path) = path {
            this.open(path, cx);
        }
        this
    }

    /// Shows `path` (a photo or a folder) and everything next to it.
    pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let path = std::path::absolute(&path).unwrap_or(path);
        self.drop_all_images();
        self.errors.clear();
        self.thumb_errors.clear();
        self.rotations.clear();
        self.notice = None;
        // Show the photo right away; the folder listing arrives a moment later.
        let files = if path.is_dir() { Vec::new() } else { vec![path.clone()] };
        self.set_listing(files, 0);
        self.reset_view();
        self.request_work();
        cx.notify();

        let listing = cx.background_spawn(async move { catalog::listing_for(&path) });
        self.scan = Some(cx.spawn(async move |this, cx| {
            let (files, index) = listing.await;
            this.update(cx, |this, cx| {
                this.set_listing(files, index);
                this.request_work();
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_listing(&mut self, files: Vec<PathBuf>, index: usize) {
        self.positions = files
            .iter()
            .enumerate()
            .map(|(i, p)| (p.clone(), i))
            .collect();
        self.files = files;
        self.index = index.min(self.files.len().saturating_sub(1));
    }

    fn current(&self) -> Option<&PathBuf> {
        self.files.get(self.index)
    }

    // ---------------------------------------------------------------- loading

    fn on_loaded(&mut self, loaded: Loaded) {
        let Loaded { path, kind, result } = loaded;
        let Some(&index) = self.positions.get(&path) else {
            return;
        };
        let mut decoded = match result {
            Ok(decoded) => decoded,
            Err(error) => {
                match kind {
                    Kind::Thumb => {
                        self.thumb_errors.insert(path);
                    }
                    Kind::Preview | Kind::Full => {
                        self.errors.insert(path, error);
                    }
                }
                return;
            }
        };
        if !self.keeps(index, kind) {
            return;
        }
        self.apply_rotation(&path, &mut decoded);
        let replaced = match kind {
            Kind::Thumb => self.thumbs.insert(path, decoded),
            Kind::Preview => self.previews.insert(path, decoded),
            Kind::Full => self.full.replace((path, decoded)).map(|(_, d)| d),
        };
        if let Some(old) = replaced {
            self.pending_drops.push(old.image);
        }
    }

    /// Whether a decode of `kind` for the photo at `index` is worth holding.
    fn keeps(&self, index: usize, kind: Kind) -> bool {
        let i = self.index;
        match kind {
            Kind::Full => index == i,
            Kind::Preview => {
                let (ahead, behind) = if self.direction >= 0 {
                    (PREVIEW_AHEAD, PREVIEW_BEHIND)
                } else {
                    (PREVIEW_BEHIND, PREVIEW_AHEAD)
                };
                index + behind >= i && index <= i + ahead
            }
            Kind::Thumb => index.abs_diff(i) <= THUMB_KEEP,
        }
    }

    fn evict(&mut self) {
        let index_of = |p: &PathBuf| self.positions.get(p).copied();
        let stale_previews: Vec<PathBuf> = self
            .previews
            .keys()
            .filter(|p| !index_of(p).is_some_and(|i| self.keeps(i, Kind::Preview)))
            .cloned()
            .collect();
        let stale_thumbs: Vec<PathBuf> = self
            .thumbs
            .keys()
            .filter(|p| !index_of(p).is_some_and(|i| self.keeps(i, Kind::Thumb)))
            .cloned()
            .collect();
        let mut dropped: Vec<Decoded> = Vec::new();
        for path in stale_previews {
            dropped.extend(self.previews.remove(&path));
        }
        for path in stale_thumbs {
            dropped.extend(self.thumbs.remove(&path));
        }
        let current = self.files.get(self.index);
        if self.full.as_ref().is_some_and(|(p, _)| Some(p) != current) {
            dropped.extend(self.full.take().map(|(_, d)| d));
        }
        self.pending_drops
            .extend(dropped.into_iter().map(|d| d.image));
    }

    fn drop_all_images(&mut self) {
        let images = self
            .previews
            .drain()
            .chain(self.thumbs.drain())
            .map(|(_, d)| d.image)
            .chain(self.full.take().map(|(_, d)| d.image));
        self.pending_drops.extend(images);
    }

    /// Tells the decoders what matters now, most important first.
    fn request_work(&mut self) {
        self.evict();
        let Some(current) = self.current().cloned() else {
            self.loader.set_queue(Vec::new());
            return;
        };
        let mut requests = Vec::new();
        let mut want = |this: &Self, path: &PathBuf, kind: Kind| {
            let missing = match kind {
                Kind::Thumb => {
                    !this.thumbs.contains_key(path) && !this.thumb_errors.contains(path)
                }
                Kind::Preview => {
                    !this.previews.contains_key(path) && !this.errors.contains_key(path)
                }
                Kind::Full => {
                    this.full.as_ref().is_none_or(|(p, _)| p != path)
                        && !this.errors.contains_key(path)
                }
            };
            if missing {
                requests.push(Request {
                    path: path.clone(),
                    kind,
                });
            }
        };

        if !self.previews.contains_key(&current) {
            // An embedded EXIF thumbnail shows up long before the real decode.
            want(self, &current, Kind::Thumb);
            want(self, &current, Kind::Preview);
        }
        if self.needs_full() {
            want(self, &current, Kind::Full);
        }
        let step = |d: usize, forward: bool| {
            let i = if forward {
                self.index.checked_add(d)
            } else {
                self.index.checked_sub(d)
            };
            i.and_then(|i| self.files.get(i))
        };
        let forward = self.direction >= 0;
        let mut neighbours = Vec::new();
        for d in 1..=PREVIEW_AHEAD {
            neighbours.extend(step(d, forward));
            if d <= PREVIEW_BEHIND {
                neighbours.extend(step(d, !forward));
            }
        }
        for path in neighbours {
            want(self, path, Kind::Preview);
        }
        let reach = self.strip_slots() / 2 + 8;
        for d in 0..=reach {
            for path in [step(d, true), step(d, false)].into_iter().flatten() {
                want(self, path, Kind::Thumb);
            }
        }
        self.loader.set_queue(requests);
    }

    /// Re-renders freshly decoded pixels of `path` to match the user's rotation.
    fn apply_rotation(&mut self, path: &Path, decoded: &mut Decoded) {
        let target = self.rotations.get(path).copied().unwrap_or(0);
        self.pending_drops.extend(rotate_to(decoded, target));
    }

    fn rotate(&mut self, quarter_turns: u8, cx: &mut Context<Self>) {
        let Some(path) = self.current().cloned() else {
            return;
        };
        let rotation = self.rotations.entry(path.clone()).or_default();
        *rotation = (*rotation + quarter_turns) % 4;
        let target = *rotation;

        let mut replaced = Vec::new();
        if let Some(d) = self.previews.get_mut(&path) {
            replaced.extend(rotate_to(d, target));
        }
        if let Some(d) = self.thumbs.get_mut(&path) {
            replaced.extend(rotate_to(d, target));
        }
        if let Some((p, d)) = self.full.as_mut()
            && *p == path
        {
            replaced.extend(rotate_to(d, target));
        }
        self.pending_drops.extend(replaced);
        // Turn the view with the photo around the window centre, keeping zoom,
        // so whatever was in the middle stays in the middle.
        for _ in 0..quarter_turns {
            self.offset = (-self.offset.1, self.offset.0);
        }
        self.clamp_offset();
        self.request_work();
        cx.notify();
    }

    // --------------------------------------------------------------- geometry

    fn stage(&self) -> Rect {
        let (top, bottom) = if self.show_chrome {
            (TOP_BAR, BOTTOM_BAR)
        } else {
            (0.0, 0.0)
        };
        Rect {
            x: 0.0,
            y: top,
            w: self.viewport.0,
            h: (self.viewport.1 - top - bottom).max(1.0),
        }
    }

    /// Photo size in photo pixels, after user rotation.
    fn photo_size(&self) -> Option<(f32, f32)> {
        let path = self.current()?;
        let d = self
            .previews
            .get(path)
            .or(self.full.as_ref().filter(|(p, _)| p == path).map(|(_, d)| d))
            .or(self.thumbs.get(path))?;
        let (w, h) = (d.width as f32, d.height as f32);
        let rotation = self.rotations.get(path).copied().unwrap_or(0);
        Some(if rotation % 2 == 1 { (h, w) } else { (w, h) })
    }

    /// Logical pixels per photo pixel that fits the photo on the stage.
    /// Small photos are enlarged only with the "enlarge small photos" setting.
    fn fit_scale(&self) -> f32 {
        let Some((w, h)) = self.photo_size() else {
            return 1.0;
        };
        let stage = self.stage();
        let fit = (stage.w / w).min(stage.h / h);
        if self.settings.enlarge_small {
            fit
        } else {
            fit.min(1.0 / self.scale_factor)
        }
    }

    fn scale(&self) -> f32 {
        self.zoom.unwrap_or_else(|| self.fit_scale())
    }

    fn actual_scale(&self) -> f32 {
        1.0 / self.scale_factor
    }

    /// Where the photo is drawn, in window coordinates, snapped to device pixels.
    fn photo_rect(&self) -> Option<Rect> {
        let (w, h) = self.photo_size()?;
        let stage = self.stage();
        let s = self.scale();
        let snap = |v: f32| (v * self.scale_factor).round() / self.scale_factor;
        let (w, h) = (snap(w * s).max(1.0), snap(h * s).max(1.0));
        Some(Rect {
            x: snap(stage.x + stage.w / 2.0 + self.offset.0 - w / 2.0),
            y: snap(stage.y + stage.h / 2.0 + self.offset.1 - h / 2.0),
            w,
            h,
        })
    }

    /// Zoomed in past "fit". Decides trackpad scroll (pan vs. swipe to the
    /// next photo) and the hand cursor; dragging works at any zoom.
    fn can_pan(&self) -> bool {
        self.photo_size().is_some() && self.scale() > self.fit_scale() * 1.0001
    }

    /// The photo can be moved at any zoom until one of its edges reaches the
    /// window centre, so every spot, corners included, can be brought to the
    /// middle. Nothing recentres it except an explicit "fit" (F, the fit
    /// button, double-click) or moving to another photo.
    fn clamp_offset(&mut self) {
        let Some((w, h)) = self.photo_size() else {
            self.offset = (0.0, 0.0);
            return;
        };
        let s = self.scale();
        let (max_x, max_y) = (w * s / 2.0, h * s / 2.0);
        self.offset = (
            self.offset.0.clamp(-max_x, max_x),
            self.offset.1.clamp(-max_y, max_y),
        );
    }

    /// Zooms to `target` keeping the photo point under `anchor` (window
    /// coordinates) in place. Zoom stops at "fit" and "100%" when crossing them.
    fn zoom_to(&mut self, target: f32, anchor: Option<(f32, f32)>) {
        if self.photo_size().is_none() {
            return;
        }
        let old = self.scale();
        let fit = self.fit_scale();
        let actual = self.actual_scale();
        let mut new = target.clamp(fit.min(actual) * 0.2, (32.0 / self.scale_factor).max(fit));
        for stop in [fit, actual] {
            let crossed = (old < stop && new > stop) || (old > stop && new < stop);
            if crossed && (old - stop).abs() > 1e-4 {
                new = stop;
                break;
            }
        }
        let stage = self.stage();
        let (ax, ay) = anchor.unwrap_or((stage.x + stage.w / 2.0, stage.y + stage.h / 2.0));
        let rel = (ax - stage.x - stage.w / 2.0, ay - stage.y - stage.h / 2.0);
        let k = new / old;
        self.offset = (
            rel.0 - (rel.0 - self.offset.0) * k,
            rel.1 - (rel.1 - self.offset.1) * k,
        );
        self.zoom = if (new - fit).abs() < 1e-4 { None } else { Some(new) };
        self.clamp_offset();
        self.request_work();
    }

    fn reset_view(&mut self) {
        self.zoom = None;
        self.offset = (0.0, 0.0);
        self.drag = None;
    }

    /// Zoomed past what the preview can show sharply.
    fn needs_full(&self) -> bool {
        let Some(path) = self.current() else {
            return false;
        };
        let Some(preview) = self.previews.get(path) else {
            return false;
        };
        !preview.full_res && self.scale() * self.scale_factor > preview.pixel_ratio() * 1.05
    }

    /// The sharpest pixels we have for the current photo at the current zoom.
    fn display_image(&self) -> Option<&Decoded> {
        let path = self.current()?;
        let preview = self.previews.get(path);
        let full = self.full.as_ref().filter(|(p, _)| p == path).map(|(_, d)| d);
        let device_scale = self.scale() * self.scale_factor;
        match (preview, full) {
            (Some(p), Some(f)) if device_scale > p.pixel_ratio() * 1.01 => Some(f),
            (Some(p), _) => Some(p),
            (None, Some(f)) => Some(f),
            (None, None) => self.thumbs.get(path),
        }
    }

    fn strip_slots(&self) -> usize {
        let width = (self.viewport.0 - 2.0 * SIDE_CLUSTER - (CELL_CURRENT - CELL)).max(CELL_CURRENT);
        let slots = (((width + CELL_GAP) / (CELL + CELL_GAP)).floor() as usize).max(1);
        if slots % 2 == 0 { slots - 1 } else { slots }
    }

    // ---------------------------------------------------------------- actions

    fn go_to(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.files.is_empty() {
            return;
        }
        let index = index.min(self.files.len() - 1);
        if index == self.index {
            return;
        }
        self.direction = if index > self.index { 1 } else { -1 };
        self.index = index;
        self.notice = None;
        self.menu = None;
        self.reset_view();
        self.request_work();
        cx.notify();
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let target = self.index as isize + delta;
        if target >= 0 {
            self.go_to(target as usize, cx);
        }
    }

    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        self.step(1, cx);
    }

    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, cx);
    }

    fn first(&mut self, _: &First, _: &mut Window, cx: &mut Context<Self>) {
        self.go_to(0, cx);
    }

    fn last(&mut self, _: &Last, _: &mut Window, cx: &mut Context<Self>) {
        self.go_to(self.files.len().saturating_sub(1), cx);
    }

    fn zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_to(self.scale() * ZOOM_STEP, None);
        cx.notify();
    }

    fn zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_to(self.scale() / ZOOM_STEP, None);
        cx.notify();
    }

    fn zoom_fit(&mut self, _: &ZoomFit, _: &mut Window, cx: &mut Context<Self>) {
        self.reset_view();
        self.request_work();
        cx.notify();
    }

    fn zoom_actual(&mut self, _: &ZoomActual, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_to(self.actual_scale(), None);
        cx.notify();
    }

    fn rotate_left(&mut self, _: &RotateLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.rotate(3, cx);
    }

    fn rotate_right(&mut self, _: &RotateRight, _: &mut Window, cx: &mut Context<Self>) {
        self.rotate(1, cx);
    }

    fn toggle_fullscreen(&mut self, _: &ToggleFullscreen, window: &mut Window, _: &mut Context<Self>) {
        window.toggle_fullscreen();
    }

    fn toggle_info(&mut self, _: &ToggleInfo, _: &mut Window, cx: &mut Context<Self>) {
        self.show_info = !self.show_info;
        self.show_settings &= !self.show_info;
        cx.notify();
    }

    fn toggle_settings(&mut self, _: &ToggleSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.show_settings = !self.show_settings;
        self.show_info &= !self.show_settings;
        cx.notify();
    }

    fn set_scaling(&mut self, scaling: Scaling, cx: &mut Context<Self>) {
        if self.settings.scaling == scaling {
            return;
        }
        self.settings.scaling = scaling;
        self.settings.save();
        decode::set_scaling(scaling);
        // Re-decode what's on screen with the new filter; thumbnails can stay.
        let stale = self
            .previews
            .drain()
            .map(|(_, d)| d.image)
            .chain(self.full.take().map(|(_, d)| d.image))
            .chain(self.pixel_view.take().map(|(_, image)| image));
        self.pending_drops.extend(stale);
        self.request_work();
        cx.notify();
    }

    fn set_enlarge_small(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.enlarge_small = on;
        self.settings.save();
        self.clamp_offset();
        self.request_work();
        cx.notify();
    }

    fn set_check_updates(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.check_updates = on;
        self.settings.save();
        if !on {
            self.update = None;
        }
        cx.notify();
    }

    fn toggle_chrome(&mut self, _: &ToggleChrome, _: &mut Window, cx: &mut Context<Self>) {
        self.show_chrome = !self.show_chrome;
        self.clamp_offset();
        cx.notify();
    }

    fn close(&mut self, _: &Close, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_settings {
            self.show_settings = false;
            cx.notify();
        } else if self.menu.take().is_some() {
            cx.notify();
        } else if window.is_fullscreen() {
            window.toggle_fullscreen();
        } else {
            cx.quit();
        }
    }

    fn start_update(&mut self, cx: &mut Context<Self>) {
        use crate::update::{self, Action};
        match update::action() {
            Action::Run(updater) => match update::start(&updater, self.current().map(PathBuf::as_path)) {
                Ok(()) => cx.quit(),
                Err(e) => self.notice = Some(format!("Couldn't start the update: {e:#}")),
            },
            Action::Copy(command) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(command));
                self.notice = Some("Update command copied: paste it into a terminal".into());
            }
            Action::Open(url) => cx.open_url(url),
        }
        cx.notify();
    }

    fn skip_update(&mut self, cx: &mut Context<Self>) {
        if let Some(version) = self.update.take() {
            crate::update::skip(&version);
        }
        cx.notify();
    }

    fn reveal_file(&mut self, _: &RevealFile, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.current() {
            cx.reveal_path(path);
        }
    }

    fn copy_image(&mut self, _: &CopyImage, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.current().cloned() else {
            return;
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let format = match ext.as_str() {
            "png" => Some(ImageFormat::Png),
            "jpg" | "jpeg" | "jpe" | "jfif" => Some(ImageFormat::Jpeg),
            "webp" => Some(ImageFormat::Webp),
            "gif" => Some(ImageFormat::Gif),
            "bmp" => Some(ImageFormat::Bmp),
            "tif" | "tiff" => Some(ImageFormat::Tiff),
            "svg" => Some(ImageFormat::Svg),
            _ => None,
        };
        // Formats the clipboard understands go over as the original file;
        // everything else (RAW, HEIC, PSD, ...) as a PNG of what's on screen.
        let item = match format.zip(std::fs::read(&path).ok()) {
            Some((format, bytes)) => gpui::ClipboardItem::new_image(&Image::from_bytes(format, bytes)),
            None => match self.display_image().and_then(|d| png_of(&d.image)) {
                Some(png) => gpui::ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, png)),
                None => {
                    self.notice = Some("Nothing to copy yet".into());
                    cx.notify();
                    return;
                }
            },
        };
        cx.write_to_clipboard(item);
        self.notice = Some("Copied image".into());
        cx.notify();
    }

    fn copy_path(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.current() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.display().to_string()));
            self.notice = Some("Copied file path".into());
            cx.notify();
        }
    }

    fn open_file(&mut self, _: &OpenFile, _: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| this.open(path, cx)).ok();
            }
        })
        .detach();
    }

    fn delete_file(&mut self, _: &DeleteFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.current().cloned() else {
            return;
        };
        let name = catalog::file_name(&path);
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Move “{name}” to the trash?"),
            None,
            &["Move to Trash", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let target = path.clone();
            let result = cx
                .background_spawn(async move { trash::delete(&target) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.remove_from_listing(&path),
                    Err(e) => this.notice = Some(format!("Couldn't delete {name}: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn remove_from_listing(&mut self, path: &Path) {
        let Some(&removed) = self.positions.get(path) else {
            return;
        };
        self.drop_cached(path);
        let mut files = std::mem::take(&mut self.files);
        files.remove(removed);
        // Stay on the same slot, which now holds the next photo.
        let index = if removed < self.index { self.index - 1 } else { self.index };
        self.set_listing(files, index);
        self.reset_view();
        self.request_work();
    }

    fn drop_cached(&mut self, path: &Path) {
        let images = [
            self.previews.remove(path),
            self.thumbs.remove(path),
            self.full.take_if(|(p, _)| p == path).map(|(_, d)| d),
        ];
        self.pending_drops
            .extend(images.into_iter().flatten().map(|d| d.image));
    }

    // ------------------------------------------------------------------ mouse

    fn stage_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let point = (f32::from(event.position.x), f32::from(event.position.y));
        let on_photo = self.photo_rect().is_some_and(|r| r.contains(point));
        if event.click_count == 2 && on_photo {
            // Double click flips between fit and 100% around the clicked spot.
            if self.zoom.is_none() && (self.fit_scale() - self.actual_scale()).abs() > 1e-4 {
                self.zoom_to(self.actual_scale(), Some(point));
            } else {
                self.reset_view();
                self.request_work();
            }
            self.drag = None;
            cx.notify();
            return;
        }
        self.drag = Some(Drag {
            start: point,
            offset: self.offset,
            on_photo,
            moved: false,
        });
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
        let (dx, dy) = (x - drag.start.0, y - drag.start.1);
        if dx.abs() + dy.abs() > 3.0 {
            drag.moved = true;
        }
        if drag.on_photo {
            self.offset = (drag.offset.0 + dx, drag.offset.1 + dy);
            self.clamp_offset();
            cx.notify();
        }
    }

    fn stage_mouse_up(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        // Like Picasa: a plain click on the dimmed background closes the viewer.
        if !drag.on_photo && !drag.moved && !self.files.is_empty() {
            self.close(&Close, window, cx);
        }
    }

    fn stage_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let anchor = Some((f32::from(event.position.x), f32::from(event.position.y)));
        match event.delta {
            ScrollDelta::Lines(delta) => {
                if delta.x.abs() > delta.y.abs() || event.modifiers.alt {
                    // Tilt wheel or Alt+wheel flips through photos.
                    let amount = if delta.x.abs() > delta.y.abs() { -delta.x } else { -delta.y };
                    self.step(amount.signum() as isize, cx);
                } else {
                    let notches = (delta.y / LINES_PER_NOTCH).clamp(-3.0, 3.0);
                    self.zoom_to(self.scale() * ZOOM_STEP.powf(notches), anchor);
                }
            }
            ScrollDelta::Pixels(delta) => {
                let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
                if matches!(event.touch_phase, TouchPhase::Started) {
                    self.swipe = 0.0;
                    self.swipe_done = false;
                }
                if event.modifiers.secondary() || event.modifiers.control {
                    self.zoom_to(self.scale() * (dy * 0.01).exp(), anchor);
                } else if self.can_pan() {
                    self.offset = (self.offset.0 + dx, self.offset.1 + dy);
                    self.clamp_offset();
                } else if dx.abs() > dy.abs() {
                    self.swipe += dx;
                    if !self.swipe_done && self.swipe.abs() > SWIPE_DISTANCE {
                        self.swipe_done = true;
                        self.step(if self.swipe < 0.0 { 1 } else { -1 }, cx);
                    }
                } else {
                    self.zoom_to(self.scale() * (dy * 0.01).exp(), anchor);
                }
                if matches!(event.touch_phase, TouchPhase::Ended) {
                    self.swipe = 0.0;
                    self.swipe_done = false;
                }
            }
        }
        cx.notify();
    }

    fn strip_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(20.0));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        let amount = if dx.abs() > dy.abs() { dx } else { dy };
        if amount.abs() > 0.5 {
            self.step(if amount < 0.0 { 1 } else { -1 }, cx);
        }
        cx.stop_propagation();
    }

    // ----------------------------------------------------------------- render

    fn render_stage(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let stage = self.stage();
        let mut content: Vec<AnyElement> = Vec::new();
        let dragging = self.drag.as_ref().is_some_and(|d| d.on_photo && d.moved);

        if let (Some(rect), Some(image)) = (self.photo_rect(), self.display_image()) {
            let (source, opaque) = (image.image.clone(), image.opaque);
            let mut frame = div()
                .absolute()
                .left(px(rect.x - stage.x))
                .top(px(rect.y - stage.y))
                .w(px(rect.w))
                .h(px(rect.h));
            if opaque {
                frame = frame.shadow_lg();
            }
            let pixel = (self.settings.scaling == Scaling::Pixel && source.frame_count() == 1)
                .then(|| self.pixel_view_for(rect, &source))
                .flatten();
            match pixel {
                // Hard-edged pixels: draw the screen-resolution crop 1:1.
                Some((visible, crop)) => {
                    content.push(frame.into_any_element());
                    content.push(
                        img(crop)
                            .absolute()
                            .left(px(visible.x - stage.x))
                            .top(px(visible.y - stage.y))
                            .w(px(visible.w))
                            .h(px(visible.h))
                            .into_any_element(),
                    );
                }
                None => content.push(
                    frame
                        .child(img(source).id(("photo", self.index)).size_full())
                        .into_any_element(),
                ),
            }
        } else if let Some(error) = self.current().and_then(|p| self.errors.get(p)) {
            let name = self.current().map(|p| catalog::file_name(p)).unwrap_or_default();
            content.push(
                centered()
                    .flex_col()
                    .gap_1()
                    .child(div().text_color(rgba(TEXT)).child(format!("Can't show {name}")))
                    .child(div().text_sm().text_color(rgba(MUTED)).child(error.clone()))
                    .into_any_element(),
            );
        } else if self.files.is_empty() && self.scan.is_some() {
            content.push(empty_state("No photos here", cx).into_any_element());
        } else if self.files.is_empty() {
            content.push(empty_state("Drop a photo here", cx).into_any_element());
        }

        if let Some(notice) = &self.notice {
            content.push(
                div()
                    .absolute()
                    .bottom(px(16.0))
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgba(0x000000b0))
                            .text_sm()
                            .text_color(rgba(TEXT))
                            .child(notice.clone()),
                    )
                    .into_any_element(),
            );
        }

        let cursor = if dragging {
            CursorStyle::ClosedHand
        } else if self.can_pan() {
            CursorStyle::OpenHand
        } else {
            CursorStyle::Arrow
        };
        div()
            .id("stage")
            .relative()
            .w_full()
            .h(px(stage.h))
            .overflow_hidden()
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::stage_mouse_down))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if this.current().is_some() {
                        this.menu = Some(event.position);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(MouseButton::Left, cx.listener(Self::stage_mouse_up))
            .on_scroll_wheel(cx.listener(Self::stage_scroll))
            .children(content)
    }

    fn render_top_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut title = div()
            .flex()
            .items_baseline()
            .gap_3()
            .overflow_hidden()
            .whitespace_nowrap();
        if let Some(path) = self.current().cloned() {
            title = title
                .child(div().text_color(rgba(TEXT)).child(catalog::file_name(&path)))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgba(MUTED))
                        .child(self.details_line(&path)),
                );
        } else {
            title = title.child(div().text_color(rgba(MUTED)).child("Vitreus"));
        }

        let drag_area = div()
            .id("title-drag")
            .flex_1()
            .h_full()
            .flex()
            .items_center()
            .pl(px(if cfg!(target_os = "macos") { 84.0 } else { 14.0 }))
            .child(title)
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    if cfg!(target_os = "macos") {
                        window.titlebar_double_click();
                    } else {
                        window.zoom_window();
                    }
                } else {
                    window.start_window_move();
                }
            });

        let settings_button = icon_button("settings", "settings", "Settings (Ctrl+,)").on_click(
            cx.listener(|this, _, window, cx| this.toggle_settings(&ToggleSettings, window, cx)),
        );

        let zoom = format!("{:.0}%", self.scale() * self.scale_factor * 100.0);
        let mut buttons = div()
            .flex()
            .items_center()
            .gap_1()
            .pr_1();
        if let Some(version) = self.update.clone() {
            buttons = buttons.child(self.render_update_pill(version, cx));
        }
        buttons = buttons
            .child(div().pr_2().text_sm().text_color(rgba(MUTED)).child(zoom))
            .child(
                icon_button("info", "info", "Photo info (I)")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_info(&ToggleInfo, window, cx))),
            )
            .child(settings_button)
            .child(
                icon_button("fullscreen", "fullscreen", "Full screen (F11)").on_click(
                    cx.listener(|this, _, window, cx| this.toggle_fullscreen(&ToggleFullscreen, window, cx)),
                ),
            );
        if !cfg!(target_os = "macos") {
            buttons = buttons
                .child(
                    icon_button("minimize", "minimize", "Minimize")
                        .window_control_area(WindowControlArea::Min)
                        .on_click(|_, window, _| window.minimize_window()),
                )
                .child(
                    icon_button("maximize", "maximize", "Maximize")
                        .window_control_area(WindowControlArea::Max)
                        .on_click(|_, window, _| window.zoom_window()),
                )
                .child(
                    icon_button("close", "close", "Close (Esc)")
                        .window_control_area(WindowControlArea::Close)
                        .hover(|s| s.bg(rgba(0xc42b1cff)))
                        .on_click(|_, _, cx| cx.quit()),
                );
        }

        div()
            .flex()
            .items_center()
            .h(px(TOP_BAR))
            .w_full()
            .bg(rgba(BAR))
            .child(drag_area)
            .child(buttons)
    }

    /// "Vitreus 0.2.0 is out · Update · Skip" in the title bar.
    fn render_update_pill(&self, version: String, cx: &mut Context<Self>) -> impl IntoElement {
        let label = crate::update::action().label();
        let pill_button = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .px_2()
                .py(px(2.0))
                .rounded_full()
                .text_xs()
                .cursor_pointer()
                .child(text)
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .mr_2()
            .pl_3()
            .pr_1()
            .py(px(3.0))
            .rounded_full()
            .bg(rgba(0x8fd3fb1a))
            .border_1()
            .border_color(rgba(0x8fd3fb55))
            .child(
                div()
                    .text_xs()
                    .text_color(rgba(ACCENT))
                    .child(format!("Vitreus {version} is out")),
            )
            .child(
                pill_button("update-now", label)
                    .bg(rgba(ACCENT))
                    .text_color(rgba(0x05060dff))
                    .hover(|s| s.opacity(0.85))
                    .on_click(cx.listener(|this, _, _, cx| this.start_update(cx))),
            )
            .child(
                pill_button("update-skip", "Skip")
                    .text_color(rgba(MUTED))
                    .hover(|s| s.text_color(rgba(TEXT)))
                    .tooltip(|_, cx| cx.new(|_| Tooltip("Skip this version; you'll hear about the next one")).into())
                    .on_click(cx.listener(|this, _, _, cx| this.skip_update(cx))),
            )
    }

    fn details_line(&mut self, path: &PathBuf) -> String {
        let mut parts = Vec::new();
        if !self.files.is_empty() {
            parts.push(format!("{} / {}", self.index + 1, self.files.len()));
        }
        if let Some((w, h)) = self.photo_size() {
            parts.push(format!("{w:.0} × {h:.0}"));
        }
        if self.file_size.as_ref().is_none_or(|(p, _)| p != path) {
            let size = std::fs::metadata(path).ok().map(|m| m.len());
            self.file_size = Some((path.clone(), size));
        }
        if let Some((_, Some(bytes))) = &self.file_size {
            parts.push(info::human_size(*bytes));
        }
        parts.join("    ")
    }

    fn render_bottom_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let left = div()
            .w(px(SIDE_CLUSTER))
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .child(
                icon_button("prev", "prev", "Previous (←)")
                    .on_click(cx.listener(|this, _, _, cx| this.step(-1, cx))),
            )
            .child(
                icon_button("next", "next", "Next (→)")
                    .on_click(cx.listener(|this, _, _, cx| this.step(1, cx))),
            )
            .child(div().w(px(12.0)))
            .child(
                icon_button("rotate-left", "rotate-left", "Rotate left (L)")
                    .on_click(cx.listener(|this, _, _, cx| this.rotate(3, cx))),
            )
            .child(
                icon_button("rotate-right", "rotate-right", "Rotate right (R)")
                    .on_click(cx.listener(|this, _, _, cx| this.rotate(1, cx))),
            );

        let right = div()
            .w(px(SIDE_CLUSTER))
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .child(
                icon_button("zoom-out", "zoom-out", "Zoom out (↓)")
                    .on_click(cx.listener(|this, _, window, cx| this.zoom_out(&ZoomOut, window, cx))),
            )
            .child(
                icon_button("fit", "fit", "Fit to window (F)")
                    .on_click(cx.listener(|this, _, window, cx| this.zoom_fit(&ZoomFit, window, cx))),
            )
            .child(
                icon_button("actual", "actual", "Actual size (A)").on_click(
                    cx.listener(|this, _, window, cx| this.zoom_actual(&ZoomActual, window, cx)),
                ),
            )
            .child(
                icon_button("zoom-in", "zoom-in", "Zoom in (↑)")
                    .on_click(cx.listener(|this, _, window, cx| this.zoom_in(&ZoomIn, window, cx))),
            )
            .child(div().w(px(12.0)))
            .child(
                icon_button("reveal", "folder", "Show in folder (W)").on_click(
                    cx.listener(|this, _, window, cx| this.reveal_file(&RevealFile, window, cx)),
                ),
            )
            .child(
                icon_button("delete", "trash", "Move to trash (Del)").on_click(
                    cx.listener(|this, _, window, cx| this.delete_file(&DeleteFile, window, cx)),
                ),
            );

        div()
            .flex()
            .items_center()
            .h(px(BOTTOM_BAR))
            .w_full()
            .bg(rgba(BAR))
            .child(left)
            .child(self.render_strip(cx))
            .child(right)
    }

    fn render_strip(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let half = self.strip_slots() / 2;
        let cells = (0..=2 * half).map(|slot| {
            let index = (self.index + slot).checked_sub(half);
            let path = index.and_then(|i| self.files.get(i));
            let (Some(index), Some(path)) = (index, path) else {
                return div().w(px(CELL)).h(px(CELL)).into_any_element();
            };
            let current = index == self.index;
            let size = if current { CELL_CURRENT } else { CELL };
            let mut cell = div()
                .id(("thumb", index))
                .flex_none()
                .size(px(size))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go_to(index, cx)));
            cell = if current {
                cell.border_2().border_color(rgba(ACCENT)).bg(rgba(0x8fd3fb1a))
            } else {
                cell.opacity(0.7).hover(|s| s.opacity(1.0))
            };
            let inner = size - 6.0;
            let child = match self.thumbs.get(path) {
                Some(thumb) => {
                    let pixels = thumb.image.size(0);
                    let (w, h) = (pixels.width.0 as f32, pixels.height.0 as f32);
                    let k = (inner / w).min(inner / h);
                    img(thumb.image.clone())
                        .w(px((w * k).round()))
                        .h(px((h * k).round()))
                        .into_any_element()
                }
                None => div()
                    .size(px(inner * 0.8))
                    .rounded(px(3.0))
                    .bg(rgba(0xffffff12))
                    .into_any_element(),
            };
            cell.child(child).into_any_element()
        });
        div()
            .id("strip")
            .min_w(px(0.0))
            .flex_1()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(CELL_GAP))
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(Self::strip_scroll))
            .children(cells)
    }

    fn render_info(&mut self) -> impl IntoElement {
        let path = self.current().cloned();
        let dims = self.photo_size().map(|(w, h)| (w as u32, h as u32));
        let stale = self
            .info
            .as_ref()
            .is_none_or(|(p, d, _)| Some(p) != path.as_ref() || *d != dims);
        if stale {
            self.info = path.map(|p| {
                let rows = info::describe(&p, dims);
                (p, dims, rows)
            });
        }
        let rows = self.info.as_ref().map(|(_, _, rows)| rows.clone()).unwrap_or_default();
        div()
            .absolute()
            .top(px(if self.show_chrome { TOP_BAR + 12.0 } else { 12.0 }))
            .right(px(12.0))
            .w(px(340.0))
            .p_3()
            .rounded_lg()
            .bg(rgba(0x161616e6))
            .border_1()
            .border_color(rgba(0xffffff1a))
            .flex()
            .flex_col()
            .gap_1()
            .text_sm()
            .children(rows.into_iter().map(|(label, value)| {
                div()
                    .flex()
                    .gap_2()
                    .child(div().w(px(96.0)).flex_none().text_color(rgba(MUTED)).child(label))
                    .child(div().flex_1().min_w(px(0.0)).text_color(rgba(TEXT)).child(value))
            }))
    }
}

impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.pending_drops.drain(..) {
            window.drop_image(image).ok();
        }
        let viewport = window.viewport_size();
        let viewport = (f32::from(viewport.width), f32::from(viewport.height));
        let resized = viewport != self.viewport;
        self.viewport = viewport;
        self.scale_factor = window.scale_factor();
        self.clamp_offset();
        if resized {
            // Fit may now need more (or fewer) pixels, and the strip holds more cells.
            self.request_work();
        }

        let title = match self.current() {
            Some(path) => format!("{} - Vitreus", catalog::file_name(path)),
            None => "Vitreus".to_owned(),
        };
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }

        let mut root = div()
            .id("viewer")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgba(OVERLAY))
            .text_color(rgba(TEXT))
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::first))
            .on_action(cx.listener(Self::last))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_fit))
            .on_action(cx.listener(Self::zoom_actual))
            .on_action(cx.listener(Self::rotate_left))
            .on_action(cx.listener(Self::rotate_right))
            .on_action(cx.listener(Self::toggle_fullscreen))
            .on_action(cx.listener(Self::toggle_info))
            .on_action(cx.listener(Self::toggle_settings))
            .on_action(cx.listener(Self::toggle_chrome))
            .on_action(cx.listener(Self::delete_file))
            .on_action(cx.listener(Self::reveal_file))
            .on_action(cx.listener(Self::copy_image))
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::close))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, _, cx| this.step(-1, cx)),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _, _, cx| this.step(1, cx)),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if let Some(path) = paths.paths().first() {
                    this.open(path.clone(), cx);
                }
            }));

        if self.show_chrome {
            root = root.child(self.render_top_bar(cx));
        }
        root = root.child(self.render_stage(cx));
        if self.show_chrome {
            root = root.child(self.render_bottom_bar(cx));
        }
        if self.show_info && !self.files.is_empty() {
            root = root.child(self.render_info());
        }
        if self.show_settings {
            root = root.child(self.render_settings(cx));
        }
        if let Some(position) = self.menu {
            // A transparent catcher: any click outside the menu closes it
            // (and a right-click reopens it there) without reaching the stage.
            root = root
                .child(
                    div()
                        .id("menu-catcher")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .occlude()
                        .on_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.menu = (event.button == MouseButton::Right).then_some(event.position);
                            cx.notify();
                        })),
                )
                .child(deferred(
                    anchored()
                        .position(position)
                        .snap_to_window_with_margin(px(8.0))
                        .child(self.render_menu(cx)),
                ));
        }
        root
    }
}

fn centered() -> gpui::Div {
    div().size_full().flex().items_center().justify_center()
}

fn empty_state(message: &'static str, cx: &mut Context<Viewer>) -> impl IntoElement {
    centered().child(
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .child(
                img(ImageSource::Resource(Resource::Embedded("brand/mark.png".into())))
                    .w(px(88.0))
                    .h(px(88.0)),
            )
            .child(div().text_color(rgba(MUTED)).child(message))
            .child(
                div()
                    .id("open")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgba(0xffffff1f))
                    .hover(|s| s.bg(rgba(0xffffff33)))
                    .cursor_pointer()
                    .child(svg().path("icons/folder.svg").size(px(16.0)).text_color(rgba(TEXT)))
                    .child("Open…")
                    .on_click(cx.listener(|this, _, window, cx| this.open_file(&OpenFile, window, cx))),
            ),
    )
}

fn icon_button(id: &'static str, icon: &'static str, tooltip: &'static str) -> Stateful<gpui::Div> {
    let group = SharedString::from(format!("btn-{id}"));
    div()
        .id(id)
        .group(group.clone())
        .flex_none()
        .size(px(34.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .hover(|s| s.bg(rgba(0xffffff1f)))
        .tooltip(move |_, cx| cx.new(|_| Tooltip(tooltip)).into())
        .child(
            svg()
                .path(format!("icons/{icon}.svg"))
                .size(px(18.0))
                .text_color(rgba(ICON))
                .group_hover(group, |s| s.text_color(Hsla::white())),
        )
}

struct Tooltip(&'static str);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgba(0x202020f2))
            .border_1()
            .border_color(rgba(0xffffff1f))
            .text_xs()
            .text_color(rgba(TEXT))
            .child(self.0)
    }
}

/// What the pixel-perfect crop was made from; any change means re-render it.
#[derive(Clone, Copy, PartialEq)]
struct PixelKey {
    source: usize,
    photo: [u32; 4],
    visible: [i32; 4],
}

impl Viewer {
    /// Pixel-perfect mode: the part of the photo inside the stage, resampled
    /// with nearest neighbour at the screen's exact device resolution, so the
    /// GPU only has to copy it. Returns where to draw it and the image.
    fn pixel_view_for(&mut self, photo: Rect, source: &Arc<RenderImage>) -> Option<(Rect, Arc<RenderImage>)> {
        let sf = self.scale_factor;
        let stage = self.stage();
        let x0 = (photo.x.max(stage.x) * sf).round() as i32;
        let y0 = (photo.y.max(stage.y) * sf).round() as i32;
        let x1 = ((photo.x + photo.w).min(stage.x + stage.w) * sf).round() as i32;
        let y1 = ((photo.y + photo.h).min(stage.y + stage.h) * sf).round() as i32;
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let visible = Rect {
            x: x0 as f32 / sf,
            y: y0 as f32 / sf,
            w: (x1 - x0) as f32 / sf,
            h: (y1 - y0) as f32 / sf,
        };
        let key = PixelKey {
            source: Arc::as_ptr(source) as usize,
            photo: [photo.x.to_bits(), photo.y.to_bits(), photo.w.to_bits(), photo.h.to_bits()],
            visible: [x0, y0, x1, y1],
        };
        if let Some((cached, crop)) = &self.pixel_view
            && *cached == key
        {
            return Some((visible, crop.clone()));
        }

        let size = source.size(0);
        let (sw, sh) = (size.width.0.max(1) as usize, size.height.0.max(1) as usize);
        let bytes = source.as_bytes(0)?;
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let (px0, py0, pw, ph) = (photo.x * sf, photo.y * sf, photo.w * sf, photo.h * sf);
        // Source column for every device column, sampled at pixel centres.
        let columns: Vec<usize> = (0..w)
            .map(|i| {
                let at = x0 as f32 + i as f32 + 0.5;
                (((at - px0) / pw * sw as f32) as usize).min(sw - 1)
            })
            .collect();
        let mut out = vec![0u8; w * h * 4];
        for (j, row) in out.chunks_exact_mut(w * 4).enumerate() {
            let at = y0 as f32 + j as f32 + 0.5;
            let sy = (((at - py0) / ph * sh as f32) as usize).min(sh - 1);
            let src = &bytes[sy * sw * 4..(sy + 1) * sw * 4];
            for (dst, &sx) in row.chunks_exact_mut(4).zip(&columns) {
                dst.copy_from_slice(&src[sx * 4..sx * 4 + 4]);
            }
        }
        let frame = ::image::Frame::new(::image::RgbaImage::from_raw(w as u32, h as u32, out)?);
        let crop = Arc::new(RenderImage::new(vec![frame]));
        if let Some((_, old)) = self.pixel_view.replace((key, crop.clone())) {
            self.pending_drops.push(old);
        }
        Some((visible, crop))
    }

    fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.settings.scaling;
        let segments = Scaling::ALL.into_iter().map(|scaling| {
            let selected = scaling == current;
            let segment = div()
                .id(scaling.label())
                .flex_1()
                .py_1()
                .text_center()
                .text_sm()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.set_scaling(scaling, cx)))
                .child(scaling.label());
            if selected {
                segment.bg(rgba(ACCENT)).text_color(rgba(0x071629ff))
            } else {
                segment.text_color(rgba(TEXT)).hover(|s| s.bg(rgba(0xffffff14)))
            }
        });
        let hint = match current {
            Scaling::Smooth => "Bicubic. Soft and natural, the classic photo look.",
            Scaling::Sharp => "Lanczos. Crisper detail when big photos are shrunk to fit.",
            Scaling::Pixel => "Nearest neighbour. Every pixel a hard-edged square: pixel art, screenshots, pixel peeping.",
        };
        let enlarge = self.settings.enlarge_small;
        let updates = self.settings.check_updates;
        div()
            .id("settings")
            .occlude()
            .absolute()
            .top(px(if self.show_chrome { TOP_BAR + 12.0 } else { 12.0 }))
            .right(px(12.0))
            .w(px(360.0))
            .p_4()
            .bg(rgba(0x0f2740f7))
            .border_1()
            .border_color(rgba(0x8fd3fb40))
            .flex()
            .flex_col()
            .gap_3()
            .text_color(rgba(TEXT))
            .child(div().text_color(rgba(ACCENT)).child("Settings"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().child("Scaling"))
                    .child(
                        div()
                            .flex()
                            .border_1()
                            .border_color(rgba(0xffffff26))
                            .children(segments),
                    )
                    .child(div().text_xs().text_color(rgba(MUTED)).child(hint)),
            )
            .child(toggle_row(
                "enlarge-small",
                "Enlarge small photos to fit",
                enlarge,
                cx.listener(move |this, _, _, cx| this.set_enlarge_small(!enlarge, cx)),
            ))
            .child(toggle_row(
                "check-updates",
                "Check for updates",
                updates,
                cx.listener(move |this, _, _, cx| this.set_check_updates(!updates, cx)),
            ))
    }
}

/// A label with a square on/off switch.
fn toggle_row(
    id: &'static str,
    label: &'static str,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let track = div()
        .w(px(36.0))
        .h(px(18.0))
        .p(px(2.0))
        .flex()
        .bg(rgba(if on { ACCENT } else { 0xffffff26 }))
        .when(on, |track| track.justify_end())
        .child(div().size(px(14.0)).bg(rgba(if on { 0x071629ff } else { 0xffffffcc })));
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .text_sm()
        .cursor_pointer()
        .on_click(on_click)
        .child(label)
        .child(track)
}

type MenuAction = fn(&mut Viewer, &mut Window, &mut Context<Viewer>);

impl Viewer {
    fn render_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ctrl = |key: &str| {
            if cfg!(target_os = "macos") { format!("⌘{key}") } else { format!("Ctrl+{key}") }
        };
        let delete_key = if cfg!(target_os = "macos") { "⌘⌫".to_owned() } else { "Del".to_owned() };
        let items: Vec<Option<(&'static str, String, MenuAction)>> = vec![
            Some(("Copy image", ctrl("C"), |this, w, cx| this.copy_image(&CopyImage, w, cx))),
            Some(("Copy file path", String::new(), |this, _, cx| this.copy_path(cx))),
            None,
            Some(("Rotate left", "L".into(), |this, _, cx| this.rotate(3, cx))),
            Some(("Rotate right", "R".into(), |this, _, cx| this.rotate(1, cx))),
            Some(("Fit to window", "F".into(), |this, w, cx| this.zoom_fit(&ZoomFit, w, cx))),
            Some(("Actual size", "A".into(), |this, w, cx| this.zoom_actual(&ZoomActual, w, cx))),
            None,
            Some(("Show in folder", "W".into(), |this, w, cx| this.reveal_file(&RevealFile, w, cx))),
            Some(("Photo info", "I".into(), |this, w, cx| this.toggle_info(&ToggleInfo, w, cx))),
            None,
            Some(("Move to trash", delete_key, |this, w, cx| this.delete_file(&DeleteFile, w, cx))),
        ];
        div()
            .id("context-menu")
            .occlude()
            .min_w(px(230.0))
            .py_1()
            .bg(rgba(0x0f2740f7))
            .border_1()
            .border_color(rgba(0x8fd3fb40))
            .shadow_lg()
            .text_sm()
            .text_color(rgba(TEXT))
            .children(items.into_iter().enumerate().map(|(i, item)| match item {
                None => div().my_1().h(px(1.0)).bg(rgba(0xffffff1a)).into_any_element(),
                Some((label, keys, action)) => div()
                    .id(("menu-item", i))
                    .flex()
                    .justify_between()
                    .gap_6()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(0x8fd3fb2e)))
                    .child(label)
                    .child(div().text_color(rgba(MUTED)).child(keys))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        action(this, window, cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            }))
    }
}

/// Encodes the first frame of a decoded (BGRA) image as PNG for the clipboard.
fn png_of(image: &RenderImage) -> Option<Vec<u8>> {
    let size = image.size(0);
    let mut rgba = image.as_bytes(0)?.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buffer = ::image::RgbaImage::from_raw(size.width.0 as u32, size.height.0 as u32, rgba)?;
    let mut png = std::io::Cursor::new(Vec::new());
    buffer.write_to(&mut png, ::image::ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

/// Rotates `decoded` to `target` quarter turns; returns the image it replaced.
fn rotate_to(decoded: &mut Decoded, target: u8) -> Option<Arc<RenderImage>> {
    if decoded.rotation == target {
        return None;
    }
    let turns = (target + 4 - decoded.rotation) % 4;
    let rotated = decode::rotate(&decoded.image, turns);
    decoded.rotation = target;
    Some(std::mem::replace(&mut decoded.image, rotated))
}
