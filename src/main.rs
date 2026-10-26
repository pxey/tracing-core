mod admin;
mod config;
mod cross_instance;
mod logic;
mod metrics;
mod proxy;
mod resource_guard;
mod rules;
mod state;
mod storage;

use axum::routing::{get, post};
use axum::Router;
use hyper::service::{make_service_fn, service_fn};
use hyper::Server;
use parking_lot::RwLock;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("ctc=info".parse()?))
        .init();

    metrics::init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config/ctc.config.toml".to_string());
    let cfg = config::Config::load(std::path::Path::new(&config_path))
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "تعذّر تحميل ملف الإعدادات، استخدام إعدادات افتراضية للتطوير");
            config::Config::default_for_dev()
        });

    tracing::info!(?cfg.listen_addr, ?cfg.upstream_url, "بدء تشغيل CTC sidecar");

    // ---- تحميل القواعد الأولي (القسم 2) ----
    let initial_rules = rules::load_rules_dir(&cfg.rules_dir, 0)?;
    let rules_shared = Arc::new(RwLock::new(Arc::new(initial_rules)));

    // ---- تخزين الشذوذ (القسم 4) ----
    let anomaly_store = Arc::new(storage::AnomalyStore::open(&cfg.sqlite_path)?);
    {
        let store_for_retention = anomaly_store.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                if let Err(e) = store_for_retention.enforce_retention() {
                    tracing::error!(error = %e, "فشل تطبيق سياسة الاحتفاظ");
                }
            }
        });
    }

    // ---- متعقب الحالة (القسم 4) ----
    let state_tracker = Arc::new(state::StateTracker::new());

    // ---- مراقب الموارد الذاتية (القسم 6 المُصحح) ----
    let austerity = Arc::new(resource_guard::AusterityMode::new());
    tokio::spawn(resource_guard::run_resource_monitor(austerity.clone()));

    // ---- بناء حالة الـ proxy وقناة التقييم غير الحاجزة (القسم 3.1) ----
    let upstream_uri: hyper::Uri = cfg.upstream_url.parse()?;
    let (proxy_state, eval_rx) = proxy::ProxyState::new(
        upstream_uri,
        rules_shared.clone(),
        state_tracker.clone(),
        anomaly_store.clone(),
    );

    tokio::spawn(proxy::run_evaluator_worker(
        eval_rx,
        rules_shared.clone(),
        state_tracker.clone(),
        anomaly_store.clone(),
    ));

    // ---- خادم الـ sidecar الاعتراضي (Hyper) ----
    let proxy_addr: SocketAddr = cfg.listen_addr.parse()?;
    let proxy_state_for_server = proxy_state.clone();
    let make_svc = make_service_fn(move |_conn| {
        let proxy_state = proxy_state_for_server.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                proxy::handle(proxy_state.clone(), req)
            }))
        }
    });
    let proxy_server = Server::bind(&proxy_addr).serve(make_svc);

    // ---- خادم المقاييس (القسم 5) ----
    let metrics_addr: SocketAddr = cfg.metrics_addr.parse()?;
    let metrics_app = Router::new().route(
        "/metrics",
        get(|| async { metrics::render() }),
    );
    let metrics_listener = tokio::net::TcpListener::bind(&metrics_addr).await?;
    let metrics_server = async move { axum::serve(metrics_listener, metrics_app).await };

    // ---- خادم الإدارة (القسم 5 المُصحح: يجب حمايته بـ mTLS في الإنتاج) ----
    let admin_state = Arc::new(admin::AdminState {
        rules: rules_shared.clone(),
        rules_dir: cfg.rules_dir.clone(),
        anomaly_store: anomaly_store.clone(),
        audit_log_path: cfg.audit_log_path.clone(),
        reload_shared_secret: cfg.reload_shared_secret.clone(),
    });
    let admin_addr: SocketAddr = cfg.admin_addr.parse()?;
    let admin_app = Router::new()
        .route("/rules/reload", post(admin::reload_rules))
        .route("/trace/:request_id", get(admin::get_trace))
        .with_state(admin_state);
    let admin_listener = tokio::net::TcpListener::bind(&admin_addr).await?;
    let admin_server = async move { axum::serve(admin_listener, admin_app).await };

    tracing::info!(
        proxy = %proxy_addr,
        metrics = %metrics_addr,
        admin = %admin_addr,
        "⚠️  admin_addr يجب أن يكون خلف mTLS/شبكة داخلية في بيئة الإنتاج"
    );

    tokio::select! {
        r = proxy_server => { if let Err(e) = r { tracing::error!(error = %e, "توقف خادم الـ proxy"); } }
        r = metrics_server => { if let Err(e) = r { tracing::error!(error = %e, "توقف خادم المقاييس"); } }
        r = admin_server => { if let Err(e) = r { tracing::error!(error = %e, "توقف خادم الإدارة"); } }
    }

    Ok(())
}
