//! Frame pacing.
//!
//! On Windows the app draws without waiting for vsync (see `main.rs`): waiting made every
//! frame block the UI thread for a whole refresh period, which on a 32 Hz Remote Desktop
//! display meant ~31 ms stalls and choppy scrolling. Without vsync nothing limits the frame
//! rate while egui animates (smooth scrolling, spinners), so [`Pacer`] caps it here.
//! When nothing moves, egui asks for no frames at all and the app stays idle.
//!
//! `FRAMELOG_8823=1` writes a once-a-second summary (frame intervals, time spent in the UI
//! and in the cap, process CPU) to `%TEMP%\8823-frames.log` (or the path given instead of 1).

use std::io::Write as _;
use std::time::{Duration, Instant};

/// Frame-rate cap while animating. 120 fps: smooth on common 60-120 Hz screens and well
/// above a Remote Desktop session's 30-60 Hz, while keeping scroll animation far below one
/// full core (a frame costs ~0.1-0.5 ms of UI work).
pub const MAX_FPS: u32 = 120;

pub struct Pacer {
    enabled: bool,
    min_interval: Duration,
    last_start: Option<Instant>,
    log: Option<FrameLog>,
}

impl Pacer {
    pub fn new() -> Self {
        Pacer {
            enabled: cfg!(windows),
            min_interval: Duration::from_secs(1) / MAX_FPS,
            last_start: None,
            log: FrameLog::from_env(),
        }
    }

    /// Call at the very start of each frame. Sleeps if the previous frame started less than
    /// 1/MAX_FPS ago; the first frame after a pause (a key press after idling) never waits.
    pub fn frame_start(&mut self) {
        let now = Instant::now();
        let mut slept = Duration::ZERO;
        if self.enabled
            && let Some(last) = self.last_start
        {
            let since = now - last;
            if since < self.min_interval {
                slept = self.min_interval - since;
                std::thread::sleep(slept);
            }
        }
        let start = Instant::now();
        if let Some(log) = &mut self.log {
            log.frame_start(start, self.last_start.map(|l| start - l), slept);
        }
        self.last_start = Some(start);
    }

    /// Call at the end of each frame's UI code.
    pub fn frame_end(&mut self) {
        if let (Some(log), Some(start)) = (&mut self.log, self.last_start) {
            log.ui_time += start.elapsed();
        }
    }
}

struct FrameLog {
    out: std::fs::File,
    window_start: Instant,
    intervals: Vec<Duration>,
    ui_time: Duration,
    slept: Duration,
    cpu_at_window_start: Duration,
}

impl FrameLog {
    fn from_env() -> Option<Self> {
        let v = std::env::var_os("FRAMELOG_8823")?;
        let path = if v == "1" { std::env::temp_dir().join("8823-frames.log") } else { v.into() };
        let mut out = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        let _ = writeln!(out, "# 8823 frame log, pacing {}, cap {MAX_FPS} fps", if cfg!(windows) { "on" } else { "off" });
        let _ = writeln!(out, "# frames  interval_ms(min/median/max)  ui_ms  capped_ms  process_cpu_ms   (per ~1 s with frames)");
        Some(FrameLog {
            out,
            window_start: Instant::now(),
            intervals: Vec::new(),
            ui_time: Duration::ZERO,
            slept: Duration::ZERO,
            cpu_at_window_start: process_cpu(),
        })
    }

    fn frame_start(&mut self, now: Instant, interval: Option<Duration>, slept: Duration) {
        if now - self.window_start >= Duration::from_secs(1) {
            self.flush(now);
        }
        // Intervals across an idle gap (no frames wanted) say nothing about pacing.
        if let Some(i) = interval.filter(|i| *i < Duration::from_millis(250)) {
            self.intervals.push(i);
        }
        self.slept += slept;
    }

    fn flush(&mut self, now: Instant) {
        let cpu = process_cpu();
        if !self.intervals.is_empty() {
            self.intervals.sort();
            let ms = |d: Duration| d.as_secs_f64() * 1000.0;
            let n = self.intervals.len();
            let _ = writeln!(
                self.out,
                "{n:6}  {:6.2} / {:6.2} / {:6.2}  {:7.1}  {:8.1}  {:8.1}",
                ms(self.intervals[0]),
                ms(self.intervals[n / 2]),
                ms(self.intervals[n - 1]),
                ms(self.ui_time),
                ms(self.slept),
                ms(cpu.saturating_sub(self.cpu_at_window_start)),
            );
        }
        self.intervals.clear();
        self.ui_time = Duration::ZERO;
        self.slept = Duration::ZERO;
        self.cpu_at_window_start = cpu;
        self.window_start = now;
    }
}

impl Drop for FrameLog {
    fn drop(&mut self) {
        self.flush(Instant::now());
    }
}

/// User + kernel CPU time of the whole process (all threads, including the GL driver's).
#[cfg(windows)]
fn process_cpu() -> Duration {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    if ok == 0 {
        return Duration::ZERO;
    }
    let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    Duration::from_nanos((t(k) + t(u)) * 100)
}

#[cfg(unix)]
fn process_cpu() -> Duration {
    // /proc/self/stat fields 14 and 15: utime, stime in clock ticks (100 Hz on Linux).
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| {
            let rest = &s[s.rfind(')')? + 2..];
            let f: Vec<&str> = rest.split_whitespace().collect();
            Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
        })
        .map(|ticks| Duration::from_millis(ticks * 10))
        .unwrap_or_default()
}

#[cfg(not(any(windows, unix)))]
fn process_cpu() -> Duration {
    Duration::ZERO
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacer_caps_back_to_back_frames_and_never_delays_after_idle() {
        let mut p = Pacer { enabled: true, min_interval: Duration::from_millis(8), last_start: None, log: None };
        let t0 = Instant::now();
        for _ in 0..10 {
            p.frame_start();
            p.frame_end();
        }
        // 9 capped gaps of 8 ms.
        assert!(t0.elapsed() >= Duration::from_millis(72), "{:?}", t0.elapsed());
        std::thread::sleep(Duration::from_millis(20));
        let t1 = Instant::now();
        p.frame_start();
        assert!(t1.elapsed() < Duration::from_millis(4), "{:?}", t1.elapsed());
    }
}
