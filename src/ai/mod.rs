//! AI background removal: one ONNX model, one session, and the same preprocessing rembg uses.
//!
//! Four modules, each small enough to replace whole: [`model`] fetches and verifies the weights, [`session`]
//! owns the ONNX Runtime session, [`preprocess`] turns a frame into the input the network expects and its
//! output back into an alpha mask, and [`batch`] walks a clip with progress and cancellation.
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
