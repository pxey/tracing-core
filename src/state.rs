//! متعقب الحالة: يحتفظ بسجل آخر N طلبات لكل (Session_ID + Resource_URL)
//! لدعم التتبع المسبب (Causal Backward Tracing) دون التأثير على مسار الطلب الحالي.
//! التحديث والقراءة هنا لا يحدثان أبدًا قبل إرسال الرد للمستخدم (post-hoc فقط).

use lru::LruCache;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::time::{SystemTime, UNIX_EPOCH};

const HISTORY_PER_KEY: usize = 5; // آخر 5 طلبات لكل جلسة كما في القسم 3.3
const LRU_CAPACITY: usize = 1000; // سعة 1000 مدخل لكل مسار كما في القسم 4

#[derive(Debug, Clone, Serialize)]
pub struct RequestSnapshot {
    pub request_id: String,
    pub timestamp_ms: u128,
    pub request_body: serde_json::Value,
    pub response_body: serde_json::Value,
}

pub struct StateTracker {
    cache: Mutex<LruCache<String, VecDeque<RequestSnapshot>>>,
}

fn compound_key(session_id: &str, resource_url: &str) -> String {
    format!("{session_id}::{resource_url}")
}

impl StateTracker {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(LRU_CAPACITY).unwrap())),
        }
    }

    /// يُستدعى بعد إرسال الرد بالكامل (ضمن مهمة async منفصلة) — لا يحجب مسار الطلب.
    pub fn record(
        &self,
        session_id: &str,
        resource_url: &str,
        request_id: String,
        request_body: serde_json::Value,
        response_body: serde_json::Value,
    ) {
        let key = compound_key(session_id, resource_url);
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let mut cache = self.cache.lock();
        let entry = cache.get_or_insert_mut(key, VecDeque::new);
        entry.push_back(RequestSnapshot {
            request_id,
            timestamp_ms,
            request_body,
            response_body,
        });
        while entry.len() > HISTORY_PER_KEY {
            entry.pop_front();
        }
    }

    /// يُستخدم في Backward Propagation لتحديد أي مدخل سبق الانحراف (Mutation Tracking).
    pub fn history(&self, session_id: &str, resource_url: &str) -> Vec<RequestSnapshot> {
        let key = compound_key(session_id, resource_url);
        let mut cache = self.cache.lock();
        cache
            .get(&key)
            .map(|dq| dq.iter().cloned().collect())
            .unwrap_or_default()
    }
}
