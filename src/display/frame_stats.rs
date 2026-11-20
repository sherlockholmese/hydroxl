use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const LIVE_WINDOW: Duration = Duration::from_secs(1);
const HISTORY: Duration = Duration::from_secs(10);
const FRAME_BUDGET: Duration = Duration::from_nanos(16_666_667);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FrameRates {
    pub(crate) live: Option<f64>,
    pub(crate) average: Option<f64>,
    pub(crate) low: Option<f64>,
}

#[derive(Default)]
pub(crate) struct FrameStats {
    started: Option<Instant>,
    last: Option<(Instant, u64)>,
    dimensions: Option<(u32, u32)>,
    frames: VecDeque<(Instant, Option<Duration>)>,
}

impl FrameStats {
    pub(crate) fn reset(&mut self) {
        self.started = None;
        self.last = None;
        self.dimensions = None;
        self.frames.clear();
    }

    pub(crate) fn painted(&mut self, now: Instant, revision: u64, dimensions: (u32, u32)) {
        if self.dimensions != Some(dimensions) {
            self.reset();
            self.dimensions = Some(dimensions);
        }
        if self.last.is_some_and(|(_, last_revision)| last_revision == revision) {
            return;
        }
        self.trim(now);
        self.started.get_or_insert(now);
        let gap = self.last.map(|(last, _)| now.duration_since(last));
        self.frames.push_back((now, gap));
        self.last = Some((now, revision));
    }

    pub(crate) fn sample(&mut self, now: Instant) -> FrameRates {
        self.trim(now);
        let started = *self.started.get_or_insert(now);
        let elapsed = now.duration_since(started);
        if elapsed < LIVE_WINDOW {
            return FrameRates::default();
        }

        let live = self
            .frames
            .iter()
            .rev()
            .take_while(|(at, _)| now.duration_since(*at) < LIVE_WINDOW)
            .count();
        let average = self.frames.len() as f64 / elapsed.min(HISTORY).as_secs_f64();
        let mut gaps: Vec<_> = self.frames.iter().filter_map(|(_, gap)| *gap).collect();
        if let Some((last, _)) = self.last {
            let trailing = now.duration_since(last).min(HISTORY);
            if trailing > FRAME_BUDGET {
                gaps.push(trailing);
            }
        }
        let low = (gaps.len() >= 2).then(|| {
            gaps.sort_unstable_by(|a, b| b.cmp(a));
            let count = gaps.len().div_ceil(100);
            let slowest: Duration = gaps[..count].iter().sum();
            count as f64 / slowest.as_secs_f64()
        });
        FrameRates {
            live: Some(live as f64),
            average: Some(average),
            low,
        }
    }

    fn trim(&mut self, now: Instant) {
        while self.frames.front().is_some_and(|(at, _)| now.duration_since(*at) >= HISTORY) {
            self.frames.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_stutter_and_ignores_repaints() {
        let now = Instant::now();
        let mut smooth = FrameStats::default();
        let mut uneven = FrameStats::default();
        for frame in 0..=598 {
            let at = now + FRAME_BUDGET * frame;
            smooth.painted(at, u64::from(frame), (1920, 1080));
            smooth.painted(at + Duration::from_micros(100), u64::from(frame), (1920, 1080));
            let offset = if frame % 10 == 1 {
                Duration::from_millis(15)
            } else {
                Duration::ZERO
            };
            uneven.painted(at + offset, u64::from(frame), (1920, 1080));
        }
        let end = now + FRAME_BUDGET * 598 + Duration::from_millis(1);
        let smooth = smooth.sample(end);
        let uneven = uneven.sample(end);
        assert_eq!(smooth.average, uneven.average);
        assert_eq!(smooth.live, Some(60.));
        assert!(smooth.low.unwrap() >= 59.9);
        assert!(uneven.low.unwrap() < 40.);
    }

    #[test]
    fn ages_out_idle_frames_and_resets_on_resolution_change() {
        let now = Instant::now();
        let mut stats = FrameStats::default();
        stats.painted(now, 1, (1280, 720));
        stats.painted(now + FRAME_BUDGET, 2, (1280, 720));
        let end = now + HISTORY + Duration::from_secs(1);
        let idle = stats.sample(end);
        assert_eq!((idle.live, idle.average, idle.low), (Some(0.), Some(0.), None));
        stats.painted(end, 3, (1920, 1080));
        assert_eq!(stats.sample(end), FrameRates::default());
    }
}
