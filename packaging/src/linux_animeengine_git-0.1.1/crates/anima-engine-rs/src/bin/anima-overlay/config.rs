//! Everything the overlay knows before it touches the compositor: the arguments, the animation, where its
//! state lives, and the settings restored from last time.
//!
//! Moved out of `main` verbatim. It is the one part of startup that reads nothing but files and environment
//! variables, which is why it was the safest slice to take on its own.

use crate::*;
use anima_engine_rs::log;

pub(super) struct Config {
    pub path: Option<PathBuf>,
    pub tray_wanted: bool,
    pub loaded: Loaded,
    pub width: u32,
    pub height: u32,
    pub instance_key: Option<String>,
    pub slug: String,
    pub label: String,
    pub state_path: PathBuf,
    pub control_path: PathBuf,
    pub region_tuning: RegionTuning,
    pub pos_x: i32,
    pub pos_y: i32,
    pub initial_scale: u32,
    pub always_on_top: bool,
    pub initial_mode: InputMode,
    pub initial_locked: bool,
    pub initial_hover_hide: bool,
    pub fps: u32,
    pub meta_fps: u32,
    pub orientation: Orientation,
    pub disp_w: u32,
    pub disp_h: u32,
}

impl Config {
    pub(super) fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let mut path: Option<PathBuf> = None;
        let mut tray_wanted = true;
        for arg in std::env::args().skip(1) {
            match arg.as_str() {
                "--no-tray" => tray_wanted = false,
                other => path = Some(PathBuf::from(other)),
            }
        }
        // Announce before decoding: a 1500x1125 animation takes seconds to load, and until the
        // marker exists the shepherd would think nothing is running and start a second copy.
        let early_control = ControlState::path_for(
            path.as_deref()
                .map(|path| anima_engine_rs::paths::canonical_path(&path.to_string_lossy()))
                .as_deref(),
        );
        match ControlState::announce(&early_control) {
            Ok(marker) => log::event(
                log::Level::Info,
                "overlay.liveness.announced",
                &[("marker", &marker.display())],
            ),
            Err(error) => log::failure("overlay.liveness.announce_failed", &error),
        }
        let Loaded {
            animation,
            blocks,
            crop,
            limits: decode_limits,
        } = load_animation(path.as_deref())?;
        let frame_count = animation.frames.len();
        if frame_count == 0 {
            return Err("animation has no frames".into());
        }
        let (width, height) = (animation.width.max(1), animation.height.max(1));

        // Instance identity for persistence: the canonical animation path. The
        // generated test pattern has no path, so it is never persisted.
        let instance_key = path
            .as_deref()
            .map(|path| anima_engine_rs::paths::canonical_path(&path.to_string_lossy()));
        // Identity for the per-instance control file and tray item.
        let slug = instance_key.as_deref().map_or_else(
            || "default".to_string(),
            anima_engine_rs::control::instance_slug,
        );
        let label = path
            .as_deref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "test pattern".to_string());
        let state_path = SecondaryState::user_path();
        let control_path = ControlState::path_for(instance_key.as_deref());
        let region_tuning = RegionTuning::from_env();
        log::event(
            log::Level::Info,
            "overlay.region_limits",
            &[("limits", &region_tuning.describe())],
        );
        let persisted = instance_key
            .as_deref()
            .and_then(|key| SecondaryState::load(&state_path).find_by_key(key).cloned());
        let (pos_x, pos_y) = persisted.as_ref().map_or(DEFAULT_POS, |instance| {
            (instance.x.max(0), instance.y.max(0))
        });
        let initial_scale = persisted
            .as_ref()
            .map_or(MAX_SCALE_PERCENT, |instance| instance.scale_percent)
            .clamp(MIN_SCALE_PERCENT, MAX_SCALE_PERCENT);
        // Saved positions are canvas margins; the surface sits `crop` pixels into the
        // canvas, so its margin is the saved one plus that (and minus it again when
        // persisting).
        let (pos_x, pos_y) = (
            pos_x + (crop.0 * initial_scale / 100) as i32,
            pos_y + (crop.1 * initial_scale / 100) as i32,
        );
        let always_on_top = persisted.as_ref().is_none_or(|i| i.always_on_top);
        let initial_mode = if persisted.as_ref().is_some_and(|i| i.click_through) {
            InputMode::PassThrough
        } else {
            InputMode::Opaque
        };
        let initial_locked = persisted.as_ref().is_some_and(|i| i.locked);
        let initial_hover_hide = persisted.as_ref().is_some_and(|i| i.hover_hide);
        let fps = persisted.as_ref().map_or(0, |instance| instance.fps);
        // `anim_meta.json` is a per-animation override; the instance's own `fps`
        // wins over it, and 0 in both means "follow the file's frame delays".
        let meta_fps =
            AnimMeta::load(&anima_engine_rs::paths::user_data_dir().join(ANIM_META_FILE))
                .fps_for(instance_key.as_deref().unwrap_or_default())
                .unwrap_or(0);
        let orientation = Orientation::from_degrees(
            persisted.as_ref().map_or(0, |i| i.rotation),
            persisted.as_ref().is_some_and(|i| i.flip_h),
            persisted.as_ref().is_some_and(|i| i.flip_v),
        );
        let (disp_w, disp_h) = orientation.size((width, height));
        if persisted.is_some() {
            log::event(
                log::Level::Info,
                "overlay.instance.restored",
                &[
                    ("x", &pos_x),
                    ("y", &pos_y),
                    ("scale", &initial_scale),
                    ("mode", &format!("{initial_mode:?}")),
                    ("locked", &initial_locked),
                    ("hover_hide", &initial_hover_hide),
                    ("fps", &fps),
                    ("meta_fps", &meta_fps),
                    ("quarters", &orientation.quarter_turns),
                    ("flip_h", &orientation.flip_h),
                    ("flip_v", &orientation.flip_v),
                ],
            );
        }

        Ok(Config {
            path,
            tray_wanted,
            loaded: Loaded {
                animation,
                blocks,
                crop,
                limits: decode_limits,
            },
            width,
            height,
            instance_key,
            slug,
            label,
            state_path,
            control_path,
            region_tuning,
            pos_x,
            pos_y,
            initial_scale,
            always_on_top,
            initial_mode,
            initial_locked,
            initial_hover_hide,
            fps,
            meta_fps,
            orientation,
            disp_w,
            disp_h,
        })
    }
}
