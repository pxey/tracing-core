//! عدادات Prometheus المعروضة عبر المنفذ 9090 كما في القسم 5،
//! مع إضافات v2: ctc_samples_dropped_total و ctc_throttle_events_total.

use once_cell::sync::Lazy;
use prometheus::{Encoder, Histogram, HistogramOpts, IntCounter, Registry, TextEncoder};

pub static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

pub static CHECKS_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    let c = IntCounter::new("ctc_checks_total", "إجمالي عمليات التحقق المُنفَّذة").unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

pub static MISMATCH_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    let c = IntCounter::new("ctc_mismatch_total", "إجمالي حالات الشذوذ المكتشفة").unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

pub static SAMPLES_DROPPED_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    let c = IntCounter::new(
        "ctc_samples_dropped_total",
        "عيّنات أُسقطت بسبب امتلاء قناة المعالجة غير الحاجزة (v2: عوض تأخير الاستجابة)",
    )
    .unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

pub static THROTTLE_EVENTS_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    let c = IntCounter::new(
        "ctc_throttle_events_total",
        "أحداث تقييد موارد ذاتية (CPU/RAM) — لرصد أن النظام نفسه لا يصبح مصدر Latency Spikes",
    )
    .unwrap();
    REGISTRY.register(Box::new(c.clone())).unwrap();
    c
});

pub static LATENCY_SECONDS: Lazy<Histogram> = Lazy::new(|| {
    let h = Histogram::with_opts(HistogramOpts::new(
        "ctc_latency_seconds",
        "زمن تنفيذ التحقق (لا يشمل زمن استجابة الطلب الأصلي، لأنه غير حاجز)",
    ))
    .unwrap();
    REGISTRY.register(Box::new(h.clone())).unwrap();
    h
});

/// يُستدعى مرة واحدة عند الإقلاع لضمان تهيئة كل العدادات (lazy_static تتطلب لمسة أولى).
pub fn init() {
    Lazy::force(&CHECKS_TOTAL);
    Lazy::force(&MISMATCH_TOTAL);
    Lazy::force(&SAMPLES_DROPPED_TOTAL);
    Lazy::force(&THROTTLE_EVENTS_TOTAL);
    Lazy::force(&LATENCY_SECONDS);
}

pub fn render() -> String {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap_or_default()
}
