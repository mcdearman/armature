use std::time::Instant;

/// A value that eases toward a target over time. Widgets keep these in
/// their state and read them while drawing.
#[derive(Clone, Copy, Debug)]
pub struct Anim {
    value: f32,
    target: f32,
    last: Option<Instant>,
}

impl Default for Anim {
    fn default() -> Self {
        Self::new(0.0)
    }
}

impl Anim {
    pub const fn new(value: f32) -> Self {
        Self { value, target: value, last: None }
    }

    /// Advances toward `target` and returns the current value. `duration`
    /// is the time for a full 0-to-1 transition; zero snaps immediately.
    pub fn step(&mut self, target: f32, now: Instant, duration: f32) -> f32 {
        self.target = target;
        let dt = self.last.map(|l| now.saturating_duration_since(l).as_secs_f32()).unwrap_or(0.0);
        self.last = Some(now);
        if duration <= 0.0 {
            self.value = target;
            return self.value;
        }
        let step = dt / duration;
        let d = target - self.value;
        if d.abs() <= step {
            self.value = target;
        } else {
            self.value += step * d.signum();
        }
        self.value
    }

    /// Jumps to `v` with no transition.
    pub fn set(&mut self, v: f32) {
        self.value = v;
        self.target = v;
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn is_animating(&self) -> bool {
        (self.value - self.target).abs() > f32::EPSILON
    }

    /// Smoothstep of the current value, for nicer motion curves.
    pub fn eased(&self) -> f32 {
        let t = self.value.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
}
