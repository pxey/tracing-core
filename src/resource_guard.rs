//! مراقبة موارد العملية نفسها (CPU/RAM) — القسم 6 المُصحح:
//! هامش أوسع (80MB بدل 50MB)، ووضع التقشف يبدأ عند 70% بدل 80%،
//! مع تسجيل ctc_throttle_events_total لرصد أي تدهور ناتج عن القيود ذاتها.

use crate::metrics;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const MEMORY_SOFT_LIMIT_MB: u64 = 80;
pub const AUSTERITY_THRESHOLD_PERCENT: f64 = 70.0;

/// عند تفعيلها، يتحول proxy.rs إلى "تسجيل سلبي" (log-only) دون اعتراض فعّال
/// — أي يتوقف عن إرسال مهام جديدة للتقييم مؤقتًا حتى تنخفض الذاكرة.
pub struct AusterityMode {
    pub active: Arc<AtomicBool>,
}

impl AusterityMode {
    pub fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }
}

/// حلقة رصد دورية (كل 5 ثوانٍ) — لا تعمل على مسار أي طلب فعلي.
pub async fn run_resource_monitor(austerity: Arc<AusterityMode>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;

        let Some((rss_mb, usage_percent, should_be_active)) =
            evaluate_rss(read_process_rss_mb())
        else {
            // لا تفسّر القياس المفقود على أنه صفر؛ أبقِ الوضع الحالي كما هو.
            // هذا يمنع قراءة RSS الفاشلة من إلغاء وضع التقشف بصورة خاطئة.
            tracing::debug!("تعذرت قراءة RSS؛ الإبقاء على حالة وضع التقشف الحالية");
            continue;
        };

        let was_active = austerity.is_active();

        if should_be_active && !was_active {
            tracing::warn!(
                rss_mb,
                usage_percent,
                "تفعيل وضع التقشف: التحويل إلى تسجيل سلبي دون اعتراض فعّال"
            );
            metrics::THROTTLE_EVENTS_TOTAL.inc();
            austerity.active.store(true, Ordering::Relaxed);
        } else if !should_be_active && was_active {
            tracing::info!(rss_mb, "إلغاء وضع التقشف: العودة للاعتراض الفعّال الكامل");
            austerity.active.store(false, Ordering::Relaxed);
        }
    }
}

fn evaluate_rss(rss_mb: Option<u64>) -> Option<(u64, f64, bool)> {
    let rss_mb = rss_mb?;
    let usage_percent = (rss_mb as f64 / MEMORY_SOFT_LIMIT_MB as f64) * 100.0;
    let should_be_active = usage_percent >= AUSTERITY_THRESHOLD_PERCENT;
    Some((rss_mb, usage_percent, should_be_active))
}

#[cfg(target_os = "linux")]
fn read_process_rss_mb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.trim().split_whitespace().next()?.parse().ok()?;
            return Some(kb / 1024);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn read_process_rss_mb() -> Option<u64> {
    None // على أنظمة غير لينكس، يُعتمد بديل عبر crate مثل sysinfo في الإنتاج
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_rss_does_not_become_zero_usage() {
        assert_eq!(evaluate_rss(None), None);
    }

    #[test]
    fn austerity_starts_at_seventy_percent() {
        let (rss_mb, usage_percent, active) = evaluate_rss(Some(56)).unwrap();
        assert_eq!(rss_mb, 56);
        assert_eq!(usage_percent, 70.0);
        assert!(active);
    }

    #[test]
    fn austerity_stays_off_below_threshold() {
        let (rss_mb, usage_percent, active) = evaluate_rss(Some(55)).unwrap();
        assert_eq!(rss_mb, 55);
        assert_eq!(usage_percent, 68.75);
        assert!(!active);
    }
}
