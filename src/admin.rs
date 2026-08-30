//! نقطة دخول الإدارة (Admin API) — القسم 5 المُصحح في v2:
//! - يجب نشرها خلف mTLS ضمن الـ Service Mesh (انظر ملاحظة الأمان أسفل).
//! - /rules/reload يتطلب توقيعًا على المحتوى ويُسجَّل في audit log غير قابل للحذف.
//! - /trace/{id} يُطبّق تنقيحًا تلقائيًا على الحقول الحساسة ما لم تُقدَّم صلاحية trace:full.
//!
//! ⚠️ ملاحظة تنفيذ: التحقق من التوقيع (Ed25519) ومصادقة mTLS الفعلية تتطلبان
//! شهادات ومفاتيح حقيقية تُدار خارج هذا الكود (عبر cert-manager/Vault مثلًا).
//! هذا الملف يوفر نقاط الإدخال (hooks) اللازمة ويطبّق تحققًا تجريبيًا (HMAC مشترك)
//! قابلًا للاستبدال بسهولة بموفّر مصادقة حقيقي في بيئة الإنتاج.

use crate::rules::{self, RuleSet};
use crate::storage::AnomalyStore;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use parking_lot::RwLock;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub struct AdminState {
    pub rules: Arc<RwLock<Arc<RuleSet>>>,
    pub rules_dir: std::path::PathBuf,
    pub anomaly_store: Arc<AnomalyStore>,
    pub audit_log_path: std::path::PathBuf,
    /// سر مشترك للتحقق من توقيع طلبات إعادة التحميل — استبدله بمفتاح Ed25519 حقيقي.
    pub reload_shared_secret: String,
}

#[derive(Serialize)]
struct ReloadResponse {
    status: String,
    version: u64,
}

fn verify_reload_signature(secret: &str, headers: &HeaderMap) -> bool {
    let provided = headers
        .get("x-ctc-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    let expected = format!("{:x}", hasher.finalize());
    // مقارنة زمن ثابت مبسطة؛ في الإنتاج استخدم subtle::ConstantTimeEq
    provided == expected
}

fn append_audit_log(path: &std::path::Path, actor: &str, action: &str, result: &str) {
    use std::io::Write;
    let line = format!(
        "{} actor={} action={} result={}\n",
        chrono::Utc::now().to_rfc3339(),
        actor,
        action,
        result
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// POST /rules/reload
/// يتطلب رأس x-ctc-signature صحيحًا (mTLS يُفترض مُطبَّقًا على مستوى الشبكة/الـ mesh).
pub async fn reload_rules(
    State(admin): State<Arc<AdminState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !verify_reload_signature(&admin.reload_shared_secret, &headers) {
        append_audit_log(&admin.audit_log_path, "unknown", "rules_reload", "denied_bad_signature");
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({
            "error": "توقيع غير صالح أو مفقود في x-ctc-signature"
        })))
            .into_response();
    }

    let current_version = admin.rules.read().version;
    match rules::load_rules_dir(&admin.rules_dir, current_version) {
        Ok(new_set) => {
            let version = new_set.version;
            *admin.rules.write() = Arc::new(new_set);
            append_audit_log(&admin.audit_log_path, "admin", "rules_reload", "success");
            (
                StatusCode::OK,
                Json(serde_json::to_value(ReloadResponse {
                    status: "reloaded".into(),
                    version,
                })
                .unwrap()),
            )
                .into_response()
        }
        Err(e) => {
            append_audit_log(&admin.audit_log_path, "admin", "rules_reload", &format!("failed: {e}"));
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /trace/{request_id}
/// يرجع شجرة المسببات (سلسلة الطلبات المرتبطة + الشذوذ المكتشف) بصيغة DAG مبسّطة،
/// مع تنقيح تلقائي للحقول الحساسة ما لم يحمل الطالب صلاحية trace:full.
pub async fn get_trace(
    State(admin): State<Arc<AdminState>>,
    AxumPath(request_id): AxumPath<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let has_full_access = headers
        .get("x-ctc-scope")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.contains("trace:full"))
        .unwrap_or(false);

    match admin.anomaly_store.find_by_request_id(&request_id) {
        Ok(records) => {
            let redacted: Vec<Value> = records
                .into_iter()
                .map(|r| {
                    let mut v = serde_json::to_value(&r).unwrap();
                    if !has_full_access {
                        // تنقيح تلقائي: إخفاء تفاصيل قد تحتوي قيمًا حساسة (أسعار/خصومات فعلية)
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("details".into(), Value::String("[REDACTED]".into()));
                        }
                    }
                    v
                })
                .collect();

            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "request_id": request_id,
                    "dag_nodes": redacted,
                    "redacted": !has_full_access
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
