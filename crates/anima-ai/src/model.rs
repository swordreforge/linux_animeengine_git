//! Where the model comes from, and how to fetch it.

use md5::{Digest, Md5};
use std::io::Read;
use std::path::PathBuf;

/// The anime-tuned segmentation model. Still on this URL: rembg renamed the *session* to `dis-anime` and
/// kept the weights, so the file the Python app downloaded is the file this downloads.
pub const MODEL_URL: &str =
    "https://github.com/danielgatis/rembg/releases/download/v0.0.0/isnet-anime.onnx";
pub const MODEL_FILE: &str = "isnet-anime.onnx";
/// The square the network works at. Both the input resize and the mask it returns use this.
pub const MODEL_SIZE: u32 = 1024;
/// rembg's published md5. A download that does not match it is not the model, and saying so at download
/// time beats a session that fails to load or, worse, one that loads and segments badly.
pub const MODEL_MD5: &str = "6f184e756bb3bd901c8849220a83e38e";

/// Where the model lives once downloaded: the same place the Python app would have put it.
pub fn model_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("AnimaEngine").join("models").join(MODEL_FILE)
}

/// Whether the model is here, so the UI can offer to fetch it instead of failing on the first frame.
pub fn model_ready() -> bool {
    model_path().is_file()
}

/// Download the model, checksum it, and only then give it the real name. `progress` receives
/// `(received, total)` as it goes, so a 176 MB download can say something true about itself.
pub fn download_model(mut progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf, String> {
    let target = model_path();
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("{parent:?}: {error}"))?;
    }
    let partial = target.with_extension("onnx.part");

    let response = ureq::get(MODEL_URL)
        .call()
        .map_err(|error| format!("{MODEL_URL}: {error}"))?;
    let total: Option<u64> = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());

    let mut reader = response.into_body().into_reader();
    let mut file =
        std::fs::File::create(&partial).map_err(|error| format!("{partial:?}: {error}"))?;
    let mut hasher = Md5::new();
    let mut received = 0u64;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("reading the download: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        std::io::Write::write_all(&mut file, &buffer[..read])
            .map_err(|error| format!("writing {partial:?}: {error}"))?;
        received += read as u64;
        progress(received, total);
    }
    drop(file);

    // Hex by hand: this digest version's output array does not implement LowerHex.
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest != MODEL_MD5 {
        let _ = std::fs::remove_file(&partial);
        return Err(format!(
            "the model did not match its checksum (got {digest}, expected {MODEL_MD5}) — not keeping it"
        ));
    }
    std::fs::rename(&partial, &target).map_err(|error| format!("{target:?}: {error}"))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_lands_where_the_python_app_would_have_looked() {
        let path = model_path();
        assert!(
            path.ends_with("AnimaEngine/models/isnet-anime.onnx"),
            "{path:?}"
        );
    }
}
