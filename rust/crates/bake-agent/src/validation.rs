//! Tool-argument coercion and JSON-schema validation.
//!
//! Ported from Pi `packages/ai/src/utils/validation.ts` (v1.1.0), which the
//! agent loop calls before it runs a tool. Pi's steps are kept in order:
//! optional `null` members are dropped, primitives are coerced with Pi's
//! AJV-compatible rules (`"42"` to `42`, `"true"` to `true`, `null` to `""`,
//! and so on), and the result is checked against the schema. Pi checks with
//! TypeBox's compiled validator; this module carries a lean JSON-schema
//! validator instead of a dependency, because tool schemas are plain JSON in
//! Rust and Pi only needs a yes-or-no answer and readable error lines.
//!
//! The validator covers the keywords tool schemas use: boolean schemas,
//! local `$ref` (`#/...` JSON pointers, such as `$defs`), `type` (a name or
//! a list), `const`, `enum`, numeric bounds and `multipleOf`, string
//! lengths (in UTF-16 units, as JavaScript counts) and `pattern` (Rust
//! `regex` syntax), array `items` (a schema or a tuple), `prefixItems`,
//! `additionalItems`, item counts, `uniqueItems`, and `contains`, object
//! `required`, `properties`, `patternProperties`, `additionalProperties`,
//! property counts, and `propertyNames`, and `allOf`, `anyOf`, `oneOf`,
//! `not`, and `if`/`then`/`else`. `format` and other keywords are not
//! checked. Error lines follow TypeBox's `Errors`: its English messages, its
//! keyword order, the nested errors it reports for failed `anyOf`, `oneOf`,
//! `allOf`, `else`, `additionalProperties`, and `propertyNames` members, and
//! its limit of eight errors. Numbers in messages and numbers coerced to
//! strings use JavaScript's `Number.prototype.toString`. TypeBox's
//! `Value.Convert` step is not ported: for plain JSON schemas Pi's own
//! coercion covers it.
//!
//! Arguments come from the model and are untrusted, and schemas may come from
//! third-party tools: nothing here panics, recursion through `$ref` stops at a
//! fixed depth, and each call has a fixed budget of schema evaluations, so a
//! self-referencing schema that branches (`{"anyOf":[{"$ref":"#"},
//! {"$ref":"#"}]}`) fails with an error instead of running for ever. Pi fails
//! the same call with JavaScript's stack-overflow error. Each distinct
//! regular expression is compiled once per call, with a compiled-size limit,
//! and a call that still runs past a wall-clock limit fails with the same
//! error, so no schema holds the agent loop's task for long.
//!
//! A `pattern` that Rust's `regex` cannot compile, such as one with
//! lookaround or a backreference (valid in JavaScript), or one over the size
//! limit, is not checked: the string passes that keyword, as it passes
//! `format`, and the tool must check it itself. Likewise such a
//! `patternProperties` key matches no property. Pi would apply the pattern.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use bake_ai::{Tool, ToolCall};
use serde_json::{Map, Number, Value};

/// Deepest schema nesting the validator follows, `$ref` hops included.
const MAX_DEPTH: usize = 256;

/// Schema evaluations one call may spend across normalization, coercion,
/// and validation. Real tool arguments need a small fraction of it.
const STEP_BUDGET: usize = 500_000;

/// Wall-clock time one call may take before it fails as too complex. The
/// step budget normally ends a costly call well before this; the limit
/// bounds steps that cost more than usual, such as long regex matches.
const TIME_LIMIT: Duration = Duration::from_secs(1);

/// Compiled-program size limit for one `pattern` regex. Rust's default of
/// 10 MiB lets a pattern such as `\w{200}\w{200}` take tens of
/// milliseconds to compile; 1 MiB keeps every compile to a few.
const PATTERN_SIZE_LIMIT: usize = 1 << 20;

/// TypeBox's default `maxErrors`: errors kept per context.
const MAX_ERRORS: usize = 8;

/// Pi's `validateToolCall`: finds the tool by name and validates the call.
pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> Result<Value, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name == tool_call.name)
        .ok_or_else(|| format!("Tool \"{}\" not found", tool_call.name))?;
    validate_tool_arguments(
        &tool.parameters,
        &tool_call.name,
        &Value::Object(tool_call.arguments.clone()),
    )
}

/// Pi's `validateToolArguments`: the validated, possibly coerced, arguments
/// of a call to `tool_name`, or the error message Pi throws.
pub fn validate_tool_arguments(
    parameters: &Value,
    tool_name: &str,
    arguments: &Value,
) -> Result<Value, String> {
    let budget = Budget::new(STEP_BUDGET, TIME_LIMIT);
    let result = validate_within(parameters, tool_name, arguments, &budget);
    if budget.exhausted() {
        return Err(format!(
            "Validation failed for tool \"{tool_name}\": the schema is too complex or recursive to check"
        ));
    }
    result
}

fn validate_within(
    parameters: &Value,
    tool_name: &str,
    arguments: &Value,
    budget: &Budget,
) -> Result<Value, String> {
    let validator = Validator::new(parameters, budget);
    let mut args = arguments.clone();
    validator.normalize_optional_nulls(&mut args, parameters);

    let coerced = validator.coerce_with_json_schema(args.clone(), parameters);
    if args.is_object() && coerced.is_object() {
        args = coerced;
    } else if coerced != args {
        // Pi returns the uncoerced arguments unchecked when a non-object root
        // fails to coerce; tool parameters are objects, so this is rare.
        return Ok(if validator.check(parameters, &coerced, 0) {
            coerced
        } else {
            args
        });
    }

    if validator.check(parameters, &args, 0) {
        return Ok(args);
    }
    let mut errors = Errors::default();
    validator.errors(parameters, &args, "", 0, &mut errors);
    let lines = errors
        .list
        .iter()
        .map(|error| format!("  - {}: {}", error.path(), error.message))
        .collect::<Vec<_>>()
        .join("\n");
    let lines = if lines.is_empty() {
        "Unknown validation error".to_owned()
    } else {
        lines
    };
    let received = serde_json::to_string_pretty(arguments).unwrap_or_default();
    Err(format!(
        "Validation failed for tool \"{tool_name}\":\n{lines}\n\nReceived arguments:\n{received}"
    ))
}

/// The schema evaluations and the time a call has left.
struct Budget {
    remaining: Cell<usize>,
    exhausted: Cell<bool>,
    /// `None` when the limit is too far off to represent.
    deadline: Option<Instant>,
    timed_out: Cell<bool>,
}

impl Budget {
    fn new(steps: usize, time_limit: Duration) -> Self {
        Self {
            remaining: Cell::new(steps),
            exhausted: Cell::new(false),
            deadline: Instant::now().checked_add(time_limit),
            timed_out: Cell::new(false),
        }
    }

    /// Spends one step; `false` once none is left or time is up.
    fn spend(&self) -> bool {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.timed_out.set(true);
            self.remaining.set(0);
        }
        match self.remaining.get().checked_sub(1) {
            Some(left) => {
                self.remaining.set(left);
                true
            }
            None => {
                self.exhausted.set(true);
                false
            }
        }
    }

    fn exhausted(&self) -> bool {
        self.exhausted.get()
    }
}

/// The regexes of one call's `pattern` and `patternProperties` keywords,
/// each compiled once however often the schema is evaluated.
#[derive(Default)]
struct Patterns {
    /// `None` for a pattern that does not compile.
    compiled: RefCell<HashMap<String, Option<regex::Regex>>>,
}

impl Patterns {
    /// Whether `text` matches `pattern`; `None` when the pattern does not
    /// compile, so it is not checked.
    fn is_match(&self, pattern: &str, text: &str) -> Option<bool> {
        let mut compiled = self.compiled.try_borrow_mut().ok()?;
        if !compiled.contains_key(pattern) {
            let regex = regex::RegexBuilder::new(pattern)
                .size_limit(PATTERN_SIZE_LIMIT)
                .build()
                .ok();
            compiled.insert(pattern.to_owned(), regex);
        }
        compiled
            .get(pattern)?
            .as_ref()
            .map(|regex| regex.is_match(text))
    }
}

// ------------------------------------------------------------------ numbers

/// JavaScript's `Number.prototype.toString()` for a finite or infinite
/// number: shortest round-trip digits, plain decimals for exponents from -7
/// to 20, and `-0` printed as `0`.
fn js_number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    // Rust's `{:e}` prints the shortest round-trip digits, `d[.ddd]e<exp>`.
    let scientific = format!("{:e}", value.abs());
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return format!("{sign}{scientific}");
    };
    let Ok(exponent) = exponent.parse::<i64>() else {
        return format!("{sign}{scientific}");
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let k = digits.len() as i64;
    let n = exponent + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        let (whole, fraction) = digits.split_at(n as usize);
        format!("{whole}.{fraction}")
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let exponent_sign = if e >= 0 { "+" } else { "-" };
        let (first, rest) = digits.split_at(1);
        if rest.is_empty() {
            format!("{first}e{exponent_sign}{}", e.abs())
        } else {
            format!("{first}.{rest}e{exponent_sign}{}", e.abs())
        }
    };
    format!("{sign}{body}")
}

/// JavaScript's `String(number)` for a JSON number, which JavaScript holds
/// as a double.
fn js_number_string(number: &Number) -> String {
    number
        .as_f64()
        .map_or_else(|| number.to_string(), js_number_to_string)
}

/// A JavaScript number as JSON: integral values in the `i64` range become
/// integers, so they compare equal to the integers serde parses and read
/// back with `as_i64`.
fn js_number_value(value: f64) -> Value {
    // 2^63; every integral double below it in magnitude fits an `i64`.
    const I64_BOUND: f64 = 9_223_372_036_854_775_808.0;
    if value.fract() == 0.0 && (-I64_BOUND..I64_BOUND).contains(&value) {
        Value::from(value as i64)
    } else {
        Number::from_f64(value).map_or(Value::Null, Value::Number)
    }
}

/// JavaScript's `Number(text)` for a non-blank string; `None` for `NaN`.
fn js_number(text: &str) -> Option<f64> {
    let text = text.trim();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits == "Infinity" {
        return Some(sign * f64::INFINITY);
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            // JavaScript accepts no sign before a radix prefix.
            return u64::from_str_radix(rest, radix)
                .ok()
                .map(|value| value as f64);
        }
    }
    if text.is_empty()
        || !text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '+' | '-' | 'e' | 'E'))
    {
        return None;
    }
    text.parse::<f64>().ok()
}

/// A numeric limit as TypeBox prints it in a message.
fn limit_text(limit: &Value) -> String {
    match limit {
        Value::Number(number) => js_number_string(number),
        other => other.to_string(),
    }
}

// ------------------------------------------------------------------ coercion

fn schema_types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn is_integer(number: &Number) -> bool {
    number.is_i64()
        || number.is_u64()
        || number
            .as_f64()
            .is_some_and(|value| value.is_finite() && value.fract() == 0.0)
}

fn matches_json_type(value: &Value, kind: &str) -> bool {
    match kind {
        "number" => value.is_number(),
        "integer" => value.as_number().is_some_and(is_integer),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn coerce_primitive_by_type(value: &Value, kind: &str) -> Value {
    match kind {
        "number" | "integer" => match value {
            Value::Null => Value::from(0),
            Value::String(text) if !text.trim().is_empty() => match js_number(text) {
                Some(parsed)
                    if parsed.is_finite() && (kind == "number" || parsed.fract() == 0.0) =>
                {
                    js_number_value(parsed)
                }
                _ => value.clone(),
            },
            Value::Bool(flag) => Value::from(i64::from(*flag)),
            _ => value.clone(),
        },
        "boolean" => match value {
            Value::Null => Value::Bool(false),
            Value::String(text) if text == "true" => Value::Bool(true),
            Value::String(text) if text == "false" => Value::Bool(false),
            Value::Number(number) => match number.as_f64() {
                Some(1.0) => Value::Bool(true),
                Some(0.0) => Value::Bool(false),
                _ => value.clone(),
            },
            _ => value.clone(),
        },
        "string" => match value {
            Value::Null => Value::String(String::new()),
            Value::Number(number) => Value::String(js_number_string(number)),
            Value::Bool(flag) => Value::String(flag.to_string()),
            _ => value.clone(),
        },
        "null" => match value {
            Value::String(text) if text.is_empty() => Value::Null,
            Value::Number(number) if number.as_f64() == Some(0.0) => Value::Null,
            Value::Bool(false) => Value::Null,
            _ => value.clone(),
        },
        _ => value.clone(),
    }
}

/// Schema evaluation against one root, within one call's budget.
struct Validator<'r, 'b> {
    root: &'r Value,
    budget: &'b Budget,
    patterns: Patterns,
}

impl<'r, 'b> Validator<'r, 'b> {
    fn new(root: &'r Value, budget: &'b Budget) -> Self {
        Self {
            root,
            budget,
            patterns: Patterns::default(),
        }
    }
}

impl Validator<'_, '_> {
    fn apply_schema_object_coercion(&self, object: &mut Map<String, Value>, schema: &Value) {
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(properties) = properties {
            for (key, property_schema) in properties {
                if let Some(slot) = object.get_mut(key) {
                    *slot = self.coerce_with_json_schema(slot.take(), property_schema);
                }
            }
        }
        if let Some(additional) = schema
            .get("additionalProperties")
            .filter(|value| value.is_object())
        {
            for (key, slot) in object.iter_mut() {
                if properties.is_some_and(|properties| properties.contains_key(key)) {
                    continue;
                }
                *slot = self.coerce_with_json_schema(slot.take(), additional);
            }
        }
    }

    fn apply_schema_array_coercion(&self, items: &mut [Value], schema: &Value) {
        match schema.get("items") {
            Some(Value::Array(item_schemas)) => {
                for (item, item_schema) in items.iter_mut().zip(item_schemas) {
                    *item = self.coerce_with_json_schema(item.take(), item_schema);
                }
            }
            Some(item_schema @ Value::Object(_)) => {
                for item in items.iter_mut() {
                    *item = self.coerce_with_json_schema(item.take(), item_schema);
                }
            }
            _ => {}
        }
    }

    fn coerce_with_union_schema(&self, value: Value, schemas: &[Value]) -> Value {
        if schemas.iter().any(|schema| self.check(schema, &value, 0)) {
            return value;
        }
        for schema in schemas {
            let coerced = self.coerce_with_json_schema(value.clone(), schema);
            if self.check(schema, &coerced, 0) {
                return coerced;
            }
        }
        value
    }

    /// Pi's `coerceWithJsonSchema`.
    fn coerce_with_json_schema(&self, value: Value, schema: &Value) -> Value {
        let mut next = value;
        if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
            for nested in all_of {
                next = self.coerce_with_json_schema(next, nested);
            }
        }
        if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
            next = self.coerce_with_union_schema(next, any_of);
        }
        if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
            next = self.coerce_with_union_schema(next, one_of);
        }

        let kinds = schema_types(schema);
        let matches_union_member =
            kinds.len() > 1 && kinds.iter().any(|kind| matches_json_type(&next, kind));
        if !kinds.is_empty() && !matches_union_member {
            for kind in &kinds {
                let candidate = coerce_primitive_by_type(&next, kind);
                if candidate != next {
                    next = candidate;
                    break;
                }
            }
        }

        if kinds.contains(&"object")
            && let Value::Object(object) = &mut next
        {
            self.apply_schema_object_coercion(object, schema);
        }
        if kinds.contains(&"array")
            && let Value::Array(items) = &mut next
        {
            self.apply_schema_array_coercion(items, schema);
        }
        next
    }

    /// Pi's `normalizeOptionalNulls`: a `null` member that is optional and
    /// whose schema rejects `null` is treated as omitted.
    fn normalize_optional_nulls(&self, value: &mut Value, schema: &Value) {
        match value {
            Value::Array(items) => match schema.get("items") {
                Some(Value::Array(item_schemas)) => {
                    for (item, item_schema) in items.iter_mut().zip(item_schemas) {
                        self.normalize_optional_nulls(item, item_schema);
                    }
                }
                Some(item_schema) => {
                    for item in items.iter_mut() {
                        self.normalize_optional_nulls(item, item_schema);
                    }
                }
                None => {}
            },
            Value::Object(object) => {
                let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
                    return;
                };
                let required: BTreeSet<&str> = schema
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|keys| keys.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                for (key, property_schema) in properties {
                    let drop = match object.get(key) {
                        None => continue,
                        Some(member) => {
                            member.is_null()
                                && !required.contains(key.as_str())
                                && !property_schema.get("$ref").is_some_and(Value::is_string)
                                && !self.check(property_schema, &Value::Null, 0)
                        }
                    };
                    if drop {
                        object.remove(key);
                    } else if let Some(member) = object.get_mut(key) {
                        self.normalize_optional_nulls(member, property_schema);
                    }
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- validation

/// One failed keyword, as TypeBox reports it.
#[derive(Debug, Clone)]
struct SchemaError {
    /// Dotted instance path; empty at the root.
    path: String,
    message: String,
    /// The first missing property of a `required` error.
    missing: Option<String>,
}

impl SchemaError {
    /// Pi's `formatValidationPath`.
    fn path(&self) -> String {
        match (&self.missing, self.path.is_empty()) {
            (Some(missing), true) => missing.clone(),
            (Some(missing), false) => format!("{}.{missing}", self.path),
            (None, true) => "root".to_owned(),
            (None, false) => self.path.clone(),
        }
    }
}

/// TypeBox's `ErrorContext`: the errors of one evaluation, at most
/// [`MAX_ERRORS`].
#[derive(Debug, Default)]
struct Errors {
    list: Vec<SchemaError>,
}

impl Errors {
    fn at_capacity(&self) -> bool {
        self.list.len() >= MAX_ERRORS
    }

    /// Records an error if there is room; always `false`, the failed result.
    fn add(&mut self, path: &str, message: String) -> bool {
        self.add_error(SchemaError {
            path: path.to_owned(),
            message,
            missing: None,
        })
    }

    fn add_error(&mut self, error: SchemaError) -> bool {
        if !self.at_capacity() {
            self.list.push(error);
        }
        false
    }

    /// Copies a failed nested context's errors into this one.
    fn absorb(&mut self, nested: Errors) {
        for error in nested.list {
            self.add_error(error);
        }
    }
}

/// A numeric bound keyword, its comparison, and whether a value meets it.
type Bound = (&'static str, &'static str, fn(f64, f64) -> bool);

/// TypeBox's numeric keywords in its error order.
const BOUNDS: [Bound; 4] = [
    ("exclusiveMaximum", "<", |v, l| v < l),
    ("exclusiveMinimum", ">", |v, l| v > l),
    ("maximum", "<=", |v, l| v <= l),
    ("minimum", ">=", |v, l| v >= l),
];

fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

/// JSON equality with numbers compared by value, as JavaScript does.
fn json_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| json_equal(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, x)| b.get(key).is_some_and(|y| json_equal(x, y)))
        }
        _ => left == right,
    }
}

fn resolve_pointer<'r>(root: &'r Value, reference: &str) -> Option<&'r Value> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        return Some(root);
    }
    root.pointer(pointer)
}

/// Whether two items are equal as [`json_equal`] compares them, found by
/// hashing each item's canonical form and spending one step per item.
/// `false` once the budget runs out; the call then fails as too complex.
fn has_duplicate(items: &[Value], budget: &Budget) -> bool {
    let mut seen = HashSet::with_capacity(items.len());
    for item in items {
        if !budget.spend() {
            return false;
        }
        let mut key = String::new();
        canonical_json(item, &mut key);
        if !seen.insert(key) {
            return true;
        }
    }
    false
}

/// A text form under which two values are equal exactly when
/// [`json_equal`] says so: numbers printed from their `f64` value, so `1`,
/// `1.0`, and `-0` versus `0` agree, and object keys sorted.
fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Number(number) => match number.as_f64() {
            Some(float) => out.push_str(&js_number_to_string(float)),
            None => out.push_str(&number.to_string()),
        },
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        Value::Object(members) => {
            let mut keys: Vec<&String> = members.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                if let Some(member) = members.get(key) {
                    canonical_json(member, out);
                }
            }
            out.push('}');
        }
        other => out.push_str(&other.to_string()),
    }
}

impl Validator<'_, '_> {
    /// Whether `text` fails `pattern`. A pattern that does not compile is
    /// not checked.
    fn fails_pattern(&self, pattern: &str, text: &str) -> bool {
        self.patterns.is_match(pattern, text) == Some(false)
    }

    /// Whether `key` matches `pattern`. A pattern that does not compile
    /// matches nothing.
    fn matches_pattern(&self, pattern: &str, key: &str) -> bool {
        self.patterns.is_match(pattern, key) == Some(true)
    }

    /// Whether `key` is named by `properties` or matched by
    /// `patternProperties`, so `additionalProperties` does not apply to it.
    fn is_declared_key(&self, schema: &Map<String, Value>, key: &str) -> bool {
        schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| properties.contains_key(key))
            || schema
                .get("patternProperties")
                .and_then(Value::as_object)
                .is_some_and(|patterns| {
                    patterns
                        .keys()
                        .any(|pattern| self.matches_pattern(pattern, key))
                })
    }
}

/// The schema's `prefixItems`, or a tuple `items`, and the schema for the
/// items after it.
fn array_item_schemas(schema: &Map<String, Value>) -> (&[Value], Option<&Value>) {
    match (schema.get("prefixItems"), schema.get("items")) {
        (_, Some(Value::Array(tuple))) => (tuple, schema.get("additionalItems")),
        (Some(Value::Array(prefix)), rest) => (prefix, rest),
        (_, rest) => (&[], rest),
    }
}

impl Validator<'_, '_> {
    /// Whether `value` matches `schema`. Stops at the first failure.
    fn check(&self, schema: &Value, value: &Value, depth: usize) -> bool {
        if depth > MAX_DEPTH || !self.budget.spend() {
            return false;
        }
        let object = match schema {
            Value::Bool(accept) => return *accept,
            Value::Object(object) => object,
            _ => return true,
        };
        let deeper = depth + 1;

        let kinds = schema_types(schema);
        if !kinds.is_empty() && !kinds.iter().any(|kind| matches_json_type(value, kind)) {
            return false;
        }
        let keywords_hold = match value {
            Value::Object(members) => self.check_object(object, members, deeper),
            Value::Array(items) => self.check_array(object, items, deeper),
            Value::String(text) => self.string_errors(object, text, "").is_empty(),
            Value::Number(number) => number_errors(object, number, "").is_empty(),
            _ => true,
        };
        if !keywords_hold {
            return false;
        }
        if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
            match resolve_pointer(self.root, reference) {
                Some(target) if self.check(target, value, deeper) => {}
                _ => return false,
            }
        }
        if object
            .get("const")
            .is_some_and(|constant| !json_equal(constant, value))
        {
            return false;
        }
        if let Some(Value::Array(allowed)) = object.get("enum")
            && !allowed.iter().any(|option| json_equal(option, value))
        {
            return false;
        }
        if let Some(condition) = object.get("if") {
            let branch = if self.check(condition, value, deeper) {
                object.get("then")
            } else {
                object.get("else")
            };
            if branch.is_some_and(|branch| !self.check(branch, value, deeper)) {
                return false;
            }
        }
        if object
            .get("not")
            .is_some_and(|not| self.check(not, value, deeper))
        {
            return false;
        }
        if let Some(Value::Array(all_of)) = object.get("allOf")
            && !all_of
                .iter()
                .all(|nested| self.check(nested, value, deeper))
        {
            return false;
        }
        if let Some(Value::Array(any_of)) = object.get("anyOf")
            && !any_of
                .iter()
                .any(|nested| self.check(nested, value, deeper))
        {
            return false;
        }
        if let Some(Value::Array(one_of)) = object.get("oneOf") {
            let mut passing = 0;
            for nested in one_of {
                if self.check(nested, value, deeper) {
                    passing += 1;
                    if passing > 1 {
                        return false;
                    }
                }
            }
            if passing != 1 {
                return false;
            }
        }
        true
    }

    fn check_object(
        &self,
        schema: &Map<String, Value>,
        members: &Map<String, Value>,
        depth: usize,
    ) -> bool {
        if let Some(Value::Array(required)) = schema.get("required")
            && required
                .iter()
                .filter_map(Value::as_str)
                .any(|key| !members.contains_key(key))
        {
            return false;
        }
        if let Some(additional) = schema.get("additionalProperties")
            && !members.iter().all(|(key, member)| {
                self.is_declared_key(schema, key) || self.check(additional, member, depth)
            })
        {
            return false;
        }
        if let Some(patterns) = schema.get("patternProperties").and_then(Value::as_object) {
            for (pattern, nested) in patterns {
                if !members
                    .iter()
                    .filter(|(key, _)| self.matches_pattern(pattern, key))
                    .all(|(_, member)| self.check(nested, member, depth))
                {
                    return false;
                }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object)
            && !properties.iter().all(|(key, nested)| {
                members
                    .get(key)
                    .is_none_or(|member| self.check(nested, member, depth))
            })
        {
            return false;
        }
        if let Some(names) = schema.get("propertyNames")
            && !members
                .keys()
                .all(|key| self.check(names, &Value::String(key.clone()), depth))
        {
            return false;
        }
        property_count_errors(schema, members.len(), "").is_empty()
    }

    fn check_array(&self, schema: &Map<String, Value>, items: &[Value], depth: usize) -> bool {
        if let Some(contains) = schema.get("contains")
            && !items.iter().any(|item| self.check(contains, item, depth))
        {
            return false;
        }
        let (prefix, rest) = array_item_schemas(schema);
        for (index, item) in items.iter().enumerate() {
            let item_schema = prefix.get(index).or(rest);
            if item_schema.is_some_and(|item_schema| !self.check(item_schema, item, depth)) {
                return false;
            }
        }
        self.item_count_errors(schema, items, "").is_empty()
    }

    /// TypeBox's `ErrorSchema`: records why `value` fails `schema`, keyword
    /// by keyword in TypeBox's order, and returns whether it matched.
    fn errors(
        &self,
        schema: &Value,
        value: &Value,
        path: &str,
        depth: usize,
        errors: &mut Errors,
    ) -> bool {
        if depth > MAX_DEPTH || !self.budget.spend() {
            return errors.add(path, "schema nesting is too deep".to_owned());
        }
        let object = match schema {
            Value::Bool(true) => return true,
            Value::Bool(false) => return errors.add(path, "schema is false".to_owned()),
            Value::Object(object) => object,
            _ => return true,
        };
        if errors.at_capacity() {
            return false;
        }
        let deeper = depth + 1;
        let mut valid = true;

        let kinds = schema_types(schema);
        if !kinds.is_empty() && !kinds.iter().any(|kind| matches_json_type(value, kind)) {
            let message = if let [kind] = kinds.as_slice() {
                format!("must be {kind}")
            } else {
                format!("must be either {}", kinds.join(" or "))
            };
            valid = errors.add(path, message);
        }

        match value {
            Value::Object(members) => {
                valid &= self.object_errors(object, members, path, deeper, errors);
            }
            Value::Array(items) => valid &= self.array_errors(object, items, path, deeper, errors),
            Value::String(text) => {
                for error in self.string_errors(object, text, path) {
                    valid = errors.add_error(error);
                }
            }
            Value::Number(number) => {
                for error in number_errors(object, number, path) {
                    valid = errors.add_error(error);
                }
            }
            _ => {}
        }

        if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
            match resolve_pointer(self.root, reference) {
                Some(target) => {
                    let mut nested = Errors::default();
                    if !self.errors(target, value, path, deeper, &mut nested) {
                        errors.absorb(nested);
                        valid = false;
                    }
                }
                None => {
                    valid = errors.add(path, format!("cannot resolve reference \"{reference}\""));
                }
            }
        }
        if object
            .get("const")
            .is_some_and(|constant| !json_equal(constant, value))
        {
            valid = errors.add(path, "must be equal to constant".to_owned());
        }
        if let Some(Value::Array(allowed)) = object.get("enum")
            && !allowed.iter().any(|option| json_equal(option, value))
        {
            valid = errors.add(
                path,
                "must be equal to one of the allowed values".to_owned(),
            );
        }
        if let Some(condition) = object.get("if") {
            // TypeBox drops the errors of `if` and `then` but keeps those of
            // `else`.
            let mut discarded = Errors::default();
            if self.errors(condition, value, path, deeper, &mut discarded) {
                if let Some(then) = object.get("then")
                    && !self.errors(then, value, path, deeper, &mut discarded)
                {
                    valid = errors.add(path, "must match \"then\" schema".to_owned());
                }
            } else if let Some(otherwise) = object.get("else")
                && !self.errors(otherwise, value, path, deeper, errors)
            {
                valid = errors.add(path, "must match \"else\" schema".to_owned());
            }
        }
        if let Some(not) = object.get("not")
            && self.check(not, value, deeper)
        {
            valid = errors.add(path, "must not be valid".to_owned());
        }
        if let Some(Value::Array(all_of)) = object.get("allOf") {
            for nested_schema in all_of {
                let mut nested = Errors::default();
                if !self.errors(nested_schema, value, path, deeper, &mut nested) {
                    errors.absorb(nested);
                    valid = false;
                }
            }
        }
        if let Some(Value::Array(any_of)) = object.get("anyOf") {
            let (passing, failed) = self.branch_errors(any_of, value, path, deeper);
            if passing == 0 {
                for nested in failed {
                    errors.absorb(nested);
                }
                valid = errors.add(path, "must match a schema in anyOf".to_owned());
            }
        }
        if let Some(Value::Array(one_of)) = object.get("oneOf") {
            let (passing, failed) = self.branch_errors(one_of, value, path, deeper);
            if passing != 1 {
                if passing == 0 {
                    for nested in failed {
                        errors.absorb(nested);
                    }
                }
                valid = errors.add(path, "must match exactly one schema in oneOf".to_owned());
            }
        }
        valid
    }

    /// Evaluates each branch in its own context: how many matched, and the
    /// errors of those that did not.
    fn branch_errors(
        &self,
        branches: &[Value],
        value: &Value,
        path: &str,
        depth: usize,
    ) -> (usize, Vec<Errors>) {
        let mut passing = 0;
        let mut failed = Vec::new();
        for branch in branches {
            let mut nested = Errors::default();
            if self.errors(branch, value, path, depth, &mut nested) {
                passing += 1;
            } else {
                failed.push(nested);
            }
        }
        (passing, failed)
    }

    fn object_errors(
        &self,
        schema: &Map<String, Value>,
        members: &Map<String, Value>,
        path: &str,
        depth: usize,
        errors: &mut Errors,
    ) -> bool {
        let mut valid = true;
        if let Some(Value::Array(required)) = schema.get("required") {
            let missing: Vec<&str> = required
                .iter()
                .filter_map(Value::as_str)
                .filter(|key| !members.contains_key(*key))
                .collect();
            if let Some(first) = missing.first() {
                valid = errors.add_error(SchemaError {
                    path: path.to_owned(),
                    message: format!("must have required properties {}", missing.join(", ")),
                    missing: Some((*first).to_owned()),
                });
            }
        }
        if let Some(additional) = schema.get("additionalProperties") {
            let mut all_allowed = true;
            for (key, member) in members {
                if !self.is_declared_key(schema, key)
                    && !self.errors(additional, member, &child_path(path, key), depth, errors)
                {
                    all_allowed = false;
                }
            }
            if !all_allowed {
                valid = errors.add(path, "must not have additional properties".to_owned());
            }
        }
        if let Some(patterns) = schema.get("patternProperties").and_then(Value::as_object) {
            for (pattern, nested) in patterns {
                for (key, member) in members {
                    if self.matches_pattern(pattern, key) {
                        valid &= self.errors(nested, member, &child_path(path, key), depth, errors);
                    }
                }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (key, nested) in properties {
                if let Some(member) = members.get(key) {
                    valid &= self.errors(nested, member, &child_path(path, key), depth, errors);
                }
            }
        }
        if let Some(names) = schema.get("propertyNames") {
            let mut invalid = Vec::new();
            for key in members.keys() {
                let name = Value::String(key.clone());
                if !self.errors(names, &name, &child_path(path, key), depth, errors) {
                    invalid.push(key.as_str());
                }
            }
            if !invalid.is_empty() {
                valid = errors.add(
                    path,
                    format!("property names {} are invalid", invalid.join(", ")),
                );
            }
        }
        for error in property_count_errors(schema, members.len(), path) {
            valid = errors.add_error(error);
        }
        valid
    }

    fn array_errors(
        &self,
        schema: &Map<String, Value>,
        items: &[Value],
        path: &str,
        depth: usize,
        errors: &mut Errors,
    ) -> bool {
        let mut valid = true;
        let (prefix, rest) = array_item_schemas(schema);
        let item_path = |index: usize| child_path(path, &index.to_string());
        // TypeBox reports `additionalItems` and `contains` before `items`.
        if let (Some(Value::Array(_)), Some(additional)) =
            (schema.get("items"), schema.get("additionalItems"))
        {
            for (index, item) in items.iter().enumerate().skip(prefix.len()) {
                valid &= self.errors(additional, item, &item_path(index), depth, errors);
            }
        }
        if let Some(contains) = schema.get("contains")
            && !items.iter().any(|item| self.check(contains, item, depth))
        {
            valid = errors.add(path, "must contain at least 1 valid item".to_owned());
        }
        let tuple_items = matches!(schema.get("items"), Some(Value::Array(_)));
        if tuple_items {
            for (index, (item, item_schema)) in items.iter().zip(prefix).enumerate() {
                valid &= self.errors(item_schema, item, &item_path(index), depth, errors);
            }
        } else if let Some(rest) = rest {
            for (index, item) in items.iter().enumerate().skip(prefix.len()) {
                valid &= self.errors(rest, item, &item_path(index), depth, errors);
            }
        }
        let mut counts = self.item_count_errors(schema, items, path);
        // `uniqueItems` comes after `prefixItems`.
        let unique = counts
            .iter()
            .position(|error| error.message == "must not have duplicate items")
            .map(|index| counts.remove(index));
        for error in counts {
            valid = errors.add_error(error);
        }
        if !tuple_items && let Some(Value::Array(prefix)) = schema.get("prefixItems") {
            for (index, (item, item_schema)) in items.iter().zip(prefix).enumerate() {
                valid &= self.errors(item_schema, item, &item_path(index), depth, errors);
            }
        }
        if let Some(unique) = unique {
            valid = errors.add_error(unique);
        }
        valid
    }
}

fn keyword_error(path: &str, message: String) -> SchemaError {
    SchemaError {
        path: path.to_owned(),
        message,
        missing: None,
    }
}

/// `minProperties` and `maxProperties` failures.
fn property_count_errors(
    schema: &Map<String, Value>,
    count: usize,
    path: &str,
) -> Vec<SchemaError> {
    let count = count as u64;
    let mut found = Vec::new();
    if let Some(limit) = schema.get("minProperties").and_then(Value::as_u64)
        && count < limit
    {
        found.push(keyword_error(
            path,
            format!("must not have fewer than {limit} properties"),
        ));
    }
    if let Some(limit) = schema.get("maxProperties").and_then(Value::as_u64)
        && count > limit
    {
        found.push(keyword_error(
            path,
            format!("must not have more than {limit} properties"),
        ));
    }
    found
}

impl Validator<'_, '_> {
    /// `maxItems`, `minItems`, and `uniqueItems` failures, in TypeBox's order.
    fn item_count_errors(
        &self,
        schema: &Map<String, Value>,
        items: &[Value],
        path: &str,
    ) -> Vec<SchemaError> {
        let count = items.len() as u64;
        let mut found = Vec::new();
        if let Some(limit) = schema.get("maxItems").and_then(Value::as_u64)
            && count > limit
        {
            found.push(keyword_error(
                path,
                format!("must not have more than {limit} items"),
            ));
        }
        if let Some(limit) = schema.get("minItems").and_then(Value::as_u64)
            && count < limit
        {
            found.push(keyword_error(
                path,
                format!("must not have fewer than {limit} items"),
            ));
        }
        if schema.get("uniqueItems") == Some(&Value::Bool(true))
            && has_duplicate(items, self.budget)
        {
            found.push(keyword_error(
                path,
                "must not have duplicate items".to_owned(),
            ));
        }
        found
    }

    /// `maxLength`, `minLength`, and `pattern` failures, in TypeBox's order.
    fn string_errors(
        &self,
        schema: &Map<String, Value>,
        text: &str,
        path: &str,
    ) -> Vec<SchemaError> {
        let mut found = Vec::new();
        let length = text.encode_utf16().count() as u64;
        if let Some(limit) = schema.get("maxLength").and_then(Value::as_u64)
            && length > limit
        {
            found.push(keyword_error(
                path,
                format!("must not have more than {limit} characters"),
            ));
        }
        if let Some(limit) = schema.get("minLength").and_then(Value::as_u64)
            && length < limit
        {
            found.push(keyword_error(
                path,
                format!("must not have fewer than {limit} characters"),
            ));
        }
        if let Some(pattern) = schema.get("pattern").and_then(Value::as_str)
            && self.fails_pattern(pattern, text)
        {
            found.push(keyword_error(
                path,
                format!("must match pattern \"{pattern}\""),
            ));
        }
        found
    }
}

/// Numeric bound and `multipleOf` failures, in TypeBox's order.
fn number_errors(schema: &Map<String, Value>, number: &Number, path: &str) -> Vec<SchemaError> {
    let mut found = Vec::new();
    let Some(value) = number.as_f64() else {
        return found;
    };
    for (keyword, comparison, holds) in BOUNDS {
        if let Some(limit) = schema.get(keyword)
            && let Some(bound) = limit.as_f64()
            && !holds(value, bound)
        {
            found.push(keyword_error(
                path,
                format!("must be {comparison} {}", limit_text(limit)),
            ));
        }
    }
    if let Some(divisor) = schema.get("multipleOf")
        && let Some(step) = divisor.as_f64()
        && step > 0.0
    {
        let quotient = value / step;
        if !quotient.is_finite() || (quotient - quotient.round()).abs() > 1e-9 {
            found.push(keyword_error(
                path,
                format!("must be multiple of {}", limit_text(divisor)),
            ));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn check(schema: &Value, value: &Value, root: &Value) -> bool {
        let budget = Budget::new(STEP_BUDGET, TIME_LIMIT);
        Validator::new(root, &budget).check(schema, value, 0)
    }

    fn plain(schema: Value, input: Value) -> Result<Value, String> {
        let parameters = json!({
            "type": "object",
            "properties": { "value": schema },
            "required": ["value"],
        });
        validate_tool_arguments(&parameters, "echo", &json!({ "value": input }))
    }

    /// Pi `validation.test.ts`: "still validates when Function constructor
    /// is unavailable" (the TypeBox object schema case without the CSP part).
    #[test]
    fn coerces_a_numeric_string_for_a_number_member() {
        let parameters = json!({
            "type": "object",
            "properties": { "count": { "type": "number" } },
            "required": ["count"],
        });
        assert_eq!(
            validate_tool_arguments(&parameters, "echo", &json!({ "count": "42" })),
            Ok(json!({ "count": 42 }))
        );
    }

    /// Pi `validation.test.ts`: "coerces serialized plain JSON schemas with
    /// AJV-compatible primitive rules".
    #[test]
    fn coerces_plain_schemas_with_ajv_primitive_rules() {
        let cases = [
            (json!({ "type": "number" }), json!("42"), json!(42)),
            (json!({ "type": "number" }), json!(true), json!(1)),
            (json!({ "type": "number" }), json!(null), json!(0)),
            (json!({ "type": "integer" }), json!("42"), json!(42)),
            (json!({ "type": "boolean" }), json!("true"), json!(true)),
            (json!({ "type": "boolean" }), json!("false"), json!(false)),
            (json!({ "type": "boolean" }), json!(1), json!(true)),
            (json!({ "type": "boolean" }), json!(0), json!(false)),
            (json!({ "type": "string" }), json!(null), json!("")),
            (json!({ "type": "string" }), json!(true), json!("true")),
            (json!({ "type": "null" }), json!(""), json!(null)),
            (json!({ "type": "null" }), json!(0), json!(null)),
            (json!({ "type": "null" }), json!(false), json!(null)),
            (
                json!({ "type": ["number", "string"] }),
                json!("1"),
                json!("1"),
            ),
            (
                json!({ "type": ["boolean", "number"] }),
                json!("1"),
                json!(1),
            ),
        ];
        for (schema, input, expected) in cases {
            assert_eq!(
                plain(schema.clone(), input.clone()),
                Ok(json!({ "value": expected })),
                "{schema} {input}"
            );
        }
    }

    /// Pi `validation.test.ts`: "treats null as omission for optional
    /// non-nullable properties".
    #[test]
    fn treats_null_as_omission_for_optional_non_nullable_members() {
        let parameters = json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "number" },
                "nullable": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
                "metadata": {
                    "type": "object",
                    "properties": { "enabled": { "type": "boolean" } },
                },
            },
            "required": ["path", "metadata"],
        });
        let args = json!({
            "path": "file.txt",
            "offset": null,
            "nullable": null,
            "metadata": { "enabled": null },
        });
        assert_eq!(
            validate_tool_arguments(&parameters, "echo", &args),
            Ok(json!({ "path": "file.txt", "nullable": null, "metadata": {} }))
        );
    }

    /// Pi `validation.test.ts`: "preserves optional nulls whose referenced
    /// schema is nullable".
    #[test]
    fn preserves_optional_nulls_behind_a_nullable_reference() {
        let parameters = json!({
            "type": "object",
            "properties": { "value": { "$ref": "#/$defs/value" } },
            "$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } },
        });
        assert_eq!(
            validate_tool_arguments(&parameters, "echo", &json!({ "value": null })),
            Ok(json!({ "value": null }))
        );
    }

    /// Pi `validation.test.ts`: "preserves a value that already matches a
    /// nullable union arm", its `oneOf` twin, and "still coerces nullable
    /// unions when the original value does not match any arm".
    #[test]
    fn nullable_unions_keep_matching_values_and_coerce_others() {
        let any_of = json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] });
        let one_of = json!({ "oneOf": [{ "type": "number" }, { "type": "null" }] });
        assert_eq!(
            plain(any_of.clone(), json!(null)),
            Ok(json!({ "value": null }))
        );
        assert_eq!(plain(one_of, json!(null)), Ok(json!({ "value": null })));
        assert_eq!(plain(any_of, json!("42")), Ok(json!({ "value": 42 })));
    }

    /// Pi `validation.test.ts`: "accepts null for nullable array schemas
    /// with items".
    #[test]
    fn accepts_null_for_nullable_arrays() {
        let schema = json!({ "type": ["array", "null"], "items": { "type": "string" } });
        assert_eq!(plain(schema, json!(null)), Ok(json!({ "value": null })));
    }

    /// Pi `validation.test.ts`: "rejects invalid coercions for serialized
    /// plain JSON schemas".
    #[test]
    fn rejects_invalid_coercions() {
        let cases = [
            (json!({ "type": "boolean" }), json!("1")),
            (json!({ "type": "boolean" }), json!("0")),
            (json!({ "type": "null" }), json!("null")),
            (json!({ "type": "integer" }), json!("42.1")),
        ];
        for (schema, input) in cases {
            let error = plain(schema.clone(), input.clone()).unwrap_err();
            assert!(
                error.contains("Validation failed"),
                "{schema} {input}: {error}"
            );
        }
    }

    /// Error lines and coerced values recorded from Pi's
    /// `validateToolArguments` (TypeBox `Errors`) for every keyword the
    /// validator covers; see the fixture's `source`.
    #[test]
    fn matches_pi_error_lines_and_results() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/pi-validation-errors.json"))
                .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let actual = validate_tool_arguments(&case["parameters"], "echo", &case["arguments"]);
            let expected = match case.get("err") {
                Some(error) => Err(error.as_str().unwrap().to_owned()),
                None => Ok(case["ok"].clone()),
            };
            assert_eq!(actual, expected, "{}", case["parameters"]);
        }
    }

    #[test]
    fn checks_the_remaining_keywords() {
        let pass = [
            (json!({ "enum": ["a", "b"] }), json!("b")),
            (json!({ "const": 1 }), json!(1.0)),
            (
                json!({ "type": "integer", "minimum": 1, "maximum": 3 }),
                json!(3),
            ),
            (json!({ "type": "number", "multipleOf": 0.5 }), json!(1.5)),
            (
                json!({ "type": "string", "minLength": 2, "pattern": "^a" }),
                json!("ab"),
            ),
            (
                json!({ "type": "array", "items": { "type": "integer" }, "uniqueItems": true }),
                json!([1, 2]),
            ),
            (
                json!({ "type": "array", "prefixItems": [{ "type": "string" }], "minItems": 1 }),
                json!(["x", 2]),
            ),
            (
                json!({ "allOf": [{ "type": "number" }, { "minimum": 0 }] }),
                json!(2),
            ),
            (json!({ "not": { "type": "string" } }), json!(2)),
            (
                json!({ "if": { "type": "string" }, "then": { "minLength": 1 } }),
                json!("a"),
            ),
            (
                json!({ "type": "object", "patternProperties": { "^x": { "type": "integer" } }, "additionalProperties": false }),
                json!({ "x1": 1 }),
            ),
            (json!(true), json!({ "anything": [] })),
        ];
        for (schema, input) in pass {
            assert!(
                check(&schema, &input, &schema),
                "{schema} should accept {input}"
            );
        }
        let fail = [
            (json!({ "enum": ["a", "b"] }), json!("c")),
            (
                json!({ "type": "integer", "exclusiveMaximum": 3 }),
                json!(3),
            ),
            (json!({ "type": "string", "maxLength": 1 }), json!("😀")),
            (
                json!({ "type": "array", "uniqueItems": true }),
                json!([1, 1.0]),
            ),
            (json!({ "type": "array", "maxItems": 1 }), json!([1, 2])),
            (
                json!({ "oneOf": [{ "type": "number" }, { "type": "integer" }] }),
                json!(1),
            ),
            (
                json!({ "type": "object", "propertyNames": { "pattern": "^[a-z]+$" } }),
                json!({ "A": 1 }),
            ),
            (
                json!({ "type": "array", "contains": { "const": 2 } }),
                json!([1]),
            ),
            (json!(false), json!(null)),
            (json!({ "$ref": "#/missing" }), json!(1)),
        ];
        for (schema, input) in fail {
            assert!(
                !check(&schema, &input, &schema),
                "{schema} should reject {input}"
            );
        }
    }

    #[test]
    fn stops_at_a_reference_cycle_without_overflowing() {
        let schema = json!({ "$ref": "#" });
        let error = validate_tool_arguments(&schema, "loop", &json!({})).unwrap_err();
        assert!(error.contains("schema nesting is too deep"), "{error}");
    }

    /// A self-reference that branches at every level would take about
    /// 2^256 steps without the budget; Pi fails it with a stack overflow.
    #[test]
    fn stops_a_branching_reference_cycle_within_the_budget() {
        let started = std::time::Instant::now();
        for schema in [
            json!({ "anyOf": [{ "$ref": "#" }, { "$ref": "#" }] }),
            json!({
                "type": "object",
                "properties": { "a": { "$ref": "#/$defs/x" } },
                "$defs": { "x": { "anyOf": [{ "$ref": "#/$defs/x" }, { "$ref": "#/$defs/x" }] } },
            }),
            json!({ "oneOf": [{ "$ref": "#" }, { "not": { "$ref": "#" } }] }),
        ] {
            let error = validate_tool_arguments(&schema, "loop", &json!({ "a": 1 })).unwrap_err();
            assert_eq!(
                error,
                "Validation failed for tool \"loop\": the schema is too complex or recursive to check"
            );
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
    }

    /// Runs `work` on its own thread and fails the test if it takes longer
    /// than `limit`, so a regression fails instead of hanging the suite. The
    /// thread is left behind on failure and ends with the test process.
    fn within<T: Send + 'static>(
        limit: std::time::Duration,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work());
        });
        receiver
            .recv_timeout(limit)
            .unwrap_or_else(|_| panic!("validation took longer than {limit:?}"))
    }

    /// A pattern under a branching cycle is evaluated at every step. Compiled
    /// each time, `\w{20}\w{20}` takes about a millisecond a step, so 20,000
    /// steps take far longer than the bound; cached, the step budget ends the
    /// call, not the clock.
    #[test]
    fn compiles_each_pattern_once_under_a_branching_cycle() {
        let (exhausted, timed_out, error) = within(std::time::Duration::from_secs(5), || {
            let schema = json!({
                "type": "string",
                "pattern": "\\w{20}\\w{20}",
                "anyOf": [{ "$ref": "#" }, { "$ref": "#" }],
            });
            let budget = Budget::new(20_000, std::time::Duration::from_secs(3600));
            let result = validate_within(&schema, "loop", &json!("x".repeat(40)), &budget);
            (budget.exhausted(), budget.timed_out.get(), result.is_err())
        });
        assert!(exhausted && error, "the step budget should end the call");
        assert!(
            !timed_out,
            "the step budget, not the clock, should end the call"
        );
    }

    /// The public call with costly patterns, under the production limits:
    /// `\w{200}\w{200}` exceeds the compiled-size limit and is not checked;
    /// the other compiles once.
    #[test]
    fn costly_patterns_under_a_cycle_fail_quickly() {
        within(std::time::Duration::from_secs(20), || {
            for pattern in ["\\w{200}\\w{200}", "\\w{50}[a-z]{30}\\w{20}"] {
                let schema = json!({
                    "type": "string",
                    "pattern": pattern,
                    "anyOf": [{ "$ref": "#" }, { "$ref": "#" }],
                });
                let error = validate_tool_arguments(&schema, "loop", &json!("hello")).unwrap_err();
                assert_eq!(
                    error,
                    "Validation failed for tool \"loop\": the schema is too complex or recursive to check"
                );
            }
        });
    }

    /// Steps that cost more than the budget assumes still end at the clock.
    #[test]
    fn stops_at_the_time_limit_when_steps_are_unbounded() {
        let (timed_out, error) = within(std::time::Duration::from_secs(20), || {
            let schema = json!({ "anyOf": [{ "$ref": "#" }, { "$ref": "#" }] });
            let budget = Budget::new(usize::MAX, std::time::Duration::from_millis(50));
            let result = validate_within(&schema, "loop", &json!({}), &budget);
            (budget.timed_out.get(), result.is_err())
        });
        assert!(timed_out && error);
    }

    /// Lookaround is valid JavaScript but not Rust `regex` syntax: such a
    /// pattern is not checked, rather than failing every string.
    #[test]
    fn does_not_check_patterns_that_do_not_compile() {
        assert_eq!(
            plain(
                json!({ "type": "string", "pattern": "^(?!x)" }),
                json!("xyz")
            ),
            Ok(json!({ "value": "xyz" }))
        );
        let schema = json!({
            "type": "object",
            "patternProperties": { "^(?=a)": { "type": "integer" } },
            "additionalProperties": false,
        });
        assert!(validate_tool_arguments(&schema, "echo", &json!({ "ab": 1 })).is_err());
    }

    /// `uniqueItems` hashes items instead of comparing every pair, and
    /// compares numbers by value as JavaScript does.
    #[test]
    fn checks_unique_items_in_linear_time() {
        within(std::time::Duration::from_secs(5), || {
            let schema = json!({ "type": "array", "uniqueItems": true });
            let unique: Vec<Value> = (0..50_000).map(Value::from).collect();
            assert!(plain(schema.clone(), Value::Array(unique.clone())).is_ok());
            let mut repeated = unique;
            repeated.push(json!(49_999.0));
            let error = plain(schema, Value::Array(repeated)).unwrap_err();
            assert!(error.contains("must not have duplicate items"), "{error}");
        });
        let schema = json!({ "type": "array", "uniqueItems": true });
        for (items, duplicate) in [
            (json!([1, 1.0]), true),
            (json!([0, -0.0]), true),
            (json!([{ "a": 1, "b": [2] }, { "b": [2.0], "a": 1 }]), true),
            (json!(["1", 1]), false),
            (json!([[1, 2], [2, 1]]), false),
            (json!([null, false, 0, ""]), false),
        ] {
            assert_eq!(
                plain(schema.clone(), items.clone()).is_err(),
                duplicate,
                "{items}"
            );
        }
    }

    /// Pi's `String(number)` for coerced strings, recorded with `npx tsx`.
    #[test]
    fn converts_numbers_to_strings_as_javascript_does() {
        let cases = [
            (json!(1e20), "100000000000000000000"),
            (json!(0.000001), "0.000001"),
            (json!(-0.0), "0"),
            (json!(123456789012345680000.0), "123456789012345680000"),
            (json!(12345678901234567890_u64), "12345678901234567000"),
            (json!(1e21), "1e+21"),
            (json!(1.5e-7), "1.5e-7"),
            (json!(0.1), "0.1"),
            (json!(123.456), "123.456"),
            (json!(-42), "-42"),
            (json!(-2.5e-10), "-2.5e-10"),
            (json!(1.7976931348623157e308), "1.7976931348623157e+308"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                plain(json!({ "type": "string" }), input.clone()),
                Ok(json!({ "value": expected })),
                "{input}"
            );
        }
    }

    /// Pi coerces `"9007199254740993"` to the double 9007199254740992; here
    /// the integral result stays an integer so tools can read it with
    /// `as_i64`.
    #[test]
    fn integral_coercions_stay_integers() {
        for (input, expected) in [
            ("9007199254740993", 9_007_199_254_740_992_i64),
            ("-9007199254740992", -9_007_199_254_740_992),
            ("1e18", 1_000_000_000_000_000_000),
        ] {
            let result = plain(json!({ "type": "integer" }), json!(input)).unwrap();
            assert_eq!(result["value"].as_i64(), Some(expected), "{input}");
        }
        let large = plain(json!({ "type": "number" }), json!("1e20")).unwrap();
        assert_eq!(large["value"].as_f64(), Some(1e20));
    }

    #[test]
    fn parses_numbers_as_javascript_number_does() {
        assert_eq!(js_number(" 42 "), Some(42.0));
        assert_eq!(js_number("0x10"), Some(16.0));
        assert_eq!(js_number("1e3"), Some(1000.0));
        assert_eq!(js_number("-Infinity"), Some(f64::NEG_INFINITY));
        assert_eq!(js_number("inf"), None);
        assert_eq!(js_number("NaN"), None);
        assert_eq!(js_number("1e"), None);
        assert_eq!(js_number("12px"), None);
    }
}
