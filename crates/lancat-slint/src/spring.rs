//! 弹簧曲线：按 Apple 从 iOS 17 / macOS 14 起统一使用的「时长 + 回弹」参数（WWDC23 “Animate with springs”）：
//! 质量 1，刚度 = (2π / duration)²，阻尼 = 4π(1 − bounce) / duration（bounce ≥ 0）。
//! 例：duration 0.5、bounce 0.3 → 刚度 157.9、阻尼 17.6，与 SwiftUI `Spring(duration: 0.5, bounce: 0.3)` 相同。
//! 预设（SwiftUI）：.smooth = (0.5, 0)，.snappy = (0.3, 0.15)，.bouncy = (0.5, 0.3)。
//! 这里是从 0 到 1、初速度 0 的解析解。

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub duration: f64,
    pub bounce: f64,
}

impl Spring {
    pub const fn new(duration: f64, bounce: f64) -> Self {
        Self { duration, bounce }
    }

    /// 秒 → 位置（0 → 1，可略超 1）
    pub fn at(&self, t: f64) -> f64 {
        if t <= 0.0 {
            return 0.0;
        }
        let w0 = std::f64::consts::TAU / self.duration.max(1e-3);
        let z = (1.0 - self.bounce).clamp(0.0, 2.0);
        if (z - 1.0).abs() < 1e-6 {
            1.0 - (-w0 * t).exp() * (1.0 + w0 * t)
        } else if z < 1.0 {
            let wd = w0 * (1.0 - z * z).sqrt();
            1.0 - (-z * w0 * t).exp() * ((wd * t).cos() + (z * w0 / wd) * (wd * t).sin())
        } else {
            // 过阻尼
            let r = w0 * (z * z - 1.0).sqrt();
            let (r1, r2) = (-z * w0 + r, -z * w0 - r);
            1.0 - (r2 * (r1 * t).exp() - r1 * (r2 * t).exp()) / (r2 - r1)
        }
    }

    /// 停稳的时间（与 1 的差 < 0.1% 的包络时刻，秒）
    pub fn settle(&self) -> f64 {
        let w0 = std::f64::consts::TAU / self.duration.max(1e-3);
        let z = (1.0 - self.bounce).clamp(0.05, 1.0);
        // 临界阻尼 e^{-w0 t}(1 + w0 t) < 1e-3 ≈ 9.23 / w0
        if z >= 0.999 {
            9.23 / w0
        } else {
            (1000.0f64 / (1.0 - z * z).sqrt()).ln() / (z * w0)
        }
    }

    /// 已经过了 `t` 秒：(位置, 是否停稳)
    pub fn sample(&self, t: f64) -> (f64, bool) {
        if t >= self.settle() {
            (1.0, true)
        } else {
            (self.at(t), false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_swiftui_parameters() {
        let s = Spring::new(0.5, 0.3);
        let w0 = std::f64::consts::TAU / s.duration;
        assert!((w0 * w0 - 157.9).abs() < 0.1);
        assert!((2.0 * (1.0 - s.bounce) * w0 - 17.6).abs() < 0.1);
        assert!(s.at(0.0) == 0.0);
        assert!((s.sample(5.0).0 - 1.0).abs() < 1e-9);
        // 有回弹：中途超过 1
        assert!((0..200).map(|i| s.at(i as f64 / 200.0)).fold(0.0, f64::max) > 1.02);
        // .smooth 不超过 1
        let m = Spring::new(0.5, 0.0);
        assert!((0..400).map(|i| m.at(i as f64 / 200.0)).fold(0.0, f64::max) <= 1.0 + 1e-9);
    }
}
