//! Local macOS utilization sampled independently from engine and UI work.

// Rust guideline compliant 2026-02-21
use std::{
    process::Stdio,
    sync::mpsc,
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Utilization {
    pub cpu: Option<u8>,
    pub gpu: Option<u8>,
}

#[derive(Debug)]
pub(crate) struct Monitor {
    rx: mpsc::Receiver<(Instant, Utilization)>,
    latest: Option<(Instant, Utilization)>,
}

impl Monitor {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let mut system = sysinfo::System::new();
                system.refresh_cpu_usage(); // Seed tick deltas; do not report a bogus first sample.
                let mut timer = tokio::time::interval(Duration::from_secs(1));
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                timer.tick().await;
                loop {
                    timer.tick().await;
                    system.refresh_cpu_usage();
                    let utilization = Utilization {
                        cpu: (!system.cpus().is_empty())
                            .then(|| percentage(system.global_cpu_usage()))
                            .flatten(),
                        gpu: gpu_usage().await,
                    };
                    match tx.try_send((Instant::now(), utilization)) {
                        Err(mpsc::TrySendError::Disconnected(_)) => break,
                        Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                    }
                }
            });
        });
        Self { rx, latest: None }
    }

    pub fn sample(&mut self) -> Utilization {
        while let Ok(sample) = self.rx.try_recv() {
            self.latest = Some(sample);
        }
        self.latest
            .filter(|(time, _)| time.elapsed() < Duration::from_secs(3))
            .map(|(_, utilization)| utilization)
            .unwrap_or_default()
    }
}

fn percentage(value: f32) -> Option<u8> {
    value
        .is_finite()
        .then(|| value.clamp(0.0, 100.0).round() as u8)
}

// macpow uses the same IORegistry PerformanceStatistics counters. Query only
// accelerators; never use sudo or spawn the full powermetrics monitoring stack.
async fn gpu_usage() -> Option<u8> {
    let read = async {
        let mut child = tokio::process::Command::new("/usr/sbin/ioreg")
            .args(["-r", "-c", "IOAccelerator", "-d", "1", "-l", "-w", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let mut output = Vec::new();
        // Bound registry output, including malformed or unexpectedly large properties.
        child
            .stdout
            .take()?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut output)
            .await
            .ok()?;
        if output.len() > 1024 * 1024 {
            return None;
        }
        if !child.wait().await.ok()?.success() {
            return None;
        }
        parse_gpu(&String::from_utf8(output).ok()?)
    };
    tokio::time::timeout(Duration::from_millis(500), read)
        .await
        .ok()
        .flatten()
}

fn parse_gpu(output: &str) -> Option<u8> {
    output
        .lines()
        .filter(|line| line.trim_start().starts_with("\"PerformanceStatistics\" ="))
        .filter_map(|line| {
            [
                "Device Utilization %",
                "GPU Core Utilization %",
                "GPU Activity(%)",
            ]
            .iter()
            .find_map(|key| {
                let (_, value) = line.split_once(&format!("\"{key}\"="))?;
                let value = value
                    .trim_start()
                    .split(|c: char| !c.is_ascii_digit() && c != '.')
                    .next()?;
                percentage(value.parse().ok()?)
            })
        })
        .max() // Busiest accelerator, rather than summing percentages above 100.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_is_distinct_from_idle_and_multiple_gpus_stay_bounded() {
        assert_eq!(parse_gpu(""), None);
        assert_eq!(
            parse_gpu("\"PerformanceStatistics\" = {\"Device Utilization %\"=0}"),
            Some(0)
        );
        assert_eq!(
            parse_gpu(
                "\"PerformanceStatistics\" = {\"Device Utilization %\"=10}\n\"PerformanceStatistics\" = {\"GPU Activity(%)\"=99}"
            ),
            Some(99)
        );
        assert_eq!(
            parse_gpu("\"PerformanceStatistics\" = {\"Device Utilization %\"=120}"),
            Some(100)
        );
        assert_eq!(percentage(f32::NAN), None);
    }

    #[tokio::test]
    async fn reads_local_gpu_without_privileges() {
        // Hardware-dependent availability is optional; a stalled command must return.
        let value = tokio::time::timeout(Duration::from_secs(1), gpu_usage())
            .await
            .expect("bounded sampler");
        eprintln!("Local GPU utilization: {value:?}");
        assert!(value.is_none_or(|value| value <= 100));
    }
}
