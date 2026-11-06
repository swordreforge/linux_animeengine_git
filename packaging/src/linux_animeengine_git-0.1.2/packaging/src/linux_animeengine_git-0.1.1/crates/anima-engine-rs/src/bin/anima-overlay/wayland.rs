//! The Wayland side: the connection, the layer surface, the state the event loop drives, and the control
//! channel the tray and the portal command it through.
//!
//! Moved out of `main` verbatim. It takes the whole `Config` because everything it needs is in there — the
//! body below still reads the same names, bound from a reference at the top — and it returns the four things
//! `main` still touches afterwards.

use crate::*;
use anima_engine_rs::log;
use smithay_client_toolkit::reexports::client::globals::GlobalList;

/// Prints every global the compositor announced. This is for sandboxed runs — a Flatpak that does not offer
/// a global the overlay needs — where the only useful question is what the compositor actually offered.
fn report_globals(globals: &GlobalList) {
    let mut offered = globals.contents().clone_list();
    offered.sort_by(|left, right| left.interface.cmp(&right.interface));
    for global in &offered {
        log::event(
            log::Level::Debug,
            "overlay.wayland.global",
            &[
                ("interface", &global.interface),
                ("version", &global.version),
                ("name", &global.name),
            ],
        );
    }
    log::event(
        log::Level::Debug,
        "overlay.wayland.globals",
        &[("count", &offered.len())],
    );
}

pub(super) struct Wayland {
    pub conn: Connection,
    pub event_queue: EventQueue<OverlayState>,
    pub state: OverlayState,
    pub control: ControlState,
    pub control_mtime: Option<SystemTime>,
}

impl Wayland {
    pub(super) fn connect(config: &Config) -> Result<Wayland, Box<dyn std::error::Error>> {
        // The same names the body used when it lived in `main`, read from the config it now receives. The
        // values that are not `Copy` are cloned here so that nothing below this line had to change.
        let (width, height) = (config.width, config.height);
        let (pos_x, pos_y) = (config.pos_x, config.pos_y);
        let (initial_scale, initial_locked) = (config.initial_scale, config.initial_locked);
        let (initial_hover_hide, fps) = (config.initial_hover_hide, config.fps);
        let (meta_fps, always_on_top) = (config.meta_fps, config.always_on_top);
        let (disp_w, disp_h) = (config.disp_w, config.disp_h);
        let initial_mode = config.initial_mode;
        let orientation = config.orientation;
        let animation = Arc::clone(&config.loaded.animation);
        let crop = config.loaded.crop;
        let instance_key = config.instance_key.clone();
        let state_path = config.state_path.clone();
        let control_path = config.control_path.clone();
        let region_tuning = config.region_tuning;

        let conn = Connection::connect_to_env()?;
        let mut event_queue: EventQueue<OverlayState> = conn.new_event_queue();
        let qh = event_queue.handle();
        let (globals, _) = registry_queue_init::<OverlayState>(&conn)?;

        // Diagnostic for sandboxed runs (a Flatpak that does not offer a global the overlay
        // needs): say what the compositor actually announced, then continue as usual.
        if std::env::var_os("ANIMA_DUMP_GLOBALS").is_some() {
            report_globals(&globals);
        }

        let compositor: wl_compositor::WlCompositor = globals
            .bind(&qh, 1..=4, ())
            .map_err(|err| format!("binding wl_compositor: {err}"))?;
        let layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 = globals
            .bind(&qh, 1..=4, ())
            .map_err(|err| format!("binding zwlr_layer_shell_v1: {err}"))?;
        let seat: wl_seat::WlSeat = globals
            .bind(&qh, 1..=9, ())
            .map_err(|err| format!("binding wl_seat: {err}"))?;
        let outputs = OutputState::new(&globals, &qh);

        let surface = compositor.create_surface(&qh, ());
        let layer = if always_on_top {
            zwlr_layer_shell_v1::Layer::Overlay
        } else {
            zwlr_layer_shell_v1::Layer::Bottom
        };
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            layer,
            "anima-overlay".to_string(),
            &qh,
            (),
        );
        let scaled = |value: u32| (u64::from(value) * u64::from(initial_scale) / 100).max(1) as u32;
        layer_surface.set_size(scaled(disp_w), scaled(disp_h));
        layer_surface
            .set_anchor(zwlr_layer_surface_v1::Anchor::Top | zwlr_layer_surface_v1::Anchor::Left);
        layer_surface
            .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        layer_surface.set_margin(pos_y, 0, 0, pos_x);
        // Receive input so we can drag on opaque pixels. The empty region toggled
        // by the tray / portal makes the whole surface click-through.
        let empty_region = compositor.create_region(&qh, ());
        surface.set_input_region(None);
        surface.commit();

        let mut state = OverlayState {
            configured: None,
            closed: false,
            surface,
            layer_surface,
            _seat: seat,
            _pointer: None,
            pointer_pos: (0.0, 0.0),
            dragging: false,
            drag_start: (0.0, 0.0),
            margin_left: pos_x,
            margin_top: pos_y,
            frame_margin_left: pos_x,
            frame_margin_top: pos_y,
            animation: Arc::clone(&animation),
            frame_index: 0,
            native_size: (width, height),
            scale_percent: initial_scale,
            orientation,
            surface_size: (scaled(disp_w), scaled(disp_h)),
            desired_size: (scaled(disp_w), scaled(disp_h)),
            compositor: compositor.clone(),
            qh: qh.clone(),
            empty_region,
            input_mode: InputMode::Full,
            region: None,
            applied_rects: Vec::new(),
            instance_key,
            state_path,
            fps,
            meta_fps,
            crop,
            locked: initial_locked,
            hover_hide: initial_hover_hide,
            hidden: false,
            pointer_inside: false,
            pointer_over: false,
            outputs,
            active_output: None,
            last_output_size: None,
            region_dirty: false,
            visual_dirty: true,
            region_tuning,
            applied_step: 1,
            menu: menu::Menu::new(),
            control_path: control_path.clone(),
        };
        event_queue.roundtrip(&mut state)?;
        // Configures the input region for the restored mode (Opaque builds the
        // per-pixel region; PassThrough installs the empty one).
        apply_input_mode(&mut state, initial_mode);

        // --- control channel: tray + portal global shortcuts -------------------
        let mut control = ControlState::fresh();
        // Seed the runtime channel with the persisted display size so a CLI
        // read-modify-write cannot silently reset it.
        control.scale_percent = initial_scale;
        control.input_mode = initial_mode;
        control.locked = initial_locked;
        control.rotation = orientation.degrees();
        control.flip_h = orientation.flip_h;
        control.flip_v = orientation.flip_v;
        control.region_step = region_tuning.step.label();
        control.region_soft_rects = region_tuning.soft_rects;
        control.region_max_rects = region_tuning.max_rects;
        if let Err(err) = control.save(&control_path) {
            log::failure("overlay.control.init_failed", &err);
        }
        let control_mtime = std::fs::metadata(&control_path)
            .and_then(|m| m.modified())
            .ok();

        Ok(Wayland {
            conn,
            event_queue,
            state,
            control,
            control_mtime,
        })
    }
}
