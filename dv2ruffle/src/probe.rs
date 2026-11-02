use std::sync::atomic::Ordering;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use ruffle_render_wgpu::OFFSCREEN_BLENDS;
use ruffle_render_wgpu::wgpu::hal::vulkan::LIVE_COMMAND_BUFFERS;

const MODES: [&str; 15] = [
    "Normal",
    "",
    "Layer",
    "Multiply",
    "Screen",
    "Lighten",
    "Darken",
    "Difference",
    "Add",
    "Subtract",
    "Invert",
    "Alpha",
    "Erase",
    "Overlay",
    "HardLight",
];

static ON: LazyLock<bool> = LazyLock::new(|| std::env::var_os("DV2_RUFFLE_PROBE").is_some());
static WINDOW: Mutex<Window> = Mutex::new(Window::new());

pub fn start() -> Option<Instant> {
    ON.then(Instant::now)
}

pub fn advance(start: Option<Instant>) {
    if let Some(start) = start {
        WINDOW.lock().unwrap().advance(start.elapsed());
    }
}

pub fn render(start: Option<Instant>) {
    if let Some(start) = start {
        let live = LIVE_COMMAND_BUFFERS.load(Ordering::Relaxed);
        let blends = OFFSCREEN_BLENDS.each_ref().map(|kind| {
            kind.each_ref()
                .map(|count| count.swap(0, Ordering::Relaxed))
        });
        if let Some(line) =
            WINDOW
                .lock()
                .unwrap()
                .render(Instant::now(), start.elapsed(), live, &blends)
        {
            eprintln!("[dv2ruffle] {line}");
        }
    }
}

struct Window {
    started: Option<Instant>,
    last: Option<Instant>,
    frames: u32,
    frame_sum: Duration,
    frame_max: Duration,
    renders: u32,
    render_sum: Duration,
    render_max: Duration,
    advance_sum: Duration,
    blends: [[u32; 15]; 2],
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

impl Window {
    const fn new() -> Self {
        Self {
            started: None,
            last: None,
            frames: 0,
            frame_sum: Duration::ZERO,
            frame_max: Duration::ZERO,
            renders: 0,
            render_sum: Duration::ZERO,
            render_max: Duration::ZERO,
            advance_sum: Duration::ZERO,
            blends: [[0; 15]; 2],
        }
    }

    fn advance(&mut self, spent: Duration) {
        self.advance_sum += spent;
    }

    fn render(
        &mut self,
        now: Instant,
        spent: Duration,
        live: usize,
        blends: &[[u32; 15]; 2],
    ) -> Option<String> {
        for (kind, counts) in self.blends.iter_mut().zip(blends) {
            for (total, count) in kind.iter_mut().zip(counts) {
                *total += count;
            }
        }
        if let Some(last) = self.last {
            let frame = now - last;
            self.frames += 1;
            self.frame_sum += frame;
            self.frame_max = self.frame_max.max(frame);
        }
        self.last = Some(now);
        self.renders += 1;
        self.render_sum += spent;
        self.render_max = self.render_max.max(spent);
        let started = *self.started.get_or_insert(now);
        if now - started < Duration::from_secs(1) {
            return None;
        }
        let per_render = |count: u32| count as f64 / self.renders as f64;
        let groups: Vec<String> = (0..15)
            .filter(|&mode| self.blends[0][mode] > 0)
            .map(|mode| {
                format!(
                    "{}:{:.1}/{:.1}",
                    MODES[mode],
                    per_render(self.blends[0][mode]),
                    per_render(self.blends[1][mode])
                )
            })
            .collect();
        let line = format!(
            "probe frames={} frame_ms={:.1}/{:.1} render_ms={:.1}/{:.1} advance_ms={:.1} command_buffers={live} blends={}",
            self.frames,
            ms(self.frame_sum) / self.frames as f64,
            ms(self.frame_max),
            ms(self.render_sum) / self.renders as f64,
            ms(self.render_max),
            ms(self.advance_sum) / self.frames as f64,
            groups.join(","),
        );
        *self = Self {
            started: Some(now),
            last: Some(now),
            ..Self::new()
        };
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_reports_once_a_second_and_starts_over() {
        let t = Instant::now();
        let at = Duration::from_millis;
        let none = [[0; 15]; 2];
        let mut one = none;
        one[0][8] = 3;
        one[1][8] = 1;
        one[0][9] = 1;
        let mut w = Window::new();
        assert_eq!(w.render(t, at(10), 32, &one), None);
        w.advance(at(4));
        assert_eq!(w.render(t + at(30), at(12), 32, &none), None);
        w.advance(at(8));
        assert_eq!(
            w.render(t + at(1000), at(14), 32, &none).as_deref(),
            Some(
                "probe frames=2 frame_ms=500.0/970.0 render_ms=12.0/14.0 advance_ms=6.0 command_buffers=32 blends=Add:1.0/0.3,Subtract:0.3/0.0"
            )
        );
        assert_eq!(w.render(t + at(1010), at(2), 16, &none), None);
        assert_eq!(
            w.render(t + at(2000), at(4), 16, &none).as_deref(),
            Some(
                "probe frames=2 frame_ms=500.0/990.0 render_ms=3.0/4.0 advance_ms=0.0 command_buffers=16 blends="
            )
        );
    }
}
