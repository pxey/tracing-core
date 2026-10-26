//! Sidecar Proxy: يعترض HTTP، يُمرر الرد فورًا دون انتظار (Fire-and-Forget)،
//! ثم يُقيّم القواعد بشكل غير حاجز تمامًا في مهمة async منفصلة.
//!
//! التصحيح الجوهري عن v1: زمن استجابة المستخدم لا يتأثر أبدًا بعملية التحقق،
//! بعكس نموذج v1 الذي كان يؤجل الرد حتى 5-7ms لانتظار Diff.

use crate::logic::{self, EvalContext};
use crate::metrics;
use crate::rules::RuleSet;
use crate::state::StateTracker;
use crate::storage::{AnomalyRecord, AnomalyStore};
use hyper::body::Bytes;
use hyper::client::HttpConnector;
use hyper::{Body, Client, Request, Response, Uri};
use parking_lot::RwLock;
use serde_json::Value;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

const EVAL_CHANNEL_CAPACITY: usize = 4096;
const CRITICAL_SAMPLE_RATE_PERCENT: u64 = 20; // مثبتة عند الحمل العالي، خلافًا لـ v1
const NORMAL_HIGH_LOAD_SAMPLE_RATE_PERCENT: u64 = 5;
const LOW_LOAD_THRESHOLD_RPS: u64 = 1000;

pub struct ProxyState {
    pub upstream_base: Uri,
    pub client: Client<HttpConnector>,
    pub rules: Arc<RwLock<Arc<RuleSet>>>,
    pub state_tracker: Arc<StateTracker>,
    pub anomaly_store: Arc<AnomalyStore>,
    pub sender: mpsc::Sender<EvalJob>,
    request_counter_window: AtomicU64, // Requests counted during the current one-second window.
    current_rps: AtomicU64,            // Completed one-second request count used for sampling.
}

pub struct EvalJob {
    pub request_id: String,
    pub session_id: String,
    pub resource_url: String,
    pub method: String,
    pub trigger: String,
    pub request_json: Value,
    pub response_json: Value,
}

impl ProxyState {
    pub fn new(
        upstream_base: Uri,
        rules: Arc<RwLock<Arc<RuleSet>>>,
        state_tracker: Arc<StateTracker>,
        anomaly_store: Arc<AnomalyStore>,
    ) -> (Arc<Self>, mpsc::Receiver<EvalJob>) {
        let (tx, rx) = mpsc::channel(EVAL_CHANNEL_CAPACITY);
        let state = Arc::new(Self {
            upstream_base,
            client: Client::new(),
            rules,
            state_tracker,
            anomaly_store,
            sender: tx,
            request_counter_window: AtomicU64::new(0),
            current_rps: AtomicU64::new(0),
        });
        (state, rx)
    }

    /// أخذ عينات تكيفي — القسم 3.2 المُصحح:
    /// - المسارات الحرجة تحتفظ بمعدل ثابت مرتفع (20%) بصرف النظر عن الحمل.
    /// - غير الحرجة تُخفَّض فقط عند الحمل العالي.
    fn should_sample(&self, critical: bool) -> bool {
        let current_rps = self.current_rps.load(Ordering::Relaxed);
        fastrand_u64() % 100 < sample_rate_percent(critical, current_rps)
    }
}

fn sample_rate_percent(critical: bool, current_rps: u64) -> u64 {
    if critical {
        CRITICAL_SAMPLE_RATE_PERCENT
    } else if current_rps > LOW_LOAD_THRESHOLD_RPS {
        NORMAL_HIGH_LOAD_SAMPLE_RATE_PERCENT
    } else {
        100
    }
}

/// Maintain a one-second request-rate estimate without blocking request handling.
pub async fn run_sampling_rate_monitor(proxy: Arc<ProxyState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.tick().await; // Skip the immediate first tick so the first window can accumulate.
    loop {
        interval.tick().await;
        let requests = proxy.request_counter_window.swap(0, Ordering::Relaxed);
        proxy.current_rps.store(requests, Ordering::Relaxed);
    }
}

fn fastrand_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
}

fn extract_session_id(req: &Request<Body>) -> String {
    req.headers()
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "anonymous".to_string())
}

/// نقطة الدخول الرئيسية للاعتراض: تُستدعى لكل طلب وارد على الـ sidecar.
pub async fn handle(
    proxy: Arc<ProxyState>,
    req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    proxy.request_counter_window.fetch_add(1, Ordering::Relaxed);
    let request_id = Uuid::new_v4().to_string();
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let session_id = extract_session_id(&req);
    let trigger = format!("{method} {path}");

    // 1) اقرأ جسم الطلب (buffered) للحاجة لاحقًا في التقييم والتوجيه معًا
    let (parts, body) = req.into_parts();
    let body_bytes = match hyper::body::to_bytes(body).await {
        Ok(b) => b,
        Err(_) => Bytes::new(),
    };
    let request_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

    // 2) وجّه الطلب فعليًا إلى الخدمة الأصلية (upstream)
    let upstream_uri = build_upstream_uri(&proxy.upstream_base, &parts.uri);
    let mut upstream_req = Request::builder()
        .method(parts.method.clone())
        .uri(upstream_uri)
        .body(Body::from(body_bytes.clone()))
        .unwrap();
    *upstream_req.headers_mut() = parts.headers.clone();

    let upstream_resp = match proxy.client.request(upstream_req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "فشل الاتصال بالخدمة الأصلية");
            return Ok(Response::builder()
                .status(502)
                .body(Body::from("Bad Gateway"))
                .unwrap());
        }
    };

    let (resp_parts, resp_body) = upstream_resp.into_parts();
    let resp_bytes = hyper::body::to_bytes(resp_body).await.unwrap_or_default();

    // 3) مرّر الرد للمستخدم فورًا — لا ننتظر أي تحقق (القسم 3.1: Non-Blocking Guarantee)
    let response_for_client = Response::from_parts(resp_parts, Body::from(resp_bytes.clone()));

    // 4) بعد بناء الرد للإرسال، أرسل نسخة للتقييم بشكل غير حاجز تمامًا (best-effort)
    let response_json: Value = serde_json::from_slice(&resp_bytes).unwrap_or(Value::Null);
    let rules_snapshot = proxy.rules.read().clone();
    let is_critical = rules_snapshot.matching(&trigger).iter().any(|r| r.critical);

    if proxy.should_sample(is_critical) {
        let job = EvalJob {
            request_id,
            session_id,
            resource_url: path,
            method,
            trigger,
            request_json,
            response_json,
        };
        // try_send: إن كانت القناة ممتلئة، تُسقط العينة فورًا دون أي انتظار
        if proxy.sender.try_send(job).is_err() {
            metrics::SAMPLES_DROPPED_TOTAL.inc();
        }
    }

    Ok(response_for_client)
}

fn build_upstream_uri(base: &Uri, incoming: &Uri) -> Uri {
    let path_and_query = incoming
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    format!("{}{}", base, path_and_query)
        .parse()
        .unwrap_or_else(|_| base.clone())
}

/// حلقة العامل (worker loop): تستهلك مهام التقييم من القناة وتنفّذها بالكامل
/// بعد أن يكون الرد قد أُرسل للمستخدم بالفعل — هذا ما يضمن "صفر تأخير" على المسار الحرج.
pub async fn run_evaluator_worker(
    mut rx: mpsc::Receiver<EvalJob>,
    rules: Arc<RwLock<Arc<RuleSet>>>,
    state_tracker: Arc<StateTracker>,
    anomaly_store: Arc<AnomalyStore>,
) {
    while let Some(job) = rx.recv().await {
        let timer = metrics::LATENCY_SECONDS.start_timer();
        metrics::CHECKS_TOTAL.inc();

        let rules_snapshot = rules.read().clone();
        let matched = rules_snapshot.matching(&job.trigger);

        let ctx = EvalContext {
            request: &job.request_json,
            response: &job.response_json,
        };

        for rule in matched {
            for constraint in &rule.constraints {
                match logic::eval(&constraint.ast, &ctx) {
                    Ok(Value::Bool(true)) => { /* القيد محقق، لا شيء للتسجيل */
                    }
                    Ok(Value::Bool(false)) => {
                        metrics::MISMATCH_TOTAL.inc();
                        let rec = AnomalyRecord {
                            request_id: job.request_id.clone(),
                            rule_id: rule.rule_id.clone(),
                            constraint_id: constraint.id.clone(),
                            severity: constraint.severity.clone(),
                            session_id: job.session_id.clone(),
                            details: format!(
                                "انتهاك القيد '{}' على {}",
                                constraint.id, job.trigger
                            ),
                            timestamp: chrono::Utc::now().timestamp(),
                        };
                        if let Err(e) = anomaly_store.insert_anomaly(&rec) {
                            tracing::error!(error = %e, "فشل تسجيل الشذوذ");
                        }
                        tracing::warn!(
                            rule_id = %rule.rule_id,
                            constraint = %constraint.id,
                            severity = %constraint.severity,
                            request_id = %job.request_id,
                            "تم اكتشاف شذوذ منطقي"
                        );
                    }
                    Ok(_) => tracing::debug!("نتيجة تقييم غير منطقية (bool) — تُتجاهل"),
                    Err(e) => tracing::error!(error = %e, "خطأ في تقييم القيد"),
                }
            }
        }

        // تحديث سجل الحالة للتتبع المسبب — بعد إرسال الرد بالكامل، لا يؤثر على أي زمن استجابة
        state_tracker.record(
            &job.session_id,
            &job.resource_url,
            job.request_id.clone(),
            job.request_json,
            job.response_json,
        );

        timer.observe_duration();
    }
}

#[cfg(test)]
mod sampling_tests {
    use super::{
        sample_rate_percent, CRITICAL_SAMPLE_RATE_PERCENT, LOW_LOAD_THRESHOLD_RPS,
        NORMAL_HIGH_LOAD_SAMPLE_RATE_PERCENT,
    };

    #[test]
    fn critical_sampling_rate_is_fixed_at_all_loads() {
        assert_eq!(sample_rate_percent(true, 0), CRITICAL_SAMPLE_RATE_PERCENT);
        assert_eq!(
            sample_rate_percent(true, LOW_LOAD_THRESHOLD_RPS + 1),
            CRITICAL_SAMPLE_RATE_PERCENT
        );
    }

    #[test]
    fn noncritical_sampling_is_full_at_or_below_threshold() {
        assert_eq!(sample_rate_percent(false, 0), 100);
        assert_eq!(sample_rate_percent(false, LOW_LOAD_THRESHOLD_RPS), 100);
    }

    #[test]
    fn noncritical_sampling_is_reduced_above_threshold() {
        assert_eq!(
            sample_rate_percent(false, LOW_LOAD_THRESHOLD_RPS + 1),
            NORMAL_HIGH_LOAD_SAMPLE_RATE_PERCENT
        );
    }
}
