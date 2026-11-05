//! Feeding an animation to the model: hand ffmpeg the video, take the frames it writes, cut them one by
//! one, and say how far along we are.
//!
//! Nothing here knows about ONNX or about the UI. It takes a video, a folder and a function that cuts one
//! frame, which means the whole batch can be tested with a fake cutter and no model at all.

use std::path::{Path, PathBuf};
use std::process::Command;

/// How many frames are in this video, if ffprobe can say. `nb_frames` comes from the container header and
/// is free when it is there; otherwise the duration and frame rate are multiplied, which is close enough to
/// draw an honest progress bar with.
pub fn probe_frames(video: &str) -> Option<u64> {
    let read = |entries: &str| -> Option<String> {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                entries,
                "-of",
                "csv=p=0",
                video,
            ])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|text| !text.is_empty())
    };

    if let Some(count) = read("stream=nb_frames")
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|count| *count > 0)
    {
        return Some(count);
    }
    let duration: f64 = read("format=duration")?.parse().ok()?;
    let rate = read("stream=r_frame_rate")?;
    let (top, bottom) = rate.split_once('/')?;
    let fps = top.parse::<f64>().ok()? / bottom.parse::<f64>().ok()?;
    let estimate = (duration * fps).round();
    (estimate > 0.0).then_some(estimate as u64)
}

/// Write every frame of the video into `into` as an RGBA png, numbered. Returns the count.
///
/// The frames keep their alpha: a png sequence is the one output where "the background is gone" is
/// representable, and re-encoding straight to a gif here would throw that away before the model has run.
pub fn extract_frames(video: &str, into: &Path) -> Result<u64, String> {
    std::fs::create_dir_all(into).map_err(|error| format!("{}: {error}", into.display()))?;
    let pattern = into.join("frame_%05d.png");
    let output = Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-i",
            video,
            // One file per frame, and no frame rate guessing: the video's own timing is what we want.
            "-vsync",
            "0",
            "-pix_fmt",
            "rgba",
            pattern.to_str().unwrap_or_default(),
        ])
        .output()
        .map_err(|error| format!("ffmpeg is not runnable: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let last = detail.lines().last().unwrap_or("ffmpeg failed").trim();
        return Err(last.to_string());
    }
    Ok(count_frames(into))
}

/// The frame files in a folder, in the order ffmpeg numbered them.
pub fn frames_in(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut frames: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
        })
        .collect();
    frames.sort();
    frames
}

fn count_frames(directory: &Path) -> u64 {
    frames_in(directory).len() as u64
}

/// What the batch has finished so far.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub done: u64,
    pub total: Option<u64>,
}

/// Cut every frame in `input`, writing the results to `output` under the same names.
///
/// `cut` is given one frame and returns the cut-out frame, so a caller can pass the model — or, in a test,
/// a function that returns its argument. `report` is called after each frame and returns whether to keep
/// going, which is the whole of the cancel button: no flag, no channel, just a boolean.
pub fn cut_animation(
    input: &Path,
    output: &Path,
    total: Option<u64>,
    mut cut: impl FnMut(&image::RgbaImage) -> Result<image::RgbaImage, String>,
    mut report: impl FnMut(Progress) -> bool,
) -> Result<u64, String> {
    let frames = frames_in(input);
    if frames.is_empty() {
        return Err(format!("no frames in {}", input.display()));
    }
    std::fs::create_dir_all(output).map_err(|error| format!("{}: {error}", output.display()))?;
    let total = total
        .filter(|count| *count > 0)
        .or(Some(frames.len() as u64));

    let mut done = 0u64;
    for frame in &frames {
        let image = image::open(frame)
            .map_err(|error| format!("{}: {error}", frame.display()))?
            .to_rgba8();
        let cut_frame = cut(&image)?;
        let name = frame.file_name().unwrap_or_default();
        let target = output.join(name);
        cut_frame
            .save(&target)
            .map_err(|error| format!("{}: {error}", target.display()))?;
        done += 1;
        if !report(Progress { done, total }) {
            break;
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A short animation made on the spot, so these tests need no sample files.
    fn make_gif(directory: &Path, frames: u32) -> Option<PathBuf> {
        std::fs::create_dir_all(directory).ok()?;
        let gif = directory.join("test.gif");
        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!(
                    "testsrc=duration={}&size=64x48:rate=10",
                    frames as f64 / 10.0
                ),
                "-frames:v",
                &frames.to_string(),
                gif.to_str()?,
            ])
            .status()
            .ok()?;
        status.success().then_some(gif)
    }

    #[test]
    fn frames_come_back_in_the_order_they_were_written() {
        let directory = std::env::temp_dir().join("anima-batch-order");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        for name in ["frame_00010.png", "frame_00002.png", "frame_00001.png"] {
            image::RgbaImage::new(2, 2)
                .save(directory.join(name))
                .unwrap();
        }
        let names: Vec<String> = frames_in(&directory)
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            ["frame_00001.png", "frame_00002.png", "frame_00010.png"]
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_batch_reports_progress_and_stops_when_told_to() {
        let directory = std::env::temp_dir().join("anima-batch-cancel");
        let input = directory.join("in");
        let output = directory.join("out");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&input).unwrap();
        for index in 1..=4 {
            image::RgbaImage::new(4, 4)
                .save(input.join(format!("frame_{index:05}.png")))
                .unwrap();
        }

        // A cutter that adds nothing, which is all this test needs: the batching is what is under test.
        let mut seen = Vec::new();
        let done = cut_animation(
            &input,
            &output,
            Some(4),
            |frame| Ok(frame.clone()),
            |progress| {
                seen.push((progress.done, progress.total));
                // Stop after the second frame.
                progress.done < 2
            },
        )
        .expect("batch");
        assert_eq!(done, 2, "the batch should stop when the report says stop");
        assert_eq!(seen, [(1, Some(4)), (2, Some(4))]);
        assert_eq!(
            frames_in(&output).len(),
            2,
            "only the finished frames are written"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_frame_the_cutter_refuses_stops_the_batch_with_a_sentence() {
        let directory = std::env::temp_dir().join("anima-batch-error");
        let input = directory.join("in");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&input).unwrap();
        image::RgbaImage::new(4, 4)
            .save(input.join("frame_00001.png"))
            .unwrap();
        let error = cut_animation(
            &input,
            &directory.join("out"),
            None,
            |_| Err("the model said no".to_string()),
            |_| true,
        )
        .unwrap_err();
        assert!(error.contains("the model said no"), "{error}");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_empty_folder_is_an_error_rather_than_a_silent_success() {
        let directory = std::env::temp_dir().join("anima-batch-empty");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let error = cut_animation(
            &directory,
            &directory.join("out"),
            None,
            |f| Ok(f.clone()),
            |_| true,
        )
        .unwrap_err();
        assert!(error.contains("no frames"), "{error}");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn ffmpeg_gives_up_the_frames_and_their_count() {
        let directory = std::env::temp_dir().join("anima-batch-extract");
        let _ = std::fs::remove_dir_all(&directory);
        let Some(gif) = make_gif(&directory, 5) else {
            eprintln!("no ffmpeg here, skipping");
            return;
        };
        assert_eq!(probe_frames(gif.to_str().unwrap()), Some(5));
        let count =
            extract_frames(gif.to_str().unwrap(), &directory.join("frames")).expect("extract");
        assert_eq!(count, 5);
        assert_eq!(frames_in(&directory.join("frames")).len(), 5);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
