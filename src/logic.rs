//! محرك تقييم JSON Logic
//!
//! يحوّل القواعد المُعرّفة في ملفات .ctc.json إلى شجرة AST مرة واحدة عند التحميل
//! (compile-time بالنسبة للقاعدة)، ثم يُنفّذها لاحقًا وقت التشغيل دون أي تفسير نصوص حي.
//! هذا يزيل الاعتماد على مُفسّر تعبيرات نصية خام كما كانت في النسخة v1.

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LogicError {
    #[error("عامل غير مدعوم: {0}")]
    UnsupportedOperator(String),
    #[error("شكل تعبير غير صالح: {0}")]
    MalformedExpression(String),
    #[error("نوع بيانات غير متوافق في: {0}")]
    TypeMismatch(String),
}

/// شجرة التعبير المُحوَّلة مسبقًا (AST).
/// كل قاعدة تُحوَّل إلى هذا الشكل مرة واحدة فقط عند /rules/reload.
#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Value),
    Var { path: String, default: Option<Box<Expr>> },
    Eq(Box<Expr>, Box<Expr>),
    Lte(Box<Expr>, Box<Expr>),
    Gte(Box<Expr>, Box<Expr>),
    Lt(Box<Expr>, Box<Expr>),
    Gt(Box<Expr>, Box<Expr>),
    Add(Vec<Expr>),
    Sub(Vec<Expr>),
    Mul(Vec<Expr>),
    AbsDiff(Box<Expr>, Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    If(Box<Expr>, Box<Expr>, Box<Expr>),
}

/// سياق التقييم: يحتوي على جسم الطلب والاستجابة كـ DOM (serde_json::Value)
pub struct EvalContext<'a> {
    pub request: &'a Value,
    pub response: &'a Value,
}

impl<'a> EvalContext<'a> {
    fn resolve_var(&self, path: &str) -> Option<Value> {
        let mut parts = path.splitn(2, '.');
        let root = parts.next()?;
        let rest = parts.next().unwrap_or("");
        let base = match root {
            "request" => self.request,
            "response" => self.response,
            _ => return None,
        };
        if rest.is_empty() {
            return Some(base.clone());
        }
        let mut current = base;
        for segment in rest.split('.') {
            current = current.get(segment)?;
        }
        Some(current.clone())
    }
}

/// تحويل تعريف JSON Logic خام إلى AST. يُنفَّذ مرة واحدة فقط عند تحميل القاعدة.
pub fn compile(value: &Value) -> Result<Expr, LogicError> {
    match value {
        Value::Object(map) if map.len() == 1 => {
            let (op, arg) = map.iter().next().unwrap();
            compile_op(op, arg)
        }
        // قيمة حرفية (رقم، نص، منطقي) تُعامل كـ Literal مباشرة
        other => Ok(Expr::Literal(other.clone())),
    }
}

fn compile_op(op: &str, arg: &Value) -> Result<Expr, LogicError> {
    match op {
        "var" => {
            // يدعم الشكلين: {"var": "a.b"} أو {"var": ["a.b"]} أو
            // {"var": "a.b", "default": ...} يُمرَّر كمفتاح شقيق في القاعدة الأصلية.
            match arg {
                Value::String(s) => Ok(Expr::Var { path: s.clone(), default: None }),
                Value::Array(arr) if !arr.is_empty() => {
                    let path = arr[0].as_str().ok_or_else(|| {
                        LogicError::MalformedExpression("var: المسار يجب أن يكون نصًا".into())
                    })?;
                    let default = if arr.len() > 1 {
                        Some(Box::new(compile(&arr[1])?))
                    } else {
                        None
                    };
                    Ok(Expr::Var { path: path.to_string(), default })
                }
                _ => Err(LogicError::MalformedExpression("var: شكل غير مدعوم".into())),
            }
        }
        "==" => bin_op(arg, Expr::Eq),
        "<=" => bin_op(arg, Expr::Lte),
        ">=" => bin_op(arg, Expr::Gte),
        "<" => bin_op(arg, Expr::Lt),
        ">" => bin_op(arg, Expr::Gt),
        "+" => Ok(Expr::Add(compile_array(arg)?)),
        "-" => Ok(Expr::Sub(compile_array(arg)?)),
        "*" => Ok(Expr::Mul(compile_array(arg)?)),
        "and" => Ok(Expr::And(compile_array(arg)?)),
        "or" => Ok(Expr::Or(compile_array(arg)?)),
        "abs_diff" => bin_op(arg, Expr::AbsDiff),
        "if" => {
            let arr = arg.as_array().ok_or_else(|| {
                LogicError::MalformedExpression("if: يتطلب مصفوفة من 3 عناصر".into())
            })?;
            if arr.len() != 3 {
                return Err(LogicError::MalformedExpression("if: يتطلب [شرط, صح, خطأ]".into()));
            }
            Ok(Expr::If(
                Box::new(compile(&arr[0])?),
                Box::new(compile(&arr[1])?),
                Box::new(compile(&arr[2])?),
            ))
        }
        other => Err(LogicError::UnsupportedOperator(other.to_string())),
    }
}

fn bin_op(arg: &Value, ctor: fn(Box<Expr>, Box<Expr>) -> Expr) -> Result<Expr, LogicError> {
    let arr = arg
        .as_array()
        .ok_or_else(|| LogicError::MalformedExpression("عملية ثنائية تتطلب مصفوفة".into()))?;
    if arr.len() != 2 {
        return Err(LogicError::MalformedExpression("عملية ثنائية تتطلب عنصرين بالضبط".into()));
    }
    Ok(ctor(Box::new(compile(&arr[0])?), Box::new(compile(&arr[1])?)))
}

fn compile_array(arg: &Value) -> Result<Vec<Expr>, LogicError> {
    let arr = arg
        .as_array()
        .ok_or_else(|| LogicError::MalformedExpression("عملية تتطلب مصفوفة عناصر".into()))?;
    arr.iter().map(compile).collect()
}

fn as_f64(v: &Value) -> Result<f64, LogicError> {
    v.as_f64()
        .ok_or_else(|| LogicError::TypeMismatch(format!("{:?} ليس رقمًا", v)))
}

/// تنفيذ الـ AST المُحوَّل مسبقًا مقابل سياق طلب/استجابة فعلي.
/// هذه الدالة هي الوحيدة التي تُستدعى وقت التشغيل الفعلي — لا تفسير نصوص هنا.
pub fn eval(expr: &Expr, ctx: &EvalContext) -> Result<Value, LogicError> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Var { path, default } => match ctx.resolve_var(path) {
            Some(v) if !v.is_null() => Ok(v),
            _ => match default {
                Some(d) => eval(d, ctx),
                None => Ok(Value::Null),
            },
        },
        Expr::Eq(a, b) => Ok(Value::Bool(eval(a, ctx)? == eval(b, ctx)?)),
        Expr::Lte(a, b) => Ok(Value::Bool(as_f64(&eval(a, ctx)?)? <= as_f64(&eval(b, ctx)?)?)),
        Expr::Gte(a, b) => Ok(Value::Bool(as_f64(&eval(a, ctx)?)? >= as_f64(&eval(b, ctx)?)?)),
        Expr::Lt(a, b) => Ok(Value::Bool(as_f64(&eval(a, ctx)?)? < as_f64(&eval(b, ctx)?)?)),
        Expr::Gt(a, b) => Ok(Value::Bool(as_f64(&eval(a, ctx)?)? > as_f64(&eval(b, ctx)?)?)),
        Expr::Add(items) => {
            let mut sum = 0.0;
            for i in items {
                sum += as_f64(&eval(i, ctx)?)?;
            }
            Ok(serde_json::json!(sum))
        }
        Expr::Sub(items) => {
            let mut iter = items.iter();
            let first = iter
                .next()
                .ok_or_else(|| LogicError::MalformedExpression("-: تتطلب عنصرًا واحدًا على الأقل".into()))?;
            let mut acc = as_f64(&eval(first, ctx)?)?;
            for i in iter {
                acc -= as_f64(&eval(i, ctx)?)?;
            }
            Ok(serde_json::json!(acc))
        }
        Expr::Mul(items) => {
            let mut prod = 1.0;
            for i in items {
                prod *= as_f64(&eval(i, ctx)?)?;
            }
            Ok(serde_json::json!(prod))
        }
        Expr::AbsDiff(a, b) => {
            let x = as_f64(&eval(a, ctx)?)?;
            let y = as_f64(&eval(b, ctx)?)?;
            Ok(serde_json::json!((x - y).abs()))
        }
        Expr::And(items) => {
            for i in items {
                if !eval(i, ctx)?.as_bool().unwrap_or(false) {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        Expr::Or(items) => {
            for i in items {
                if eval(i, ctx)?.as_bool().unwrap_or(false) {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }
        Expr::If(cond, then_e, else_e) => {
            if eval(cond, ctx)?.as_bool().unwrap_or(false) {
                eval(then_e, ctx)
            } else {
                eval(else_e, ctx)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_stock_not_oversold() {
        let rule = json!({"<=": [{"var": "response.stock"}, {"var": "request.quantity"}]});
        let ast = compile(&rule).unwrap();
        let request = json!({"quantity": 5});
        let response = json!({"stock": 3});
        let ctx = EvalContext { request: &request, response: &response };
        assert_eq!(eval(&ast, &ctx).unwrap(), json!(true));
    }

    #[test]
    fn test_total_matches_calculation() {
        let rule = json!({
            "<=": [
                {"abs_diff": [
                    {"var": "response.total"},
                    {"-": [
                        {"*": [{"var": "request.price"}, {"var": "request.quantity"}]},
                        {"var": ["request.discount", 0]}
                    ]}
                ]},
                0.01
            ]
        });
        let ast = compile(&rule).unwrap();
        let request = json!({"price": 10.0, "quantity": 3, "discount": 2.0});
        let response = json!({"total": 28.0});
        let ctx = EvalContext { request: &request, response: &response };
        assert_eq!(eval(&ast, &ctx).unwrap(), json!(true));

        let response_bad = json!({"total": 999.0});
        let ctx_bad = EvalContext { request: &request, response: &response_bad };
        assert_eq!(eval(&ast, &ctx_bad).unwrap(), json!(false));
    }
}
