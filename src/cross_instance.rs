//! مقارنة التجزئات عبر النسخ (Horizontal Scaling) — القسم 7 المُصحح:
//! - رُفعت التجزئة من 32-بت إلى 64-بت (xxHash64) لتقليل احتمال التصادم.
//! - عند تطابق التجزئة، تُجرى مقارنة تأكيدية على القيمة الكاملة قبل اعتبارها "غير متناقضة" —
//!   التجزئة تُستخدم كفلتر أولي لتقليل نقل البيانات فقط، لا كمصدر ثقة نهائي.
//!
//! هذه الوحدة اختيارية (feature = "redis-cross-instance") لأنها تتطلب بنية تحتية Redis
//! فعلية غير متوفرة في كل بيئة تشغيل.

use serde::{Deserialize, Serialize};
use twox_hash::XxHash64;
use std::hash::Hasher;

#[derive(Debug, Serialize, Deserialize)]
pub struct HashAnnouncement {
    pub transaction_id: String,
    pub instance_id: String,
    pub value_hash: u64,       // 64-بت بدل 32-بت في v1
    pub full_value_snapshot: serde_json::Value, // للتأكيد الكامل عند تطابق التجزئة فقط
}

pub fn compute_hash_64(value: &serde_json::Value) -> u64 {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let mut hasher = XxHash64::with_seed(0);
    hasher.write(&bytes);
    hasher.finish()
}

/// عند استقبال إعلانين لنفس transaction_id بتجزئات متطابقة، هذه الدالة تتحقق
/// من القيمة الكاملة فعليًا قبل اعتبار المعاملتين متطابقتين حقًا.
/// هذا يمنع إخفاء تناقضات حقيقية بسبب تصادم عرضي في التجزئة (Hash Collision).
pub fn confirm_no_real_mismatch(a: &HashAnnouncement, b: &HashAnnouncement) -> bool {
    if a.value_hash != b.value_hash {
        return false; // التجزئة مختلفة أصلًا ⇒ تناقض حقيقي واضح
    }
    // التجزئة متطابقة: تحقق فعلي من القيمة الكاملة لاستبعاد التصادم العرضي
    a.full_value_snapshot == b.full_value_snapshot
}

#[cfg(feature = "redis-cross-instance")]
pub mod pubsub {
    use super::*;
    use anyhow::Result;
    use redis::AsyncCommands;

    const CHANNEL: &str = "ctc:cross_instance_hashes";

    pub async fn publish(redis_url: &str, ann: &HashAnnouncement) -> Result<()> {
        let client = redis::Client::open(redis_url)?;
        let mut conn = client.get_async_connection().await?;
        let payload = serde_json::to_string(ann)?;
        conn.publish(CHANNEL, payload).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identical_values_confirmed_no_mismatch() {
        let v = json!({"total": 100.5, "currency": "SAR"});
        let h = compute_hash_64(&v);
        let a = HashAnnouncement {
            transaction_id: "tx1".into(),
            instance_id: "i1".into(),
            value_hash: h,
            full_value_snapshot: v.clone(),
        };
        let b = HashAnnouncement {
            transaction_id: "tx1".into(),
            instance_id: "i2".into(),
            value_hash: h,
            full_value_snapshot: v,
        };
        assert!(confirm_no_real_mismatch(&a, &b));
    }

    #[test]
    fn hash_collision_caught_by_full_value_check() {
        // محاكاة تصادم افتراضي: نفس التجزئة يدويًا لكن قيم مختلفة فعليًا
        let v1 = json!({"total": 100.5});
        let v2 = json!({"total": 999.9});
        let a = HashAnnouncement {
            transaction_id: "tx2".into(),
            instance_id: "i1".into(),
            value_hash: 42, // قيمة تجزئة متطابقة مُفترضة (تصادم)
            full_value_snapshot: v1,
        };
        let b = HashAnnouncement {
            transaction_id: "tx2".into(),
            instance_id: "i2".into(),
            value_hash: 42,
            full_value_snapshot: v2,
        };
        // التجزئة متطابقة لكن القيمة الفعلية مختلفة ⇒ يجب رصده كتناقض حقيقي
        assert!(!confirm_no_real_mismatch(&a, &b));
    }
}
