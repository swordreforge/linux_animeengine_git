//! The ONNX Runtime side: load the model, run it over a frame.

use crate::ai::model::{MODEL_SIZE, model_path};
use crate::ai::preprocess::{apply_mask, mask_from_output, preprocess};
use ort::session::Session;
use ort::value::Tensor;

/// The name the model declares for its input. It is not `input`, and feeding a name the model does not
/// have produced a session that ran happily and returned an empty mask, so it is checked.
const INPUT_NAME: &str = "img";

/// A loaded model. ONNX Runtime wants `&mut self` for `run`, so whoever owns this owns it exclusively.
pub struct Removal {
    session: Session,
}

impl Removal {
    /// Open the model with ONNX Runtime's own graph optimisation on. That is the ahead-of-time half of the
    /// split: fusing and constant folding happen once, here. Choosing kernels for this machine happens at
    /// the same moment and is the just-in-time half.
    pub fn load() -> Result<Self, String> {
        let environment = ort::init()
            .with_name("AnimaEngine")
            .build()
            .map_err(|error| format!("onnxruntime would not start: {error}"))?;
        let threads = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(4)
            .min(8);
        let path = model_path();
        let session = Session::builder(&environment)
            .map_err(|error| format!("onnxruntime: {error}"))?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(|error| format!("onnxruntime: {error}"))?
            .with_intra_threads(threads)
            .map_err(|error| format!("onnxruntime: {error}"))?
            .commit_from_file(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let names: Vec<String> = session
            .inputs()
            .iter()
            .map(|input| input.name().to_string())
            .collect();
        if !names.iter().any(|name| name == INPUT_NAME) {
            return Err(format!(
                "{} wants an input called {names:?}, and this code only feeds `{INPUT_NAME}`",
                path.display()
            ));
        }
        Ok(Self { session })
    }

    /// One frame in, one cut-out frame out. About three seconds on this CPU for a 288x218 source, which is
    /// slow enough that the caller should be a batch with a progress readout, not a single button.
    pub fn remove_background(
        &mut self,
        frame: &image::RgbaImage,
    ) -> Result<image::RgbaImage, String> {
        let pixels = preprocess(frame);
        let side = MODEL_SIZE as i64;
        let debug = std::env::var_os("ANIMA_AI_DEBUG").is_some();
        if debug {
            let low = pixels.iter().copied().fold(f32::MAX, f32::min);
            let high = pixels.iter().copied().fold(f32::MIN, f32::max);
            let mean = pixels.iter().map(|value| *value as f64).sum::<f64>() / pixels.len() as f64;
            println!("input {side}x{side}: min {low:.4} max {high:.4} mean {mean:.4}");
        }
        let input = Tensor::from_array(([1i64, 3, side, side], pixels))
            .map_err(|error| format!("building the input: {error}"))?;

        // The output comes back by index, because that is what rembg does: the first of several is the mask.
        let outputs = self
            .session
            .run(ort::inputs![INPUT_NAME => input])
            .map_err(|error| format!("the model refused the frame: {error}"))?;
        let (shape, values) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|error| format!("reading the mask: {error}"))?;

        let expected = (MODEL_SIZE * MODEL_SIZE) as usize;
        if values.len() < expected {
            return Err(format!(
                "the model returned {} mask values, expected {expected} — is this a {MODEL_SIZE} model?",
                values.len()
            ));
        }
        if debug {
            let plane = &values[..expected];
            let low = plane.iter().copied().fold(f32::MAX, f32::min);
            let high = plane.iter().copied().fold(f32::MIN, f32::max);
            let mean = plane.iter().map(|value| *value as f64).sum::<f64>() / expected as f64;
            println!("mask {shape:?}: min {low:.6} max {high:.6} mean {mean:.6}");
        }
        let mask = mask_from_output(&values[..expected], frame.width(), frame.height());
        Ok(apply_mask(frame, &mask))
    }
}

/// Whether ONNX Runtime can be loaded at all, so a card can say why it is unavailable rather than offering
/// a button that fails.
pub fn runtime_ready() -> bool {
    ort::init().with_name("AnimaEngine").build().is_ok()
}
