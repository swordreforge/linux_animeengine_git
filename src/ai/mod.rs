//! AI background removal: one ONNX model, one session, and the same preprocessing rembg uses.
//!
//! Split into small files on purpose. This work was first written as one 500-line file and four editing
//! mistakes in a row left it unparseable, because every change had to match formatted text; a module small
//! enough to replace whole is a module that cannot be corrupted by an edit.
//!
//! The model is `isnet-anime`, the anime-tuned segmentation network the Python app was written for. rembg
//! renamed the session to `dis-anime` and kept the weights, which is the only reason the Python tool looks
//! broken: it asks for the old session name.

pub mod batch;
pub mod model;
pub mod preprocess;
pub mod session;

pub use batch::{Progress, cut_animation, extract_frames, frames_in, probe_frames};
pub use model::{MODEL_FILE, MODEL_SIZE, MODEL_URL, download_model, model_path, model_ready};
pub use preprocess::{apply_mask, mask_from_output, preprocess};
pub use session::{Removal, runtime_ready};
