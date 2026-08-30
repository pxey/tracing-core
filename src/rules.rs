//! تحميل وتفسير ملفات .ctc.json وتحويلها إلى قواعد مُصرّحة (compiled) عند الإقلاع
//! وعند كل استدعاء لـ POST /rules/reload — دون إعادة تشغيل الخدمة.

use crate::logic::{self, Expr};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct RawConstraint {
    pub id: String,
    pub assert: Value,
    #[serde(default = "default_severity")]
    pub severity: String,
    /// حقول تُعتبر حساسة ويجب تنقيحها (redact) عند إرجاعها عبر /trace
    #[serde(default)]
    pub sensitive_fields: Vec<String>,
}

fn default_severity() -> String {
    "warning".into()
}

#[derive(Debug, Deserialize)]
pub struct RawRule {
    pub rule_id: String,
    pub trigger: String,
    pub constraints: Vec<RawConstraint>,
    #[serde(default)]
    pub critical: bool,
}

/// قاعدة مُصرّحة: كل constraint تحوَّل مسبقًا إلى AST مرة واحدة عند التحميل.
pub struct CompiledConstraint {
    pub id: String,
    pub ast: Expr,
    pub severity: String,
    pub sensitive_fields: Vec<String>,
}

pub struct CompiledRule {
    pub rule_id: String,
    pub trigger: String,
    pub critical: bool,
    pub constraints: Vec<CompiledConstraint>,
}

/// مجموعة القواعد الحالية، مفهرسة حسب trigger لتسريع المطابقة وقت التشغيل.
pub struct RuleSet {
    pub by_trigger: HashMap<String, Vec<CompiledRule>>,
    pub version: u64,
}

impl RuleSet {
    pub fn matching(&self, trigger: &str) -> &[CompiledRule] {
        self.by_trigger
            .get(trigger)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

/// يقرأ كل ملفات *.ctc.json من مجلد، يتحقق من صحتها، ويُصرّفها إلى AST.
/// يُستدعى عند الإقلاع وعند POST /rules/reload.
pub fn load_rules_dir(dir: &Path, prev_version: u64) -> Result<RuleSet> {
    let mut by_trigger: HashMap<String, Vec<CompiledRule>> = HashMap::new();
    let mut file_count = 0usize;

    if !dir.exists() {
        anyhow::bail!("مجلد القواعد غير موجود: {:?}", dir);
    }

    for entry in std::fs::read_dir(dir).context("تعذّرت قراءة مجلد القواعد")? {
        let entry = entry?;
        let path = entry.path();
        let is_rule_file = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(".ctc.json"))
            .unwrap_or(false);
        if !is_rule_file {
            continue;
        }

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("تعذّرت قراءة {:?}", path))?;
        let raw: RawRule = serde_json::from_str(&content)
            .with_context(|| format!("صيغة غير صالحة في {:?}", path))?;

        let mut compiled_constraints = Vec::new();
        for c in raw.constraints {
            let ast = logic::compile(&c.assert)
                .with_context(|| format!("قيد '{}' في القاعدة '{}' غير قابل للتصريف", c.id, raw.rule_id))?;
            compiled_constraints.push(CompiledConstraint {
                id: c.id,
                ast,
                severity: c.severity,
                sensitive_fields: c.sensitive_fields,
            });
        }

        let compiled_rule = CompiledRule {
            rule_id: raw.rule_id,
            trigger: raw.trigger.clone(),
            critical: raw.critical,
            constraints: compiled_constraints,
        };

        by_trigger.entry(raw.trigger).or_default().push(compiled_rule);
        file_count += 1;
    }

    tracing::info!(files = file_count, "تم تحميل وتصريف القواعد بنجاح");

    Ok(RuleSet {
        by_trigger,
        version: prev_version + 1,
    })
}
