//! TypeScript declarations of the script-visible API, rendered from JSON
//! Schema; ported from Pi's `packages/codemode/src/declarations.ts` (v1.3.0;
//! MIT, see `NOTICE`).

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::identifier::{is_identifier, to_codemode_identifier};
use crate::js;

const INDENT: &str = "  ";
/// Largest rendered input type, in characters, before it becomes `unknown`.
pub const DEFAULT_INPUT_SCHEMA_MAX_CHARS: usize = 16_000;
/// Local `$ref` expansions per rendered schema, so shared definitions cannot
/// blow up the output.
const MAX_REF_EXPANSIONS: usize = 32;
/// JSON Schema keywords with a number or string value shown as
/// `@keyword value` property tags.
const TAG_KEYWORDS: [&str; 11] = [
    "minimum",
    "exclusiveMinimum",
    "maximum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minItems",
    "maxItems",
];
/// Tag keywords whose values all hold at once, so only the largest (lower) or
/// smallest (upper) matters.
const LOWER_BOUNDS: [&str; 4] = ["minimum", "exclusiveMinimum", "minLength", "minItems"];
const UPPER_BOUNDS: [&str; 4] = ["maximum", "exclusiveMaximum", "maxLength", "maxItems"];
/// Longest property tag; longer ones (large defaults, long patterns) are left out.
const MAX_TAG_CHARS: usize = 80;

/// A tool or global as its declaration shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptFunction {
    /// The tool's name, or the global's: an identifier or `namespace.member`.
    pub name: String,
    /// Shown as a doc comment.
    pub description: Option<String>,
    /// Schema of the single argument; `unknown` when absent.
    pub input_schema: Option<Value>,
    /// Schema of the resolved value; `unknown` when absent.
    pub output_schema: Option<Value>,
    /// Globals only: a TypeScript parameter list and return type, for example
    /// `(type: string): Promise<Model[]>`, which replaces the rendering from
    /// the schemas.
    pub signature: Option<String>,
}

/// Renders TypeScript declarations for the script-visible API. Tools become
/// members of `declare const tools`, globals `declare function` statements,
/// and `ns.member` globals members of `declare const ns`. Descriptions become
/// doc comments; schemas become types.
pub fn render_declarations(tools: &[ScriptFunction], globals: &[ScriptFunction]) -> String {
    let mut sections = Vec::new();
    if !tools.is_empty() {
        let members: Vec<String> = tools
            .iter()
            .map(|tool| {
                format!(
                    "{}{INDENT}{}",
                    doc_comment(tool.description.as_deref(), INDENT),
                    render_tool_signature(tool, None)
                )
            })
            .collect();
        sections.push(format!(
            "declare const tools: {{\n{}\n}};",
            members.join("\n")
        ));
    }
    let mut namespaces: Vec<(String, Vec<String>)> = Vec::new();
    for global in globals {
        let Some((namespace, member)) = global.name.split_once('.') else {
            sections.push(render_global(
                &format!("declare function {}", global.name),
                global,
                "",
            ));
            continue;
        };
        let rendered = render_global(member, global, INDENT);
        match namespaces.iter_mut().find(|(name, _)| name == namespace) {
            Some((_, members)) => members.push(rendered),
            None => namespaces.push((namespace.to_owned(), vec![rendered])),
        }
    }
    for (namespace, members) in namespaces {
        sections.push(format!(
            "declare const {namespace}: {{\n{}\n}};",
            members.join("\n")
        ));
    }
    sections.join("\n\n")
}

/// One tool as a member of the `tools` object: `name(args: T): Promise<R>;`
/// with the identifier scripts use. Input types longer than `input_max_chars`
/// (default [`DEFAULT_INPUT_SCHEMA_MAX_CHARS`]) render as `unknown`.
pub fn render_tool_signature(tool: &ScriptFunction, input_max_chars: Option<usize>) -> String {
    let input = match &tool.input_schema {
        None => "unknown".to_owned(),
        Some(schema) => schema_to_type(
            schema,
            Some(input_max_chars.unwrap_or(DEFAULT_INPUT_SCHEMA_MAX_CHARS)),
        ),
    };
    format!(
        "{}(args: {input}): Promise<{}>;",
        to_codemode_identifier(&tool.name),
        render_tool_output_type(tool.output_schema.as_ref())
    )
}

/// A tool's sample: the description followed by the tool's declaration, for
/// tool listings and `ALL_TOOLS` entries.
pub fn render_tool_sample(tool: &ScriptFunction, input_max_chars: Option<usize>) -> String {
    let declaration = format!(
        "declare const tools: {{ {} }};",
        render_tool_signature(tool, input_max_chars)
    );
    format!(
        "{}\n\ncodemode tool declaration:\n```ts\n{declaration}\n```",
        tool.description.as_deref().map_or("", str::trim)
    )
}

/// The type a tool call resolves to: the output schema's type, and `unknown`
/// without a schema.
pub fn render_tool_output_type(schema: Option<&Value>) -> String {
    schema.map_or_else(
        || "unknown".to_owned(),
        |schema| schema_to_type(schema, None),
    )
}

fn render_global(head: &str, global: &ScriptFunction, indent: &str) -> String {
    let doc = doc_comment(global.description.as_deref(), indent);
    if let Some(signature) = &global.signature {
        return format!("{doc}{indent}{head}{signature};");
    }
    let input = global.input_schema.as_ref().map_or_else(
        || "unknown".to_owned(),
        |schema| schema_to_type(schema, None),
    );
    let output = global.output_schema.as_ref().map_or_else(
        || "unknown".to_owned(),
        |schema| schema_to_type(schema, None),
    );
    format!("{doc}{indent}{head}(args: {input}): Promise<{output}>;")
}

fn doc_comment(description: Option<&str>, indent: &str) -> String {
    let text = description.map_or("", str::trim);
    if text.is_empty() {
        return String::new();
    }
    let escaped = text.replace("*/", "*\\/");
    let lines: Vec<&str> = escaped
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    if let [line] = lines.as_slice() {
        return format!("{indent}/** {line} */\n");
    }
    let body: Vec<String> = lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                format!("{indent} *")
            } else {
                format!("{indent} * {line}")
            }
        })
        .collect();
    format!("{indent}/**\n{}\n{indent} */\n", body.join("\n"))
}

fn property_key(name: &str) -> String {
    if is_identifier(name) {
        name.to_owned()
    } else {
        js::stringify(&Value::String(name.to_owned()))
    }
}

fn union(types: Vec<String>) -> String {
    let mut unique: Vec<String> = Vec::new();
    for entry in types {
        if !unique.contains(&entry) {
            unique.push(entry);
        }
    }
    if unique.iter().any(|entry| entry == "unknown") {
        return "unknown".to_owned();
    }
    if unique.is_empty() {
        "never".to_owned()
    } else {
        unique.join(" | ")
    }
}

/// Converts a JSON Schema to a TypeScript type expression: objects on one
/// line (`{ a: string; b?: number; }`) with properties sorted by name, or one
/// property per line with `//` comments when a property has a description or
/// constraints the type cannot express (`// @integer @minimum 1 @default 20`);
/// `Array<T>` for arrays. Local references (`#/$defs/...`,
/// `#/definitions/...`) resolve against `schema`; recursive and remote
/// references render as `unknown`. A result longer than `max_chars` (UTF-16
/// units) is rendered again without the constraint tags, and renders as
/// `unknown` if it is still too long.
pub fn schema_to_type(schema: &Value, max_chars: Option<usize>) -> String {
    let render = |tags: bool| {
        let mut context = SchemaContext {
            root: schema,
            resolving: HashSet::new(),
            expansions: 0,
            tags,
        };
        to_type(schema, &mut context)
    };
    let rendered = render(true);
    let Some(max_chars) = max_chars else {
        return rendered;
    };
    if js::utf16_len(&rendered) <= max_chars {
        return rendered;
    }
    let untagged = render(false);
    if js::utf16_len(&untagged) <= max_chars {
        untagged
    } else {
        "unknown".to_owned()
    }
}

struct SchemaContext<'a> {
    root: &'a Value,
    /// References being expanded on the current path, to stop at recursive types.
    resolving: HashSet<String>,
    expansions: usize,
    /// Whether property comments include constraint tags.
    tags: bool,
}

/// `decodeURIComponent`; `None` where JavaScript would throw.
fn decode_uri_component(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = segment.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn resolve_ref<'a>(reference: &str, root: &'a Value) -> Option<&'a Value> {
    if reference != "#" && !reference.starts_with("#/") {
        return None;
    }
    let mut current = root;
    for segment in reference
        .get(2..)
        .unwrap_or("")
        .split('/')
        .filter(|segment| !segment.is_empty())
    {
        let key = decode_uri_component(segment)?
            .replace("~1", "/")
            .replace("~0", "~");
        current = current.as_object()?.get(&key)?;
    }
    matches!(current, Value::Bool(_) | Value::Object(_)).then_some(current)
}

fn to_type(schema: &Value, context: &mut SchemaContext<'_>) -> String {
    let object = match schema {
        Value::Bool(true) => return "unknown".to_owned(),
        Value::Bool(false) => return "never".to_owned(),
        Value::Object(object) => object,
        _ => return "unknown".to_owned(),
    };
    if let Some(Value::String(reference)) = object.get("$ref") {
        if context.resolving.contains(reference) || context.expansions >= MAX_REF_EXPANSIONS {
            return "unknown".to_owned();
        }
        let Some(target) = resolve_ref(reference, context.root) else {
            return "unknown".to_owned();
        };
        context.expansions += 1;
        context.resolving.insert(reference.clone());
        let rendered = to_type(target, context);
        context.resolving.remove(reference);
        return rendered;
    }
    if let Some(constant) = object.get("const") {
        return js::stringify(constant);
    }
    if let Some(Value::Array(values)) = object.get("enum") {
        return union(values.iter().map(js::stringify).collect());
    }
    let variants = match (object.get("anyOf"), object.get("oneOf")) {
        (Some(Value::Array(variants)), _) | (_, Some(Value::Array(variants))) => Some(variants),
        _ => None,
    };
    if let Some(variants) = variants {
        return union(
            variants
                .iter()
                .map(|variant| to_type(variant, context))
                .collect(),
        );
    }
    if let Some(Value::Array(parts)) = object.get("allOf") {
        let parts: Vec<String> = parts
            .iter()
            .map(|part| to_type(part, context))
            .filter(|part| part != "unknown")
            .collect();
        if parts.is_empty() {
            return "unknown".to_owned();
        }
        return parts
            .iter()
            .map(|part| {
                if part.contains(" | ") {
                    format!("({part})")
                } else {
                    part.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" & ");
    }
    match object.get("type") {
        Some(Value::Array(types)) => union(
            types
                .iter()
                .map(|entry| {
                    let mut single = object.clone();
                    single.insert("type".to_owned(), entry.clone());
                    to_type(&Value::Object(single), context)
                })
                .collect(),
        ),
        Some(Value::String(kind)) => match kind.as_str() {
            "string" => "string".to_owned(),
            "number" | "integer" => "number".to_owned(),
            "boolean" => "boolean".to_owned(),
            "null" => "null".to_owned(),
            "array" => array_type(object, context),
            "object" => object_type(object, context),
            _ => "unknown".to_owned(),
        },
        Some(_) => "unknown".to_owned(),
        None => {
            if ["properties", "additionalProperties", "required"]
                .iter()
                .any(|key| object.contains_key(*key))
            {
                object_type(object, context)
            } else if object.contains_key("items") || object.contains_key("prefixItems") {
                array_type(object, context)
            } else {
                "unknown".to_owned()
            }
        }
    }
}

fn array_type(schema: &Map<String, Value>, context: &mut SchemaContext<'_>) -> String {
    match schema.get("items") {
        Some(items) if !items.is_array() => return format!("Array<{}>", to_type(items, context)),
        _ => {}
    }
    let tuple = match (schema.get("prefixItems"), schema.get("items")) {
        (Some(Value::Array(items)), _) | (_, Some(Value::Array(items))) => items.as_slice(),
        _ => &[],
    };
    if tuple.is_empty() {
        return "unknown[]".to_owned();
    }
    let items: Vec<String> = tuple.iter().map(|item| to_type(item, context)).collect();
    format!("[{}]", items.join(", "))
}

/// JavaScript's line terminators, which end a `//` comment.
fn is_line_terminator(character: char) -> bool {
    matches!(character, '\r' | '\n' | '\u{2028}' | '\u{2029}')
}

fn description_of(property: &Value) -> &str {
    match property.get("description") {
        Some(Value::String(description)) => description.trim(),
        _ => "",
    }
}

/// Constraints of a property that its TypeScript type cannot express, as
/// JSDoc-style tags: `@integer @minimum 1 @maximum 100 @default 20`. Empty
/// when there are none.
fn constraint_tags(schema: &Value) -> String {
    let Value::Object(property) = schema else {
        return String::new();
    };
    // Optional fields such as Pydantic's `int | None` keep their constraints in the non-null branch.
    // The branch's constraints apply together with the property's, so both are read.
    let variants = match property.get("anyOf") {
        Some(Value::Array(variants)) => Some(variants),
        _ => match property.get("oneOf") {
            Some(Value::Array(variants)) => Some(variants),
            _ => None,
        },
    };
    let non_null: Vec<&Value> = variants.map_or_else(Vec::new, |variants| {
        variants
            .iter()
            .filter(|variant| variant.get("type") != Some(&Value::String("null".to_owned())))
            .collect()
    });
    let mut branches = vec![property];
    if let [Value::Object(branch)] = non_null.as_slice() {
        branches.push(branch);
    }
    // Draft-04 and OpenAPI 3.0 make `minimum`/`maximum` exclusive with a boolean `exclusiveMinimum`/`exclusiveMaximum`.
    let schemas: Vec<Map<String, Value>> = branches
        .into_iter()
        .map(|schema| {
            let mut adjusted = schema.clone();
            for (bound, exclusive) in [
                ("minimum", "exclusiveMinimum"),
                ("maximum", "exclusiveMaximum"),
            ] {
                if schema.get(exclusive) == Some(&Value::Bool(true)) {
                    adjusted.remove(bound);
                    match schema.get(bound) {
                        Some(value) => adjusted.insert(exclusive.to_owned(), value.clone()),
                        None => adjusted.remove(exclusive),
                    };
                }
            }
            adjusted
        })
        .collect();

    let mut tags: Vec<String> = Vec::new();
    let is_integer = |kind: Option<&Value>| match kind {
        Some(Value::String(kind)) => kind == "integer",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "integer"),
        _ => false,
    };
    if schemas.iter().any(|schema| is_integer(schema.get("type"))) {
        tags.push("@integer".to_owned());
    }
    for keyword in TAG_KEYWORDS {
        let values: Vec<Option<&Value>> =
            schemas.iter().map(|schema| schema.get(keyword)).collect();
        let lower = LOWER_BOUNDS.contains(&keyword);
        if lower || UPPER_BOUNDS.contains(&keyword) {
            let numbers: Vec<f64> = values
                .iter()
                .filter_map(|value| value.and_then(Value::as_f64))
                .collect();
            let bound = if lower {
                numbers.iter().copied().reduce(f64::max)
            } else {
                numbers.iter().copied().reduce(f64::min)
            };
            if let Some(bound) = bound {
                tags.push(format!("@{keyword} {}", js::number_to_string(bound)));
            }
            continue;
        }
        let mut seen: Vec<&Value> = Vec::new();
        for value in values.into_iter().flatten() {
            if seen.contains(&value) {
                continue;
            }
            seen.push(value);
            match value {
                Value::Number(number) => {
                    tags.push(format!(
                        "@{keyword} {}",
                        js::number_to_string(number.as_f64().unwrap_or(f64::NAN))
                    ));
                }
                Value::String(text) => tags.push(format!("@{keyword} {text}")),
                _ => {}
            }
        }
    }
    if schemas
        .iter()
        .any(|schema| schema.get("uniqueItems") == Some(&Value::Bool(true)))
    {
        tags.push("@uniqueItems".to_owned());
    }
    // Annotations, unlike constraints, come from the property itself.
    match property.get("default") {
        None | Some(Value::Null) => {}
        Some(default) => tags.push(format!("@default {}", js::stringify(default))),
    }
    if property.get("deprecated") == Some(&Value::Bool(true)) {
        tags.push("@deprecated".to_owned());
    }
    tags.into_iter()
        .filter(|tag| js::utf16_len(tag) <= MAX_TAG_CHARS)
        .map(|tag| {
            let mut escaped = String::with_capacity(tag.len());
            for character in tag.chars() {
                if is_line_terminator(character) {
                    escaped.push_str(&format!("\\u{:04x}", character as u32));
                } else {
                    escaped.push(character);
                }
            }
            escaped
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn object_type(schema: &Map<String, Value>, context: &mut SchemaContext<'_>) -> String {
    let empty = Map::new();
    let properties = match schema.get("properties") {
        Some(Value::Object(properties)) => properties,
        _ => &empty,
    };
    let required: Vec<&Value> = match schema.get("required") {
        Some(Value::Array(required)) => required.iter().collect(),
        _ => Vec::new(),
    };
    let mut names: Vec<&String> = properties.keys().collect();
    // JavaScript sorts strings by UTF-16 code units.
    names.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    let mut members: Vec<String> = names
        .iter()
        .map(|name| {
            let optional = if required
                .iter()
                .any(|entry| entry.as_str() == Some(name.as_str()))
            {
                ""
            } else {
                "?"
            };
            let property = properties.get(name.as_str()).unwrap_or(&Value::Null);
            format!(
                "{}{optional}: {};",
                property_key(name),
                to_type(property, context)
            )
        })
        .collect();
    match schema.get("additionalProperties") {
        None if names.is_empty() => members.push("[key: string]: unknown;".to_owned()),
        None | Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => members.push("[key: string]: unknown;".to_owned()),
        Some(additional) => {
            members.push(format!("[key: string]: {};", to_type(additional, context)))
        }
    }
    if members.is_empty() {
        return "{}".to_owned();
    }
    let comments: Vec<Vec<String>> = names
        .iter()
        .map(|name| {
            let property = properties.get(name.as_str()).unwrap_or(&Value::Null);
            let mut lines: Vec<String> = description_of(property)
                .split(is_line_terminator)
                .map(|line| line.trim().to_owned())
                .filter(|line| !line.is_empty())
                .collect();
            let tags = if context.tags {
                constraint_tags(property)
            } else {
                String::new()
            };
            if !tags.is_empty() {
                lines.push(tags);
            }
            lines
        })
        .collect();
    if comments.iter().all(Vec::is_empty) {
        return format!("{{ {} }}", members.join(" "));
    }
    let mut lines = vec!["{".to_owned()];
    for (index, comment) in comments.iter().enumerate() {
        for line in comment {
            lines.push(format!("{INDENT}// {line}"));
        }
        lines.push(format!(
            "{INDENT}{}",
            members[index].replace('\n', &format!("\n{INDENT}"))
        ));
    }
    for member in &members[names.len()..] {
        lines.push(format!("{INDENT}{member}"));
    }
    lines.push("}".to_owned());
    lines.join("\n")
}

/// Pi's `test/declarations.test.ts`, case for case.
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn to_type(schema: Value) -> String {
        schema_to_type(&schema, None)
    }

    #[test]
    fn renders_primitives_literals_and_unions() {
        assert_eq!(to_type(json!({ "type": "string" })), "string");
        assert_eq!(to_type(json!({ "type": "integer" })), "number");
        assert_eq!(
            to_type(json!({ "type": ["string", "null"] })),
            "string | null"
        );
        assert_eq!(to_type(json!({ "const": "a" })), "\"a\"");
        assert_eq!(
            to_type(json!({ "enum": ["a", 1, null] })),
            "\"a\" | 1 | null"
        );
        assert_eq!(
            to_type(json!({ "anyOf": [{ "type": "string" }, { "type": "number" }] })),
            "string | number"
        );
        assert_eq!(
            to_type(json!({ "anyOf": [{ "type": "string" }, {}] })),
            "unknown"
        );
        assert_eq!(
            to_type(
                json!({ "allOf": [{ "anyOf": [{ "type": "string" }, { "type": "number" }] }, { "const": 1 }] })
            ),
            "(string | number) & 1"
        );
        assert_eq!(to_type(json!({ "$ref": "#/defs/x" })), "unknown");
        assert_eq!(to_type(json!(true)), "unknown");
        assert_eq!(to_type(json!(false)), "never");
    }

    #[test]
    fn renders_objects_on_one_line_with_sorted_properties() {
        assert_eq!(
            to_type(json!({
                "type": "object",
                "properties": { "city": { "type": "string" }, "max-lines": { "type": "number" } },
                "required": ["city"],
                "additionalProperties": false,
            })),
            "{ city: string; \"max-lines\"?: number; }"
        );
        assert_eq!(
            to_type(json!({ "type": "object", "additionalProperties": { "type": "number" } })),
            "{ [key: string]: number; }"
        );
        assert_eq!(
            to_type(json!({ "type": "object" })),
            "{ [key: string]: unknown; }"
        );
        assert_eq!(
            to_type(json!({ "type": "object", "properties": {}, "additionalProperties": false })),
            "{}"
        );
    }

    #[test]
    fn puts_property_descriptions_on_comment_lines() {
        assert_eq!(
            to_type(json!({
                "type": "object",
                "properties": {
                    "weather": {
                        "type": "array",
                        "description": "look up weather for a given list of locations",
                        "items": { "type": "object", "properties": { "location": { "type": "string" } }, "required": ["location"] },
                    },
                },
                "required": ["weather"],
            })),
            "{\n  // look up weather for a given list of locations\n  weather: Array<{ location: string; }>;\n}"
        );
        assert_eq!(
            to_type(json!({
                "type": "object",
                "properties": {
                    "outer": {
                        "type": "object",
                        "description": "Outer",
                        "properties": { "inner": { "type": "string", "description": "Inner" } },
                    },
                },
            })),
            "{\n  // Outer\n  outer?: {\n    // Inner\n    inner?: string;\n  };\n}"
        );
    }

    // https://github.com/earendil-works/pi/issues/10707
    #[test]
    fn puts_constraints_the_type_cannot_express_on_property_comment_lines() {
        assert_eq!(
            to_type(json!({
                "type": "object",
                "properties": {
                    "timeoutMs": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 30000,
                        "default": 0,
                        "description": "Wait in milliseconds.",
                    },
                    "id": { "type": "string", "pattern": "^[a-z]+$", "maxLength": 8 },
                    "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 5, "uniqueItems": true, "deprecated": true },
                    "name": { "type": "string" },
                },
                "required": ["id"],
            })),
            [
                "{",
                "  // @maxLength 8 @pattern ^[a-z]+$",
                "  id: string;",
                "  name?: string;",
                "  // @maxItems 5 @uniqueItems @deprecated",
                "  tags?: Array<string>;",
                "  // Wait in milliseconds.",
                "  // @integer @minimum 0 @maximum 30000 @default 0",
                "  timeoutMs?: number;",
                "}",
            ]
            .join("\n")
        );
    }

    #[test]
    fn reads_constraints_of_optional_fields_from_the_non_null_branch() {
        // Pydantic's `limit: int | None = Field(None, ge=1, le=100)`.
        assert_eq!(
            to_type(json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "anyOf": [{ "type": "integer", "minimum": 1, "maximum": 100 }, { "type": "null" }],
                        "default": null,
                        "title": "Limit",
                    },
                },
            })),
            "{\n  // @integer @minimum 1 @maximum 100\n  limit?: number | null;\n}"
        );
        // Constraints of the property and the branch both apply.
        assert_eq!(
            to_type(json!({
                "type": "object",
                "properties": {
                    "n": {
                        "anyOf": [{ "type": "integer", "minimum": 10, "maximum": 50, "format": "int32" }, { "type": "null" }],
                        "minimum": 0,
                        "maximum": 20,
                        "format": "port",
                    },
                },
            })),
            "{\n  // @integer @minimum 10 @maximum 20 @format port @format int32\n  n?: number | null;\n}"
        );
    }

    #[test]
    fn reads_draft_04_boolean_exclusive_bounds() {
        assert_eq!(
            to_type(json!({
                "type": "object",
                "properties": {
                    "n": { "type": "integer", "minimum": 0, "exclusiveMinimum": true, "maximum": 10, "exclusiveMaximum": false },
                    "m": { "anyOf": [{ "type": "number", "maximum": 5, "exclusiveMaximum": true }, { "type": "null" }], "maximum": 8 },
                },
            })),
            [
                "{",
                "  // @maximum 8 @exclusiveMaximum 5",
                "  m?: number | null;",
                "  // @integer @exclusiveMinimum 0 @maximum 10",
                "  n?: number;",
                "}",
            ]
            .join("\n")
        );
    }

    #[test]
    fn leaves_out_long_tags_and_escapes_line_terminators() {
        assert_eq!(
            to_type(json!({
                "type": "object",
                "properties": { "config": { "type": "object", "default": { "text": "x".repeat(100) } } },
            })),
            "{ config?: { [key: string]: unknown; }; }"
        );
        assert_eq!(
            to_type(json!({
                "type": "object",
                "properties": {
                    "sep": { "type": "string", "pattern": "^[\r\n\u{2028}\u{2029}]$", "default": "\u{2028}", "description": "a\u{2028}b" },
                },
            })),
            [
                "{",
                "  // a",
                "  // b",
                "  // @pattern ^[\\u000d\\u000a\\u2028\\u2029]$ @default \"\\u2028\"",
                "  sep?: string;",
                "}",
            ]
            .join("\n")
        );
    }

    #[test]
    fn drops_constraint_tags_before_rendering_a_type_over_the_budget_as_unknown() {
        let schema =
            json!({ "type": "object", "properties": { "n": { "type": "integer", "minimum": 1 } } });
        assert_eq!(
            schema_to_type(&schema, None),
            "{\n  // @integer @minimum 1\n  n?: number;\n}"
        );
        assert_eq!(schema_to_type(&schema, Some(20)), "{ n?: number; }");
        assert_eq!(schema_to_type(&schema, Some(10)), "unknown");
    }

    #[test]
    fn resolves_local_references_and_stops_at_recursive_ones() {
        let schema = json!({
            "type": "object",
            "properties": {
                "item": { "$ref": "#/$defs/Item" },
                "legacy": { "$ref": "#/definitions/Legacy" },
                "remote": { "$ref": "https://example.com/schema.json" },
            },
            "required": ["item"],
            "$defs": {
                "Item": {
                    "type": "object",
                    "properties": { "id": { "type": "string" }, "parent": { "$ref": "#/$defs/Item" } },
                    "required": ["id"],
                },
            },
            "definitions": { "Legacy": { "enum": ["a", "b"] } },
        });
        assert_eq!(
            schema_to_type(&schema, None),
            "{ item: { id: string; parent?: unknown; }; legacy?: \"a\" | \"b\"; remote?: unknown; }"
        );
    }

    #[test]
    fn renders_arrays_and_tuples() {
        assert_eq!(
            to_type(json!({ "type": "array", "items": { "type": "string" } })),
            "Array<string>"
        );
        assert_eq!(
            to_type(
                json!({ "type": "array", "prefixItems": [{ "type": "string" }, { "type": "number" }] })
            ),
            "[string, number]"
        );
        assert_eq!(to_type(json!({ "type": "array" })), "unknown[]");
    }

    #[test]
    fn renders_types_over_the_budget_as_unknown() {
        let properties: Map<String, Value> = (0..50)
            .map(|index| (format!("field{index}"), json!({ "type": "string" })))
            .collect();
        let schema = json!({ "type": "object", "properties": properties });
        assert_eq!(schema_to_type(&schema, Some(100)), "unknown");
        assert!(schema_to_type(&schema, None).contains("field49?: string;"));
    }

    fn function(name: &str) -> ScriptFunction {
        ScriptFunction {
            name: name.to_owned(),
            ..ScriptFunction::default()
        }
    }

    #[test]
    fn renders_signatures_with_normalized_identifiers() {
        let tool = ScriptFunction {
            input_schema: Some(json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
                "additionalProperties": false,
            })),
            output_schema: Some(
                json!({ "type": "object", "properties": { "ok": { "type": "boolean" } }, "required": ["ok"] }),
            ),
            ..function("hidden-dynamic-tool")
        };
        assert_eq!(
            render_tool_signature(&tool, None),
            "hidden_dynamic_tool(args: { city: string; }): Promise<{ ok: boolean; }>;"
        );
        assert_eq!(
            render_tool_signature(&function("free"), None),
            "free(args: unknown): Promise<unknown>;"
        );
    }

    #[test]
    fn renders_output_schemas_shaped_like_an_mcp_call_tool_result_as_plain_types() {
        let tool = ScriptFunction {
            input_schema: Some(json!({ "type": "object", "properties": {} })),
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "content": { "type": "array", "items": { "type": "object" } },
                    "isError": { "type": "boolean" },
                    "_meta": { "type": "object" },
                },
                "required": ["content"],
            })),
            ..function("plain")
        };
        assert_eq!(
            render_tool_signature(&tool, None),
            "plain(args: { [key: string]: unknown; }): Promise<{ _meta?: { [key: string]: unknown; }; content: Array<{ [key: string]: unknown; }>; isError?: boolean; }>;"
        );
    }

    #[test]
    fn renders_the_per_tool_sample() {
        let tool = ScriptFunction {
            description: Some("bar".to_owned()),
            input_schema: Some(json!({ "type": "string" })),
            ..function("foo")
        };
        assert_eq!(
            render_tool_sample(&tool, None),
            "bar\n\ncodemode tool declaration:\n```ts\ndeclare const tools: { foo(args: string): Promise<unknown>; };\n```"
        );
    }

    #[test]
    fn renders_tools_and_globals() {
        let text = render_declarations(
            &[
                ScriptFunction {
                    description: Some("Read a file.\nSecond line.".to_owned()),
                    input_schema: Some(
                        json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
                    ),
                    output_schema: Some(json!({ "type": "string" })),
                    ..function("read")
                },
                function("remote-api"),
            ],
            &[ScriptFunction {
                description: Some("Attach it.".to_owned()),
                input_schema: Some(json!({ "type": "string" })),
                ..function("attach")
            }],
        );
        assert_eq!(
            text,
            [
                "declare const tools: {",
                "  /**",
                "   * Read a file.",
                "   * Second line.",
                "   */",
                "  read(args: { path: string; }): Promise<string>;",
                "  remote_api(args: unknown): Promise<unknown>;",
                "};",
                "",
                "/** Attach it. */",
                "declare function attach(args: string): Promise<unknown>;",
            ]
            .join("\n")
        );
    }

    #[test]
    fn renders_namespaced_globals_and_explicit_signatures() {
        let text = render_declarations(
            &[],
            &[
                ScriptFunction {
                    description: Some("List models.".to_owned()),
                    signature: Some("(type: string): Promise<string[]>".to_owned()),
                    ..function("models.list")
                },
                ScriptFunction {
                    input_schema: Some(json!({ "type": "string" })),
                    ..function("models.get")
                },
                ScriptFunction {
                    signature: Some("(): void".to_owned()),
                    ..function("plain")
                },
            ],
        );
        assert_eq!(
            text,
            [
                "declare function plain(): void;",
                "",
                "declare const models: {",
                "  /** List models. */",
                "  list(type: string): Promise<string[]>;",
                "  get(args: string): Promise<unknown>;",
                "};",
            ]
            .join("\n")
        );
    }

    #[test]
    fn escapes_comment_terminators_in_descriptions() {
        let text = render_declarations(
            &[ScriptFunction {
                description: Some("a */ b".to_owned()),
                ..function("x")
            }],
            &[],
        );
        assert!(text.contains("/** a *\\/ b */"));
    }
}
