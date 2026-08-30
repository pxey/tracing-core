use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// عنوان الخدمة الأصلية التي يُنشر الـ sidecar بجوارها
    pub upstream_url: String,
    /// المنفذ الذي يستمع عليه الـ sidecar لاعتراض الطلبات (يُوجَّه إليه traffic بدل الخدمة مباشرة)
    pub listen_addr: String,
    /// منفذ Prometheus (القسم 5)
    pub metrics_addr: String,
    /// منفذ Admin API — يجب أن يكون خلف mTLS في الإنتاج (القسم 5 المُصحح)
    pub admin_addr: String,
    pub rules_dir: PathBuf,
    pub sqlite_path: PathBuf,
    pub audit_log_path: PathBuf,
    pub reload_shared_secret: String,
}

impl Config {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&content)?;
        Ok(cfg)
    }

    pub fn default_for_dev() -> Self {
        Self {
            upstream_url: "http://127.0.0.1:8080".into(),
            listen_addr: "0.0.0.0:9000".into(),
            metrics_addr: "0.0.0.0:9090".into(),
            admin_addr: "127.0.0.1:9091".into(),
            rules_dir: PathBuf::from("./rules"),
            sqlite_path: PathBuf::from("./ctc_anomalies.db"),
            audit_log_path: PathBuf::from("./ctc_audit.log"),
            reload_shared_secret: "change-me-in-production".into(),
        }
    }
}
