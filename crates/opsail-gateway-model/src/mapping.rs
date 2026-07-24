//! Bounded declarative mapping into provider-neutral events.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::GatewayError;
use crate::event::{EventSequencer, MessagePhase, OpsailEventKind, OpsailEventV1};

pub const EVENT_MAPPING_SCHEMA_VERSION: u16 = 1;

const MAX_MAPPING_RULES: usize = 128;
const MAX_RULE_CONDITIONS: usize = 16;
const MAX_JSON_POINTER_BYTES: usize = 512;
const MAX_JSON_POINTER_DEPTH: usize = 32;
const MAX_LITERAL_BYTES: usize = 4 * 1024;
const MAX_MAPPED_IDENTIFIER_BYTES: usize = 512;
const MAX_MAPPED_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// A bounded declarative mapping from JSON wire events to Opsail's semantic
/// event vocabulary.
///
/// This handles structural differences such as discriminator names and field
/// locations. Stateful protocol conversions still belong in a code adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventMappingProfileV1 {
    pub version: u16,
    /// Whether rules see the decoded JSON value directly or an envelope with
    /// both the SSE event name and decoded data.
    #[serde(default)]
    pub input: MappingInputV1,
    /// RFC 6901 JSON Pointer that selects the wire event discriminator.
    pub discriminator: String,
    pub rules: Vec<EventMappingRuleV1>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MappingInputV1 {
    #[default]
    JsonData,
    SseEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventMappingRuleV1 {
    /// Exact scalar value selected by `discriminator`.
    #[serde(rename = "match")]
    pub match_value: Value,
    /// Optional exact-value predicates keyed by RFC 6901 JSON Pointer.
    #[serde(default, rename = "where")]
    pub conditions: BTreeMap<String, Value>,
    pub emit: MappedEventTypeV1,
    #[serde(default)]
    pub fields: BTreeMap<MappedEventFieldV1, MappedValueSourceV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappedValueSourceV1 {
    /// Read a value from the input event using an RFC 6901 JSON Pointer.
    pub pointer: Option<String>,
    /// Use this literal when the event is emitted.
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MappedEventTypeV1 {
    RunStarted,
    ReasoningSummaryDelta,
    ReasoningSummaryCompleted,
    AssistantTextDelta,
    ToolCallStarted,
    ToolCallArgumentsDelta,
    ToolCallCompleted,
    UsageUpdated,
    RunCompleted,
    RunFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MappedEventFieldV1 {
    RunId,
    ResponseId,
    Model,
    ItemId,
    SummaryIndex,
    Delta,
    Text,
    Phase,
    CallId,
    Name,
    Arguments,
    InputTokens,
    OutputTokens,
    TotalTokens,
    Code,
    Message,
}

/// Validated, stateful executor for one mapping profile.
pub struct EventMapper {
    profile: EventMappingProfileV1,
    sequencer: EventSequencer,
}

struct BuiltEvent {
    run_id: Option<String>,
    event: OpsailEventKind,
}

impl EventMappingProfileV1 {
    pub fn from_toml(source: &str) -> Result<Self, GatewayError> {
        let profile: Self = toml::from_str(source)
            .map_err(|error| GatewayError::invalid_mapping(bounded_message(&error)))?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), GatewayError> {
        if self.version != EVENT_MAPPING_SCHEMA_VERSION {
            return Err(GatewayError::invalid_mapping(format!(
                "unsupported mapping version {}; expected {EVENT_MAPPING_SCHEMA_VERSION}",
                self.version
            )));
        }
        validate_pointer("discriminator", &self.discriminator)?;
        if self.rules.is_empty() || self.rules.len() > MAX_MAPPING_RULES {
            return Err(GatewayError::invalid_mapping(format!(
                "mapping must contain between 1 and {MAX_MAPPING_RULES} rules"
            )));
        }
        for (index, rule) in self.rules.iter().enumerate() {
            rule.validate(index)?;
        }
        Ok(())
    }
}

impl EventMappingRuleV1 {
    fn validate(&self, index: usize) -> Result<(), GatewayError> {
        validate_scalar(&format!("rules[{index}].match"), &self.match_value)?;
        if self.conditions.len() > MAX_RULE_CONDITIONS {
            return Err(GatewayError::invalid_mapping(format!(
                "rules[{index}] may contain at most {MAX_RULE_CONDITIONS} conditions"
            )));
        }
        for (pointer, expected) in &self.conditions {
            validate_pointer(&format!("rules[{index}].where"), pointer)?;
            validate_literal(&format!("rules[{index}].where"), expected)?;
        }
        for (field, source) in &self.fields {
            if !self.emit.allows(*field) {
                return Err(GatewayError::invalid_mapping(format!(
                    "rules[{index}] field {field:?} is not valid for {:?}",
                    self.emit
                )));
            }
            source.validate(index, *field)?;
        }
        for required in self.emit.required_fields() {
            if !self.fields.contains_key(required) {
                return Err(GatewayError::invalid_mapping(format!(
                    "rules[{index}] {:?} requires field {required:?}",
                    self.emit
                )));
            }
        }
        Ok(())
    }
}

impl MappedValueSourceV1 {
    fn validate(&self, rule_index: usize, field: MappedEventFieldV1) -> Result<(), GatewayError> {
        match (&self.pointer, &self.value) {
            (Some(pointer), None) => {
                validate_pointer(&format!("rules[{rule_index}].fields.{field:?}"), pointer)
            }
            (None, Some(value)) => {
                validate_literal(&format!("rules[{rule_index}].fields.{field:?}"), value)
            }
            (Some(_), Some(_)) | (None, None) => Err(GatewayError::invalid_mapping(format!(
                "rules[{rule_index}] field {field:?} must set exactly one of pointer or value"
            ))),
        }
    }

    fn resolve(&self, input: &Value) -> Option<Value> {
        match (&self.pointer, &self.value) {
            (Some(pointer), None) => input.pointer(pointer).cloned(),
            (None, Some(value)) => Some(value.clone()),
            _ => None,
        }
    }
}

impl MappedEventTypeV1 {
    fn required_fields(self) -> &'static [MappedEventFieldV1] {
        use MappedEventFieldV1 as Field;
        match self {
            Self::RunStarted => &[Field::RunId],
            Self::ReasoningSummaryDelta => &[Field::Delta],
            Self::ReasoningSummaryCompleted => &[Field::ItemId, Field::SummaryIndex, Field::Text],
            Self::AssistantTextDelta => &[Field::Delta],
            Self::ToolCallStarted => &[Field::CallId, Field::Name],
            Self::ToolCallArgumentsDelta => &[Field::Delta],
            Self::ToolCallCompleted => &[Field::CallId],
            Self::UsageUpdated => &[],
            Self::RunCompleted => &[],
            Self::RunFailed => &[Field::Message],
        }
    }

    fn allows(self, field: MappedEventFieldV1) -> bool {
        use MappedEventFieldV1 as Field;
        match self {
            Self::RunStarted => matches!(field, Field::RunId | Field::Model),
            Self::ReasoningSummaryDelta => {
                matches!(field, Field::ItemId | Field::SummaryIndex | Field::Delta)
            }
            Self::ReasoningSummaryCompleted => {
                matches!(field, Field::ItemId | Field::SummaryIndex | Field::Text)
            }
            Self::AssistantTextDelta => {
                matches!(field, Field::ItemId | Field::Phase | Field::Delta)
            }
            Self::ToolCallStarted | Self::ToolCallCompleted => {
                matches!(
                    field,
                    Field::ItemId | Field::CallId | Field::Name | Field::Arguments
                )
            }
            Self::ToolCallArgumentsDelta => {
                matches!(field, Field::ItemId | Field::CallId | Field::Delta)
            }
            Self::UsageUpdated => matches!(
                field,
                Field::InputTokens | Field::OutputTokens | Field::TotalTokens
            ),
            Self::RunCompleted => matches!(field, Field::ResponseId | Field::Model),
            Self::RunFailed => matches!(field, Field::Code | Field::Message),
        }
    }
}

impl EventMapper {
    pub fn new(profile: EventMappingProfileV1) -> Result<Self, GatewayError> {
        profile.validate()?;
        Ok(Self {
            profile,
            sequencer: EventSequencer::default(),
        })
    }

    /// Map one decoded JSON wire event. Unmatched events are intentionally
    /// ignored; once a rule matches, missing or mistyped required fields fail
    /// closed with a bounded diagnostic.
    pub fn map(&mut self, input: &Value) -> Result<Vec<OpsailEventV1>, GatewayError> {
        let Some(discriminator) = input.pointer(&self.profile.discriminator) else {
            return Ok(Vec::new());
        };
        let mut built = Vec::new();
        for (index, rule) in self.profile.rules.iter().enumerate() {
            if discriminator != &rule.match_value || !conditions_match(rule, input) {
                continue;
            }
            built.push(build_event(rule, input, index)?);
        }

        let mut events = Vec::with_capacity(built.len());
        for built in built {
            if let Some(run_id) = built.run_id {
                self.sequencer.set_run_id(run_id);
            }
            events.push(self.sequencer.push(built.event));
        }
        Ok(events)
    }

    pub(crate) fn gateway_failure(&mut self, code: String, message: String) -> OpsailEventV1 {
        self.sequencer.push(OpsailEventKind::RunFailed {
            code: Some(code),
            message: Some(message),
        })
    }
}

fn conditions_match(rule: &EventMappingRuleV1, input: &Value) -> bool {
    rule.conditions
        .iter()
        .all(|(pointer, expected)| input.pointer(pointer) == Some(expected))
}

fn build_event(
    rule: &EventMappingRuleV1,
    input: &Value,
    rule_index: usize,
) -> Result<BuiltEvent, GatewayError> {
    use MappedEventFieldV1 as Field;

    let field = |name| {
        rule.fields
            .get(&name)
            .and_then(|source| source.resolve(input))
    };
    let optional_string = |name| {
        field(name)
            .map(|value| value_as_string(value, rule_index, name))
            .transpose()
    };
    let optional_u64 = |name| {
        field(name)
            .map(|value| value_as_u64(value, rule_index, name))
            .transpose()
    };
    let required_string =
        |name| optional_string(name)?.ok_or_else(|| missing_field(rule_index, name));
    let required_u64 = |name| optional_u64(name)?.ok_or_else(|| missing_field(rule_index, name));

    let (run_id, event) = match rule.emit {
        MappedEventTypeV1::RunStarted => {
            let run_id = required_string(Field::RunId)?;
            (
                Some(run_id),
                OpsailEventKind::RunStarted {
                    model: optional_string(Field::Model)?,
                },
            )
        }
        MappedEventTypeV1::ReasoningSummaryDelta => (
            None,
            OpsailEventKind::ReasoningSummaryDelta {
                item_id: optional_string(Field::ItemId)?,
                summary_index: optional_u64(Field::SummaryIndex)?,
                delta: required_string(Field::Delta)?,
            },
        ),
        MappedEventTypeV1::ReasoningSummaryCompleted => (
            None,
            OpsailEventKind::ReasoningSummaryCompleted {
                item_id: required_string(Field::ItemId)?,
                summary_index: required_u64(Field::SummaryIndex)?,
                text: required_string(Field::Text)?,
            },
        ),
        MappedEventTypeV1::AssistantTextDelta => (
            None,
            OpsailEventKind::AssistantTextDelta {
                item_id: optional_string(Field::ItemId)?,
                phase: field(Field::Phase)
                    .map(|value| value_as_phase(value, rule_index))
                    .transpose()?,
                delta: required_string(Field::Delta)?,
            },
        ),
        MappedEventTypeV1::ToolCallStarted => (
            None,
            OpsailEventKind::ToolCallStarted {
                item_id: optional_string(Field::ItemId)?,
                call_id: optional_string(Field::CallId)?,
                name: optional_string(Field::Name)?,
                arguments: optional_string(Field::Arguments)?,
            },
        ),
        MappedEventTypeV1::ToolCallArgumentsDelta => (
            None,
            OpsailEventKind::ToolCallArgumentsDelta {
                item_id: optional_string(Field::ItemId)?,
                call_id: optional_string(Field::CallId)?,
                delta: required_string(Field::Delta)?,
            },
        ),
        MappedEventTypeV1::ToolCallCompleted => (
            None,
            OpsailEventKind::ToolCallCompleted {
                item_id: optional_string(Field::ItemId)?,
                call_id: optional_string(Field::CallId)?,
                name: optional_string(Field::Name)?,
                arguments: optional_string(Field::Arguments)?,
            },
        ),
        MappedEventTypeV1::UsageUpdated => {
            let input_tokens = optional_u64(Field::InputTokens)?;
            let output_tokens = optional_u64(Field::OutputTokens)?;
            let total_tokens = optional_u64(Field::TotalTokens)?;
            if input_tokens.is_none() && output_tokens.is_none() && total_tokens.is_none() {
                return Err(GatewayError::invalid_mapping(format!(
                    "rules[{rule_index}] usage event resolved no token fields"
                )));
            }
            (
                None,
                OpsailEventKind::UsageUpdated {
                    input_tokens,
                    output_tokens,
                    total_tokens,
                },
            )
        }
        MappedEventTypeV1::RunCompleted => (
            None,
            OpsailEventKind::RunCompleted {
                response_id: optional_string(Field::ResponseId)?,
                model: optional_string(Field::Model)?,
            },
        ),
        MappedEventTypeV1::RunFailed => (
            None,
            OpsailEventKind::RunFailed {
                code: optional_string(Field::Code)?,
                message: Some(
                    required_string(Field::Message)?
                        .chars()
                        .take(2048)
                        .collect(),
                ),
            },
        ),
    };
    Ok(BuiltEvent { run_id, event })
}

fn value_as_string(
    value: Value,
    rule_index: usize,
    field: MappedEventFieldV1,
) -> Result<String, GatewayError> {
    let value = value.as_str().ok_or_else(|| {
        GatewayError::invalid_mapping(format!(
            "rules[{rule_index}] field {field:?} resolved a non-string value"
        ))
    })?;
    let is_text = matches!(
        field,
        MappedEventFieldV1::Delta
            | MappedEventFieldV1::Text
            | MappedEventFieldV1::Arguments
            | MappedEventFieldV1::Message
    );
    let limit = if is_text {
        MAX_MAPPED_TEXT_BYTES
    } else {
        MAX_MAPPED_IDENTIFIER_BYTES
    };
    if value.len() > limit {
        return Err(GatewayError::invalid_mapping(format!(
            "rules[{rule_index}] field {field:?} exceeded its {limit} byte limit"
        )));
    }
    if !is_text
        && (value.is_empty() || value.trim() != value || value.chars().any(char::is_control))
    {
        return Err(GatewayError::invalid_mapping(format!(
            "rules[{rule_index}] field {field:?} must be non-empty, trimmed, and control-free"
        )));
    }
    Ok(value.to_owned())
}

fn value_as_u64(
    value: Value,
    rule_index: usize,
    field: MappedEventFieldV1,
) -> Result<u64, GatewayError> {
    value.as_u64().ok_or_else(|| {
        GatewayError::invalid_mapping(format!(
            "rules[{rule_index}] field {field:?} resolved a non-u64 value"
        ))
    })
}

fn value_as_phase(value: Value, rule_index: usize) -> Result<MessagePhase, GatewayError> {
    match value.as_str() {
        Some("commentary") => Ok(MessagePhase::Commentary),
        Some("final_answer") => Ok(MessagePhase::FinalAnswer),
        _ => Err(GatewayError::invalid_mapping(format!(
            "rules[{rule_index}] phase must resolve to commentary or final_answer"
        ))),
    }
}

fn missing_field(rule_index: usize, field: MappedEventFieldV1) -> GatewayError {
    GatewayError::invalid_mapping(format!(
        "rules[{rule_index}] required field {field:?} was absent"
    ))
}

fn validate_pointer(name: &str, pointer: &str) -> Result<(), GatewayError> {
    if !pointer.starts_with('/')
        || pointer.len() > MAX_JSON_POINTER_BYTES
        || pointer.split('/').count().saturating_sub(1) > MAX_JSON_POINTER_DEPTH
        || !valid_pointer_escapes(pointer)
    {
        return Err(GatewayError::invalid_mapping(format!(
            "{name} must be a valid RFC 6901 JSON Pointer no deeper than \
             {MAX_JSON_POINTER_DEPTH} segments and at most {MAX_JSON_POINTER_BYTES} bytes"
        )));
    }
    Ok(())
}

fn valid_pointer_escapes(pointer: &str) -> bool {
    let bytes = pointer.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~' {
            if !matches!(bytes.get(index + 1), Some(b'0' | b'1')) {
                return false;
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    true
}

fn validate_scalar(name: &str, value: &Value) -> Result<(), GatewayError> {
    if !matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
        return Err(GatewayError::invalid_mapping(format!(
            "{name} must be a string, number, or boolean"
        )));
    }
    validate_literal(name, value)
}

fn validate_literal(name: &str, value: &Value) -> Result<(), GatewayError> {
    let size = serde_json::to_vec(value)
        .map_err(|_| GatewayError::invalid_mapping(format!("{name} is not serializable")))?
        .len();
    if size > MAX_LITERAL_BYTES {
        return Err(GatewayError::invalid_mapping(format!(
            "{name} exceeds the {MAX_LITERAL_BYTES} byte literal limit"
        )));
    }
    Ok(())
}

fn bounded_message(error: &impl std::fmt::Display) -> String {
    error.to_string().chars().take(2048).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source(pointer: &str) -> MappedValueSourceV1 {
        MappedValueSourceV1 {
            pointer: Some(pointer.to_owned()),
            value: None,
        }
    }

    #[test]
    fn declarative_profile_maps_unknown_wire_shapes_to_semantic_events() {
        let profile = EventMappingProfileV1 {
            version: EVENT_MAPPING_SCHEMA_VERSION,
            input: MappingInputV1::JsonData,
            discriminator: "/event/kind".to_owned(),
            rules: vec![
                EventMappingRuleV1 {
                    match_value: json!("begin"),
                    conditions: BTreeMap::new(),
                    emit: MappedEventTypeV1::RunStarted,
                    fields: BTreeMap::from([
                        (MappedEventFieldV1::RunId, source("/data/run")),
                        (MappedEventFieldV1::Model, source("/data/model")),
                    ]),
                },
                EventMappingRuleV1 {
                    match_value: json!("thinking"),
                    conditions: BTreeMap::from([("/data/visibility".to_owned(), json!("summary"))]),
                    emit: MappedEventTypeV1::ReasoningSummaryDelta,
                    fields: BTreeMap::from([(MappedEventFieldV1::Delta, source("/data/text"))]),
                },
            ],
        };
        let mut mapper = EventMapper::new(profile).unwrap();
        let started = mapper
            .map(&json!({
                "event": {"kind": "begin"},
                "data": {"run": "run-7", "model": "future-model"}
            }))
            .unwrap();
        assert_eq!(started[0].run_id, "run-7");
        assert_eq!(started[0].sequence, 0);

        let reasoning = mapper
            .map(&json!({
                "event": {"kind": "thinking"},
                "data": {"visibility": "summary", "text": "checking"}
            }))
            .unwrap();
        assert_eq!(reasoning[0].run_id, "run-7");
        assert_eq!(reasoning[0].sequence, 1);
        assert!(matches!(
            reasoning[0].event,
            OpsailEventKind::ReasoningSummaryDelta { ref delta, .. } if delta == "checking"
        ));
    }

    #[test]
    fn mapping_is_bounded_and_fails_closed_after_a_rule_matches() {
        let profile = EventMappingProfileV1 {
            version: EVENT_MAPPING_SCHEMA_VERSION,
            input: MappingInputV1::JsonData,
            discriminator: "/type".to_owned(),
            rules: vec![EventMappingRuleV1 {
                match_value: json!("delta"),
                conditions: BTreeMap::new(),
                emit: MappedEventTypeV1::AssistantTextDelta,
                fields: BTreeMap::from([(MappedEventFieldV1::Delta, source("/payload/text"))]),
            }],
        };
        let mut mapper = EventMapper::new(profile).unwrap();
        assert!(mapper.map(&json!({"type": "other"})).unwrap().is_empty());
        assert!(mapper.map(&json!({"type": "delta"})).is_err());
        assert!(
            EventMappingProfileV1::from_toml(
                r#"
version = 1
discriminator = "not-a-pointer"
rules = []
"#
            )
            .is_err()
        );
    }
}
