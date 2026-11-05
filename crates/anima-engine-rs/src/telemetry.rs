//! What this process is doing to the machine: CPU, memory, GPU, and the threads doing the work.
//!
//! Everything here is read from `/proc/self`, which means no dependencies, no permissions and no polling
//! daemon — the kernel already counts it. The parsers are separated from the reading on purpose: `/proc`
//! text is awkward enough (a `comm` can contain a space and a closing parenthesis) that the awkward parts
//! deserve to be tested without needing a live process.
//!
//! GPU time comes from `drm-engine-*` in this process's `fdinfo`, which is the per-process number rather
//! than the per-card one. It is also the only one available here: there is no `intel_gpu_top` or `xpu-smi`
//! on this machine, and both would want more than the kernel does.

use std::collections::HashMap;
use std::time::Instant;

/// Clock ticks per second, which is how `/proc` counts CPU time. 100 on Linux since forever, but read the
/// real value rather than assuming it.
fn ticks_per_second() -> u64 {
    static TICKS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *TICKS.get_or_init(|| {
        let value = std::process::Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .ok()
            .and_then(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .trim()
                    .parse::<u64>()
                    .ok()
            });
        value.unwrap_or(100)
    })
}

/// One reading of a thread's CPU use.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadSample {
    pub name: String,
    pub cpu: f32,
}

/// One reading of the process.
#[derive(Debug, Clone, Default)]
pub struct Sample {
    /// Percent of one core, so a busy four-thread job reads about 400.
    pub cpu: f32,
    pub rss_bytes: u64,
    /// Percent of the GPU's time this process spent rendering, when the driver reports it.
    pub gpu: Option<f32>,
    pub threads: Vec<ThreadSample>,
}

/// The readings so far, oldest first, bounded. A chart needs the history and nothing else does, so the cap
/// is the only reason this type exists.
#[derive(Debug, Default)]
pub struct History {
    points: Vec<Sample>,
    limit: usize,
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self {
            points: Vec::new(),
            limit: limit.max(2),
        }
    }

    pub fn push(&mut self, sample: Sample) {
        if self.points.len() == self.limit {
            self.points.remove(0);
        }
        self.points.push(sample);
    }

    pub fn points(&self) -> &[Sample] {
        &self.points
    }

    pub fn latest(&self) -> Option<&Sample> {
        self.points.last()
    }

    /// The series a chart wants: (values, maximum seen). An empty history is (empty, 0.0) so a caller can
    /// draw an axis without special-casing.
    pub fn series(&self, pick: impl Fn(&Sample) -> Option<f32>) -> (Vec<f64>, f64) {
        let values: Vec<f64> = self
            .points
            .iter()
            .filter_map(|s| pick(s).map(f64::from))
            .collect();
        let peak = values.iter().copied().fold(0.0f64, f64::max);
        (values, peak)
    }
}

/// Keeps the previous reading so the next one can be a rate rather than a total.
#[derive(Debug, Default)]
pub struct Sampler {
    last: Option<Reading>,
}

#[derive(Debug)]
struct Reading {
    at: Instant,
    process_ticks: u64,
    thread_ticks: HashMap<u32, (String, u64)>,
    gpu_nanoseconds: u64,
}

impl Sampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read now and compare with the previous read. The first call returns rates of zero, because a rate
    /// needs two points and inventing one is worse than saying nothing.
    pub fn sample(&mut self) -> Sample {
        let now = Instant::now();
        let process_ticks = read_process_ticks();
        let thread_ticks = read_thread_ticks();
        let gpu_nanoseconds = read_gpu_nanoseconds();
        let rss_bytes = read_rss();

        let sample = match &self.last {
            Some(previous) => {
                let seconds = now.duration_since(previous.at).as_secs_f64().max(1e-6);
                let hz = ticks_per_second() as f64;
                let cpu =
                    (process_ticks.saturating_sub(previous.process_ticks)) as f64 / hz / seconds
                        * 100.0;
                let gpu = (gpu_nanoseconds > 0).then(|| {
                    (gpu_nanoseconds.saturating_sub(previous.gpu_nanoseconds)) as f64
                        / 1e9
                        / seconds
                        * 100.0
                });
                let mut threads: Vec<ThreadSample> = thread_ticks
                    .iter()
                    .map(|(id, (name, ticks))| {
                        let before = previous
                            .thread_ticks
                            .get(id)
                            .map(|(_, ticks)| *ticks)
                            .unwrap_or(*ticks);
                        ThreadSample {
                            name: name.clone(),
                            cpu: ((ticks.saturating_sub(before)) as f64 / hz / seconds * 100.0)
                                as f32,
                        }
                    })
                    .collect();
                // Busiest first: a thread list is read to find what is eating the machine, and the ones at
                // zero answer nothing.
                threads.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
                Sample {
                    cpu: cpu as f32,
                    rss_bytes,
                    gpu: gpu.map(|value| value as f32),
                    threads,
                }
            }
            None => Sample {
                rss_bytes,
                gpu: None,
                ..Sample::default()
            },
        };

        self.last = Some(Reading {
            at: now,
            process_ticks,
            thread_ticks,
            gpu_nanoseconds,
        });
        sample
    }
}

/// This process's own CPU time: `utime + stime`, in ticks. Fields 14 and 15 of `/proc/self/stat`, counted
/// after the command name, which is why the parentheses matter.
pub fn parse_process_ticks(stat: &str) -> Option<u64> {
    let after_name = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    // After the name, `state` is field 3, so utime is index 11 and stime index 12.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// `VmRSS` from `/proc/self/status`, in bytes.
pub fn parse_rss(status: &str) -> Option<u64> {
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kilobytes * 1024)
}

/// The GPU nanoseconds this process has been given, summed over every `drm-engine-*` line in its fdinfo.
pub fn parse_gpu_nanoseconds(contents: &[String]) -> u64 {
    contents
        .iter()
        .filter_map(|text| {
            let (key, value) = text.split_once(':')?;
            if !key.starts_with("drm-engine-") || key.contains("-capacity-") {
                return None;
            }
            value
                .trim()
                .trim_end_matches("ns")
                .trim()
                .parse::<u64>()
                .ok()
        })
        .sum()
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn read_process_ticks() -> u64 {
    read("/proc/self/stat")
        .as_deref()
        .and_then(parse_process_ticks)
        .unwrap_or(0)
}

fn read_rss() -> u64 {
    read("/proc/self/status")
        .as_deref()
        .and_then(parse_rss)
        .unwrap_or(0)
}

fn read_thread_ticks() -> HashMap<u32, (String, u64)> {
    let mut threads = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
        return threads;
    };
    for entry in entries.flatten() {
        let Ok(id) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Some(ticks) = read(&format!("/proc/self/task/{id}/stat"))
            .as_deref()
            .and_then(parse_process_ticks)
        else {
            continue;
        };
        let name = read(&format!("/proc/self/task/{id}/comm")).unwrap_or_default();
        threads.insert(id, (name.trim().to_string(), ticks));
    }
    threads
}

/// Every `drm-engine-*` counter the process is charged with. A driver that reports none gives zero, and
/// zero is also what a process that has not touched the GPU reports, which is why the caller shows `None`
/// rather than 0% when the total never moved.
fn read_gpu_nanoseconds() -> u64 {
    let Ok(entries) = std::fs::read_dir("/proc/self/fdinfo") else {
        return 0;
    };
    let contents: Vec<String> = entries
        .flatten()
        .filter_map(|entry| read(&entry.path().to_string_lossy()))
        .collect();
    parse_gpu_nanoseconds(&contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_ticks_survive_a_command_name_with_spaces_and_parens() {
        // A command name may contain both a space and a closing parenthesis — the kernel does not escape
        // it — so the fields start after the *last* one, not the first. The two numbers are distinct so a
        // wrong offset cannot pass by luck: after the name the state comes first, then ten more fields
        // before utime, so utime is the twelfth field and stime the thirteenth.
        let stat = "42 (a b) c) R 0 0 0 0 0 0 0 0 0 0 555 666 0 0 0 0 0 0 0";
        assert_eq!(parse_process_ticks(stat), Some(555 + 666));

        // And the same line with an ordinary name still works.
        let plain = "42 (anima-studio) R 0 0 0 0 0 0 0 0 0 0 7 5 0 0 0 0 0 0 0";
        assert_eq!(parse_process_ticks(plain), Some(12));
    }

    #[test]
    fn a_stat_without_a_command_name_is_not_panic() {
        assert_eq!(parse_process_ticks("nonsense"), None);
    }

    #[test]
    fn rss_is_read_in_bytes() {
        let status = "Name:\tanima-studio\nVmPeak:\t 1000 kB\nVmRSS:\t  2048 kB\nThreads:\t8\n";
        assert_eq!(parse_rss(status), Some(2048 * 1024));
    }

    #[test]
    fn gpu_nanoseconds_are_summed_across_engines_and_labels_ignored() {
        let lines = vec![
            "drm-total-system0:\t12\n".to_string(),
            "drm-engine-render:\t1000 ns\n".to_string(),
            "drm-engine-copy:\t500 ns\n".to_string(),
            "drm-engine-capacity-video:\t2\n".to_string(),
            "drm-client-id:\t7\n".to_string(),
            "pos:\t0\n".to_string(),
        ];
        // 1000 + 500: the capacity line is a count, not a duration, and must not be added.
        assert_eq!(parse_gpu_nanoseconds(&lines), 1500);
    }

    #[test]
    fn a_first_sample_says_nothing_rather_than_guessing() {
        let mut sampler = Sampler::new();
        let first = sampler.sample();
        assert_eq!(first.cpu, 0.0, "a rate needs two readings");
        assert!(first.gpu.is_none());
        let second = sampler.sample();
        assert!(second.cpu >= 0.0);
    }

    #[test]
    fn history_keeps_the_last_points_and_can_still_draw() {
        let mut history = History::new(3);
        for cpu in [1.0, 2.0, 3.0, 4.0] {
            history.push(Sample {
                cpu,
                ..Sample::default()
            });
        }
        assert_eq!(history.points().len(), 3);
        assert_eq!(
            history.points().first().unwrap().cpu,
            2.0,
            "the oldest went"
        );
        let (values, peak) = history.series(|sample| Some(sample.cpu));
        assert_eq!(values, vec![2.0, 3.0, 4.0]);
        assert_eq!(peak, 4.0);
        assert_eq!(
            history.series(|sample| sample.gpu).0.len(),
            0,
            "no GPU series"
        );
    }
}
