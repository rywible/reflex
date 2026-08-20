//! Interval arithmetic for Wrela certificate verification.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Interval {
    pub low: f64,
    pub high: f64,
}

impl Interval {
    pub fn new(low: f64, high: f64) -> Self {
        Self {
            low: low.min(high),
            high: low.max(high),
        }
    }

    pub fn width(&self) -> f64 {
        self.high - self.low
    }

    pub fn mid(&self) -> f64 {
        (self.low + self.high) * 0.5
    }

    pub fn contains(&self, value: f64) -> bool {
        value >= self.low && value <= self.high
    }

    pub fn contains_interval(&self, other: &Interval) -> bool {
        other.low >= self.low && other.high <= self.high
    }

    pub fn add(&self, other: &Interval) -> Interval {
        Interval::new(self.low + other.low, self.high + other.high)
    }

    pub fn sub(&self, other: &Interval) -> Interval {
        Interval::new(self.low - other.high, self.high - other.low)
    }

    pub fn mul(&self, other: &Interval) -> Interval {
        let p1 = self.low * other.low;
        let p2 = self.low * other.high;
        let p3 = self.high * other.low;
        let p4 = self.high * other.high;
        let min = p1.min(p2).min(p3).min(p4);
        let max = p1.max(p2).max(p3).max(p4);
        Interval::new(min, max)
    }

    pub fn scale(&self, s: f64) -> Interval {
        if s >= 0.0 {
            Interval::new(self.low * s, self.high * s)
        } else {
            Interval::new(self.high * s, self.low * s)
        }
    }

    pub fn pow_i(&self, n: usize) -> Interval {
        if n == 0 {
            Interval::new(1.0, 1.0)
        } else if n % 2 == 1 {
            Interval::new(self.low.powi(n as i32), self.high.powi(n as i32))
        } else {
            let min = if self.low <= 0.0 && self.high >= 0.0 {
                0.0
            } else {
                self.low.powi(n as i32).min(self.high.powi(n as i32))
            };
            let max = self.low.powi(n as i32).max(self.high.powi(n as i32));
            Interval::new(min, max)
        }
    }
}

/// Reference kernel P(x) = 2x² - 3x + 1.5
pub fn eval_polynomial(x: f64) -> f64 {
    2.0 * x * x - 3.0 * x + 1.5
}

/// True range of P on [low, high] via critical points.
pub fn true_range_on_domain(low: f64, high: f64) -> Interval {
    let lo = low.min(high);
    let hi = low.max(high);
    let mut min_v = eval_polynomial(lo).min(eval_polynomial(hi));
    let mut max_v = eval_polynomial(lo).max(eval_polynomial(hi));
    // P'(x) = 4x - 3, critical at x = 0.75
    let crit = 0.75;
    if crit > lo && crit < hi {
        let v = eval_polynomial(crit);
        min_v = min_v.min(v);
        max_v = max_v.max(v);
    }
    Interval::new(min_v, max_v)
}
