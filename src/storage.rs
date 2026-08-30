//! تخزين حالات الشذوذ (Anomalies) فقط في SQLite، مع سياسة احتفاظ 7 أيام
//! وحجم أقصى 100MB، بالإضافة لجدول تتبع تصادمات التجزئة (القسم 4 و7).

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;

const RETENTION_DAYS: i64 = 7;
const MAX_DB_SIZE_BYTES: u64 = 100 * 1024 * 1024; // 100MB

pub struct AnomalyStore {
    conn: Mutex<Connection>,
    db_path: std::path::PathBuf,
}

#[derive(Debug, Serialize)]
pub struct AnomalyRecord {
    pub request_id: String,
    pub rule_id: String,
    pub constraint_id: String,
    pub severity: String,
    pub session_id: String,
    pub details: String,
    pub timestamp: i64,
}

impl AnomalyStore {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("تعذّر فتح قاعدة بيانات SQLite")?;
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS anomalies (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                request_id TEXT NOT NULL,
                rule_id TEXT NOT NULL,
                constraint_id TEXT NOT NULL,
                severity TEXT NOT NULL,
                session_id TEXT NOT NULL,
                details TEXT NOT NULL,
                timestamp INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_anomalies_request_id ON anomalies(request_id);
            CREATE INDEX IF NOT EXISTS idx_anomalies_timestamp ON anomalies(timestamp);

            CREATE TABLE IF NOT EXISTS hash_collisions_detected (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                transaction_id TEXT NOT NULL,
                hash_a TEXT NOT NULL,
                hash_b TEXT NOT NULL,
                confirmed_real_mismatch BOOLEAN NOT NULL,
                timestamp INTEGER NOT NULL
            );
            ",
        )
        .context("تعذّر إنشاء الجداول")?;

        Ok(Self {
            conn: Mutex::new(conn),
            db_path: path.to_path_buf(),
        })
    }

    pub fn insert_anomaly(&self, rec: &AnomalyRecord) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO anomalies (request_id, rule_id, constraint_id, severity, session_id, details, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                rec.request_id,
                rec.rule_id,
                rec.constraint_id,
                rec.severity,
                rec.session_id,
                rec.details,
                rec.timestamp
            ],
        )?;
        Ok(())
    }

    pub fn insert_hash_collision(
        &self,
        transaction_id: &str,
        hash_a: &str,
        hash_b: &str,
        confirmed_real_mismatch: bool,
        timestamp: i64,
    ) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO hash_collisions_detected (transaction_id, hash_a, hash_b, confirmed_real_mismatch, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![transaction_id, hash_a, hash_b, confirmed_real_mismatch, timestamp],
        )?;
        Ok(())
    }

    pub fn find_by_request_id(&self, request_id: &str) -> Result<Vec<AnomalyRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT request_id, rule_id, constraint_id, severity, session_id, details, timestamp
             FROM anomalies WHERE request_id = ?1 ORDER BY timestamp ASC",
        )?;
        let rows = stmt.query_map(params![request_id], |row| {
            Ok(AnomalyRecord {
                request_id: row.get(0)?,
                rule_id: row.get(1)?,
                constraint_id: row.get(2)?,
                severity: row.get(3)?,
                session_id: row.get(4)?,
                details: row.get(5)?,
                timestamp: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().context("فشل استرجاع السجلات")
    }

    /// مهمة دورية: تحذف السجلات الأقدم من 7 أيام وتتحقق من حجم القاعدة.
    /// يجب استدعاؤها من تايمر منفصل (انظر main.rs) وليست على مسار أي طلب.
    pub fn enforce_retention(&self) -> Result<()> {
        let cutoff = chrono::Utc::now().timestamp() - RETENTION_DAYS * 24 * 3600;
        {
            let conn = self.conn.lock();
            conn.execute("DELETE FROM anomalies WHERE timestamp < ?1", params![cutoff])?;
            conn.execute(
                "DELETE FROM hash_collisions_detected WHERE timestamp < ?1",
                params![cutoff],
            )?;
            conn.execute_batch("VACUUM;")?;
        }

        if let Ok(meta) = std::fs::metadata(&self.db_path) {
            if meta.len() > MAX_DB_SIZE_BYTES {
                tracing::warn!(
                    size_bytes = meta.len(),
                    "قاعدة بيانات الشذوذ تجاوزت 100MB رغم سياسة الاحتفاظ — يُنصح بتقليل مدة الاحتفاظ"
                );
            }
        }
        Ok(())
    }
}
