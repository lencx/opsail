//! Provider-neutral model events.

use serde::{Deserialize, Serialize};

pub const OPSAIL_EVENT_SCHEMA_VERSION: u16 = 1;

/// One ordered semantic event observed while projecting a provider response.
///
/// Events are not persisted by the gateway. Embedders may consume them to
/// build diagnostics or a separate UI without parsing provider-specific SSE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsailEventV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub sequence: u64,
    #[serde(flatten)]
    pub event: OpsailEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum OpsailEventKind {
    RunStarted {
        model: Option<String>,
    },
    ReasoningSummaryDelta {
        item_id: Option<String>,
        summary_index: Option<u64>,
        delta: String,
    },
    ReasoningSummaryCompleted {
        item_id: String,
        summary_index: u64,
        text: String,
    },
    AssistantTextDelta {
        item_id: Option<String>,
        phase: Option<MessagePhase>,
        delta: String,
    },
    ToolCallStarted {
        item_id: Option<String>,
        call_id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    ToolCallArgumentsDelta {
        item_id: Option<String>,
        call_id: Option<String>,
        delta: String,
    },
    ToolCallCompleted {
        item_id: Option<String>,
        call_id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    UsageUpdated {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        total_tokens: Option<u64>,
    },
    RunCompleted {
        response_id: Option<String>,
        model: Option<String>,
    },
    RunFailed {
        code: Option<String>,
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Debug, Default)]
pub(crate) struct EventSequencer {
    run_id: Option<String>,
    next_sequence: u64,
}

impl EventSequencer {
    pub fn set_run_id(&mut self, run_id: impl Into<String>) {
        self.run_id = Some(run_id.into());
    }

    pub fn push(&mut self, event: OpsailEventKind) -> OpsailEventV1 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        OpsailEventV1 {
            schema_version: OPSAIL_EVENT_SCHEMA_VERSION,
            run_id: self.run_id.clone().unwrap_or_else(|| "pending".to_owned()),
            sequence,
            event,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_versioned_and_monotonically_sequenced() {
        let mut sequencer = EventSequencer::default();
        sequencer.set_run_id("resp-1");
        let first = sequencer.push(OpsailEventKind::RunStarted {
            model: Some("model".to_owned()),
        });
        let second = sequencer.push(OpsailEventKind::RunCompleted {
            response_id: Some("resp-1".to_owned()),
            model: Some("model".to_owned()),
        });
        assert_eq!(first.schema_version, OPSAIL_EVENT_SCHEMA_VERSION);
        assert_eq!(first.run_id, "resp-1");
        assert_eq!(first.sequence, 0);
        assert_eq!(second.sequence, 1);

        let value = serde_json::to_value(first).unwrap();
        assert_eq!(value["type"], "run-started");
        assert_eq!(value["schemaVersion"], 1);
    }
}
