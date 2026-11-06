//! Shows what `log` writes, so the format can be seen without starting the overlay — which needs a Wayland
//! compositor and a GPU, and so is a poor first look at a log line.
//!
//! ```text
//! cargo run --example log_events
//! ANIMA_LOG=debug cargo run --example log_events
//! ANIMA_LOG=off   cargo run --example log_events
//! ```

use anima_engine_rs::log;

fn main() {
    log::event(
        log::Level::Info,
        "overlay.instance.restored",
        &[
            ("x", &100),
            ("y", &200),
            ("scale", &100),
            ("mode", &"Opaque"),
            ("locked", &false),
        ],
    );
    log::event(log::Level::Info, "overlay.menu.closed", &[]);
    log::failure("overlay.cache.write_failed", &"no space left on device");
    log::event(
        log::Level::Debug,
        "overlay.input.opaque",
        &[("rects", &12), ("step", &4)],
    );
    log::event(
        log::Level::Debug,
        "overlay.gpu",
        &[("adapter", &"Intel Arc Graphics")],
    );
}
