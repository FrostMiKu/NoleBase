//! Deterministic arithmetic and scientific expression evaluation.

use anyhow::{Context as _, Result};
use serde_json::{json, Value};

use crate::agent::Tool;

pub struct Calculate;

fn evaluation_context() -> meval::Context<'static> {
    let mut context = meval::Context::new();
    context
        .var("tau", std::f64::consts::TAU)
        .func("cbrt", f64::cbrt)
        .func("trunc", f64::trunc)
        .func("log", f64::log10)
        .func("log2", f64::log2)
        .func2("pow", f64::powf)
        .func2("logb", |base, value| value.log(base))
        .func2("hypot", f64::hypot);
    context
}
fn normalize_leading_decimal(expression: &str) -> std::borrow::Cow<'_, str> {
    let bytes = expression.as_bytes();
    let mut output = String::new();
    let mut copied_through = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'.'
            && bytes.get(index + 1).is_some_and(u8::is_ascii_digit)
            && (index == 0
                || matches!(
                    bytes[index - 1],
                    b'+' | b'-' | b'*' | b'/' | b'%' | b'^' | b'(' | b','
                ))
        {
            output.push_str(&expression[copied_through..index]);
            output.push('0');
            copied_through = index;
        }
    }
    if output.is_empty() {
        std::borrow::Cow::Borrowed(expression)
    } else {
        output.push_str(&expression[copied_through..]);
        std::borrow::Cow::Owned(output)
    }
}

fn format_result(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value == f64::INFINITY {
        return "inf".to_string();
    }
    if value == f64::NEG_INFINITY {
        return "-inf".to_string();
    }

    let magnitude = value.abs();
    if value != 0.0 && !(1e-4..1e16).contains(&magnitude) {
        format!("{value:e}")
    } else {
        value.to_string()
    }
}

#[async_trait::async_trait]
impl Tool for Calculate {
    fn name(&self) -> &'static str {
        "calculate"
    }

    fn description(&self) -> &'static str {
        "Evaluate one arithmetic or scientific expression with IEEE 754 double-precision semantics. Supports parentheses, +, -, *, /, %, ^, pi, e, tau, and common math functions; trigonometric angles are radians."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "One expression, for example: sqrt(3^2 + 4^2)",
                    "minLength": 1,
                    "maxLength": 1000
                }
            },
            "required": ["expression"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let expression = input
            .get("expression")
            .and_then(Value::as_str)
            .context("field expression must be a string")?
            .trim();
        if expression.is_empty() {
            anyhow::bail!("expression must not be empty");
        }
        if expression.chars().count() > 1000 {
            anyhow::bail!("expression must not exceed 1000 characters");
        }

        let expression = normalize_leading_decimal(expression);
        meval::eval_str_with_context(expression.as_ref(), evaluation_context())
            .map(format_result)
            .map_err(|error| anyhow::anyhow!("invalid expression: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calculate(expression: &str) -> Result<String> {
        crate::agent::test_support::test_runtime()
            .block_on(Calculate.execute(&json!({"expression": expression})))
    }

    #[test]
    fn evaluates_arithmetic_scientific_notation_and_ieee_precision() {
        for (expression, expected) in [
            ("2 - 3*4", "-10"),
            ("2^3", "8"),
            (".5 + 1.5e2", "150.5"),
            ("0.1 + 0.2", "0.30000000000000004"),
        ] {
            assert_eq!(calculate(expression).unwrap(), expected, "{expression}");
        }
    }

    #[test]
    fn evaluates_builtin_and_extended_scientific_functions() {
        for (expression, expected) in [
            ("sqrt(3^2 + 4^2)", "5"),
            ("sin(pi/2)", "1"),
            ("pow(2, 10)", "1024"),
            ("log(1000)", "3"),
            ("logb(2, 8)", "3"),
            ("hypot(3, 4)", "5"),
            ("tau", "6.283185307179586"),
        ] {
            assert_eq!(calculate(expression).unwrap(), expected, "{expression}");
        }
    }

    #[test]
    fn formats_scientific_values_and_negative_zero_stably() {
        for (expression, expected) in [
            ("sin(pi)", "1.2246467991473532e-16"),
            ("pow(2, 100)", "1.2676506002282294e30"),
            ("-0", "-0"),
        ] {
            assert_eq!(calculate(expression).unwrap(), expected, "{expression}");
        }
    }

    #[test]
    fn returns_ieee_non_finite_results() {
        for (expression, expected) in [
            ("1/0", "inf"),
            ("-1/0", "-inf"),
            ("0/0", "NaN"),
            ("sqrt(-1)", "NaN"),
        ] {
            assert_eq!(calculate(expression).unwrap(), expected, "{expression}");
        }
    }

    #[test]
    fn rejects_empty_oversized_and_invalid_expressions() {
        assert_eq!(
            calculate(" \n\t ").unwrap_err().to_string(),
            "expression must not be empty"
        );
        assert_eq!(
            calculate(&"1".repeat(1001)).unwrap_err().to_string(),
            "expression must not exceed 1000 characters"
        );
        for expression in ["unknown", "missing(1)", "2 +"] {
            assert!(
                calculate(expression)
                    .unwrap_err()
                    .to_string()
                    .starts_with("invalid expression: "),
                "{expression}"
            );
        }
    }

    #[test]
    fn schema_requires_one_bounded_expression() {
        let schema = Calculate.input_schema();
        assert_eq!(schema["required"], json!(["expression"]));
        assert_eq!(schema["properties"]["expression"]["minLength"], 1);
        assert_eq!(schema["properties"]["expression"]["maxLength"], 1000);
        assert_eq!(schema["additionalProperties"], false);
    }
}
