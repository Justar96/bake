//! The catalog's one message projection, `image/offload`, as
//! `imageOffloadProjection` in
//! `packages/compaction/compaction-image-offload/src/projection.ts` and
//! `offloadMessageImages` in `project-message.ts` beside it define it.
//!
//! Admission reads a decision without judging it: [`decision`] records each
//! target's shape and normalized numbers, and the restoring
//! [`crate::request::RequestFold`] validates and projects the targets one at
//! a time, in the projection's order, so an earlier target's error wins over a
//! later target's shape. Derivation constructs its Session without
//! projections, so its fold refuses the decision without validating it.
//!
//! `seq` and image indexes are JavaScript numbers. [`crate::parse_json`]
//! rounds a decimal spelling as `JSON.parse` does, and the scan has already refused integer parts longer than 768
//! digits and numbers outside the `f64` range as native limits. So, unlike
//! settlement coordinates, which leave fraction and exponent spellings
//! undecided, this projection decides them: `5.0`, `5e0`, and `1e-400` (+0)
//! are exact integers here as there, and -0, fractions, and unsafe integers
//! are refused. Only the `seq` and index values are read; other numbers in
//! the decision are never copied. The message walk reads
//! blocks no check has validated: a `null` block or a `tool-result` block
//! whose `content` is not an array makes JavaScript throw a `TypeError`,
//! which this port reports as [`Walk::Coercion`] instead of claiming the
//! engine's text.

use serde_json::{Map, Value};

use crate::MAX_SAFE_INTEGER;
use crate::json_parse::{clone_value, dismantle};

/// Why `imageOffloadProjection` rejects a decision. Session construction
/// throws "invalid seed event at index `seq`: " followed by
/// [`OffloadRejection::message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffloadRejection {
    /// `data` is not an object whose only member is a non-empty `targets`
    /// array.
    Data,
    /// A target is not an object of exactly a valid `seq` and a non-empty
    /// `imageIndexes` array.
    Target,
    /// An earlier target of the same decision names the same seq.
    DuplicateTarget { seq: u64 },
    /// The target is not a current surface node.
    NotCurrent { seq: u64 },
    /// The target node is not a `user/message` or `tool/result`.
    TargetType { seq: u64 },
    /// An image index is not a non-negative safe integer greater than the
    /// one before it.
    ImageIndexes,
    /// The selected image, counted depth-first, is already offloaded.
    AlreadyOffloaded { index: u64 },
    /// The message has no image at the first unmatched selected index.
    MissingIndex { index: u64 },
}

impl OffloadRejection {
    /// The projection's exact error message.
    pub fn message(self) -> String {
        let text = match self {
            Self::Data => "data must contain a nonempty targets array".to_owned(),
            Self::Target => "each target must contain a seq and nonempty imageIndexes".to_owned(),
            Self::DuplicateTarget { seq } => format!("duplicate target seq {seq}"),
            Self::NotCurrent { seq } => format!("target seq {seq} is not a current surface node"),
            Self::TargetType { seq } => {
                format!("target seq {seq} must be user/message or tool/result")
            }
            Self::ImageIndexes => {
                "imageIndexes must be strictly increasing non-negative safe integers".to_owned()
            }
            Self::AlreadyOffloaded { index } => format!("image index {index} is already offloaded"),
            Self::MissingIndex { index } => format!("image index {index} does not exist"),
        };
        format!("image/offload: {text}")
    }
}

/// An `image/offload` payload as admission read it: `None` when `data` fails
/// the projection's first check, otherwise each target in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decision(pub(crate) Option<Vec<Target>>);

/// One target of a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// Fails the target shape check.
    Malformed,
    /// A well-shaped target. `indexes` is `None` when an index fails, a check
    /// the projection runs only after the target's node checks.
    Valid { seq: u64, indexes: Option<Vec<u64>> },
}

/// Read a decision's targets without validating them against any history.
pub(crate) fn decision(data: &Value) -> Decision {
    let targets = data
        .as_object()
        .filter(|fields| fields.len() == 1)
        .and_then(|fields| fields.get("targets"))
        .and_then(Value::as_array)
        .filter(|targets| !targets.is_empty());
    Decision(targets.map(|targets| targets.iter().map(target).collect()))
}

fn target(value: &Value) -> Target {
    let Some(fields) = value.as_object().filter(|fields| fields.len() == 2) else {
        return Target::Malformed;
    };
    let seq = fields.get("seq").and_then(index);
    let indexes = fields
        .get("imageIndexes")
        .and_then(Value::as_array)
        .filter(|indexes| !indexes.is_empty());
    let (Some(seq), Some(indexes)) = (seq, indexes) else {
        return Target::Malformed;
    };
    let mut previous = None;
    let indexes = indexes
        .iter()
        .map(|value| {
            let index = index(value).filter(|index| previous.is_none_or(|last| *index > last))?;
            previous = Some(index);
            Some(index)
        })
        .collect();
    Target::Valid { seq, indexes }
}

/// The projection's `isIndex`: a non-negative safe integer other than -0, as
/// `JSON.parse` reads the number.
fn index(value: &Value) -> Option<u64> {
    let Value::Number(number) = value else {
        return None;
    };
    if let Some(index) = number.as_u64() {
        return (index <= MAX_SAFE_INTEGER).then_some(index);
    }
    if number.is_i64() {
        return None;
    }
    let value = number.as_f64()?;
    // 2^53 − 1 is exact in `f64`, so an integral value in range converts exactly.
    (value.is_sign_positive() && value.fract() == 0.0 && value <= MAX_SAFE_INTEGER as f64)
        .then_some(value as u64)
}

/// Why the message walk stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Walk {
    Rejected(OffloadRejection),
    /// JavaScript would throw a `TypeError` here.
    Coercion,
}

/// `offloadMessageImages`: mark the selected images of `message`, counted
/// depth-first through `tool-result` blocks, as `offloaded: true`. Unchanged
/// members keep their positions, and a replaced `offloaded` member keeps its
/// place, as an object spread does. `indexes` is non-empty and strictly
/// increasing. The walk and its copies are iterative, so a qualified surface
/// payload of any depth is safe; the caller drops the projection with
/// [`crate::dismantle`] or keeps it in a [`crate::json_parse::Deep`].
pub(crate) fn offload_images(
    message: &Map<String, Value>,
    indexes: &[u64],
) -> Result<Map<String, Value>, Walk> {
    let mut walk = Images {
        indexes,
        image: 0,
        selected: 0,
    };
    let content = message["content"]
        .as_array()
        .expect("admission proved message content an array");
    let content = walk.visit(content)?;
    if let Some(&index) = indexes.get(walk.selected) {
        if let Some(content) = content {
            dismantle(Value::Array(content));
        }
        return Err(Walk::Rejected(OffloadRejection::MissingIndex { index }));
    }
    Ok(match content {
        Some(content) => with_member(message, "content", Value::Array(content)),
        None => with_member(message, "content", clone_value(&message["content"])),
    })
}

/// `{...fields, [key]: value}` copied without recursing: the other members
/// are copied in place, `key` keeps its position or is appended, and the
/// replaced member is never copied, so no deep value drops recursively.
fn with_member(fields: &Map<String, Value>, key: &str, value: Value) -> Map<String, Value> {
    let mut copy = Map::new();
    let mut value = Some(value);
    for (name, member) in fields {
        if name == key {
            copy.insert(name.clone(), value.take().unwrap_or(Value::Null));
        } else {
            copy.insert(name.clone(), clone_value(member));
        }
    }
    if let Some(value) = value {
        copy.insert(key.to_owned(), value);
    }
    copy
}

struct Images<'a> {
    indexes: &'a [u64],
    image: u64,
    selected: usize,
}

/// One block list being walked: its blocks, the next position, the
/// projected copy once a block changed, and the `tool-result` block that
/// holds the list, `None` for the message content.
struct Blocks<'a> {
    blocks: &'a [Value],
    position: usize,
    next: Option<Vec<Value>>,
    owner: Option<&'a Map<String, Value>>,
}

impl<'a> Blocks<'a> {
    const fn new(blocks: &'a [Value], owner: Option<&'a Map<String, Value>>) -> Self {
        Self {
            blocks,
            position: 0,
            next: None,
            owner,
        }
    }

    /// The projected copy, leaving none for [`Drop`] to dismantle.
    fn take_next(&mut self) -> Option<Vec<Value>> {
        self.next.take()
    }

    /// Record the current block's projection, or its copy once an earlier
    /// block changed, and move past it.
    fn record(&mut self, projected: Option<Value>) {
        if projected.is_some() && self.next.is_none() {
            self.next = Some(
                self.blocks[..self.position]
                    .iter()
                    .map(clone_value)
                    .collect(),
            );
        }
        if let Some(next) = &mut self.next {
            next.push(projected.unwrap_or_else(|| clone_value(&self.blocks[self.position])));
        }
        self.position += 1;
    }
}

/// A walk that stops early drops its open lists' partial copies, which may
/// hold deep blocks, without recursing.
impl Drop for Blocks<'_> {
    fn drop(&mut self) {
        if let Some(next) = self.next.take() {
            dismantle(Value::Array(next));
        }
    }
}

impl Images<'_> {
    /// The projected blocks, or `None` when none changed. Nested
    /// `tool-result` content is walked depth-first on an explicit stack, so
    /// any nesting is safe.
    fn visit(&mut self, blocks: &[Value]) -> Result<Option<Vec<Value>>, Walk> {
        let mut stack = vec![Blocks::new(blocks, None)];
        loop {
            let frame = stack.last_mut().expect("an open block list");
            if let Some(block) = frame.blocks.get(frame.position) {
                let projected = match block {
                    Value::Null => return Err(Walk::Coercion),
                    Value::Object(fields) => match fields.get("type").and_then(Value::as_str) {
                        Some("image") => self.image(fields)?,
                        Some("tool-result") => {
                            let Some(Value::Array(content)) = fields.get("content") else {
                                return Err(Walk::Coercion);
                            };
                            stack.push(Blocks::new(content, Some(fields)));
                            continue;
                        }
                        _ => None,
                    },
                    // A primitive or array block has no `type`.
                    _ => None,
                };
                frame.record(projected);
                continue;
            }
            let mut done = stack.pop().expect("the frame just read");
            let next = done.take_next();
            let Some(parent) = stack.last_mut() else {
                return Ok(next);
            };
            let owner = done.owner.expect("a nested list has its tool-result block");
            parent.record(next.map(|content| {
                Value::Object(with_member(owner, "content", Value::Array(content)))
            }));
        }
    }

    fn image(&mut self, fields: &Map<String, Value>) -> Result<Option<Value>, Walk> {
        let index = self.image;
        self.image += 1;
        if self.indexes.get(self.selected) != Some(&index) {
            return Ok(None);
        }
        if fields.get("offloaded") == Some(&Value::Bool(true)) {
            return Err(Walk::Rejected(OffloadRejection::AlreadyOffloaded { index }));
        }
        self.selected += 1;
        Ok(Some(Value::Object(with_member(
            fields,
            "offloaded",
            Value::Bool(true),
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).expect("JSON")
    }

    #[test]
    fn indexes_are_the_numbers_json_parse_reads() {
        for (text, expected) in [
            ("0", Some(0)),
            ("4", Some(4)),
            ("4.0", Some(4)),
            ("4e0", Some(4)),
            ("0.4e1", Some(4)),
            ("1e-400", Some(0)),
            ("0.0", Some(0)),
            ("1.0000000000000001", Some(1)),
            ("9007199254740991", Some(9_007_199_254_740_991)),
            ("9007199254740991.0", Some(9_007_199_254_740_991)),
            ("9007199254740990.9999999", Some(9_007_199_254_740_991)),
            ("-0", None),
            ("-0.0", None),
            ("-1e-400", None),
            ("-1", None),
            ("0.5", None),
            ("9007199254740992", None),
            ("9007199254740991.5", None),
            ("18446744073709551616", None),
            ("1e300", None),
            ("\"4\"", None),
            ("null", None),
            ("[4]", None),
        ] {
            assert_eq!(index(&parse(text)), expected, "{text}");
        }
    }

    #[test]
    fn a_projected_block_keeps_its_member_positions() {
        let message = parse(
            r#"{"id":"m","content":[{"type":"image","offloaded":false,"a":1},{"type":"tool-result","content":[{"type":"image","b":2}],"z":0}],"role":"user"}"#,
        );
        let projected =
            offload_images(message.as_object().expect("object"), &[0, 1]).expect("projected");
        assert_eq!(
            serde_json::to_string(&projected).expect("text"),
            r#"{"id":"m","content":[{"type":"image","offloaded":true,"a":1},{"type":"tool-result","content":[{"type":"image","b":2,"offloaded":true}],"z":0}],"role":"user"}"#
        );
    }

    #[test]
    fn the_walk_reports_the_first_failure_in_depth_first_order() {
        let walk = |content: Value, indexes: &[u64]| {
            let message = json!({"id": "m", "content": content});
            offload_images(message.as_object().expect("object"), indexes).map(drop)
        };
        let image = json!({"type": "image"});
        let done = json!({"type": "image", "offloaded": true});
        assert_eq!(
            walk(json!([done, null]), &[0]),
            Err(Walk::Rejected(OffloadRejection::AlreadyOffloaded {
                index: 0
            }))
        );
        assert_eq!(walk(json!([image, null]), &[0]), Err(Walk::Coercion));
        assert_eq!(
            walk(json!([image, {"type": "tool-result"}]), &[0]),
            Err(Walk::Coercion)
        );
        assert_eq!(
            walk(
                json!([image, "text", 7, [], {"type": "tool-result", "content": []}]),
                &[0, 2]
            ),
            Err(Walk::Rejected(OffloadRejection::MissingIndex { index: 2 }))
        );
    }

    #[test]
    fn targets_keep_their_order_and_defer_index_checks() {
        let decision = |text: &str| decision(&parse(text)).0;
        assert_eq!(decision(r#"{"targets":[]}"#), None);
        assert_eq!(
            decision(r#"{"targets":[{"seq":1,"imageIndexes":[0]}],"x":1}"#),
            None
        );
        assert_eq!(decision("[]"), None);
        assert_eq!(
            decision(
                r#"{"targets":[{"seq":2,"imageIndexes":[1,1]},null,{"seq":1.0,"imageIndexes":[0,3e0]},{"seq":1,"imageIndexes":[]}]}"#
            ),
            Some(vec![
                Target::Valid {
                    seq: 2,
                    indexes: None
                },
                Target::Malformed,
                Target::Valid {
                    seq: 1,
                    indexes: Some(vec![0, 3])
                },
                Target::Malformed,
            ])
        );
    }
}
