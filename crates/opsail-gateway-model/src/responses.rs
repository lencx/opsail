//! Validation and projection for native Responses streams.

use std::collections::{BTreeMap, BTreeSet};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::GatewayError;
use crate::event::{EventSequencer, MessagePhase, OpsailEventKind, OpsailEventV1};
use crate::sse::{SseDecoder, SseFrame, encode_json_event};

const MAX_REASONING_SUMMARY_BYTES: usize = 8 * 1024 * 1024;

/// How provider reasoning summaries are exposed to Codex.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningDisplay {
    /// Preserve the provider's reasoning items exactly.
    Strict,
    /// Convert only provider-supplied reasoning summaries into interim
    /// assistant messages with `phase: "commentary"`.
    #[default]
    Commentary,
}

#[derive(Debug, Default)]
pub struct ProjectionOutput {
    pub chunks: Vec<Bytes>,
    pub events: Vec<OpsailEventV1>,
}

/// Incrementally decodes and projects one Responses API SSE stream.
pub struct ResponsesProjector {
    mode: ReasoningDisplay,
    decoder: SseDecoder,
    sequencer: EventSequencer,
    active_item: Option<ActiveItem>,
    reasoning: Option<ReasoningProjection>,
    terminal_seen: bool,
}

#[derive(Debug, Clone)]
struct ActiveItem {
    id: Option<String>,
    phase: Option<MessagePhase>,
}

#[derive(Debug)]
struct ReasoningProjection {
    item_id: String,
    output_index: u64,
    added_template: Value,
    added_emitted: bool,
    display_text: String,
    parts: BTreeMap<u64, ReasoningPart>,
    started_parts: BTreeSet<u64>,
}

#[derive(Debug, Default)]
struct ReasoningPart {
    streamed: String,
    completed: Option<String>,
}

impl ResponsesProjector {
    pub fn new(mode: ReasoningDisplay) -> Self {
        Self {
            mode,
            decoder: SseDecoder::default(),
            sequencer: EventSequencer::default(),
            active_item: None,
            reasoning: None,
            terminal_seen: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<ProjectionOutput, GatewayError> {
        let frames = self.decoder.push(chunk)?;
        let mut output = ProjectionOutput::default();
        for frame in frames {
            let Some(value) = parse_json_frame(&frame)? else {
                output.chunks.push(frame.raw);
                continue;
            };
            if value
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    matches!(
                        kind,
                        "response.completed" | "response.failed" | "response.incomplete"
                    )
                })
            {
                self.terminal_seen = true;
            }
            self.observe(&value, &mut output.events);
            output.chunks.extend(self.project(frame, value)?);
        }
        Ok(output)
    }

    pub fn finish(&mut self) -> Result<(), GatewayError> {
        self.decoder.finish()?;
        if self.reasoning.is_some() {
            return Err(GatewayError::protocol(
                "the SSE stream ended before a reasoning output item completed",
            ));
        }
        if !self.terminal_seen {
            return Err(GatewayError::protocol(
                "the SSE stream ended before a terminal response event",
            ));
        }
        Ok(())
    }

    pub fn abort(
        &mut self,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<ProjectionOutput, GatewayError> {
        let code = code.into().chars().take(128).collect::<String>();
        let message = message.into().chars().take(2048).collect::<String>();
        self.decoder = SseDecoder::default();
        self.active_item = None;
        self.reasoning = None;
        self.terminal_seen = true;
        let event = self.sequencer.push(OpsailEventKind::RunFailed {
            code: Some(code.clone()),
            message: Some(message.clone()),
        });
        let response_id = event.run_id.clone();
        Ok(ProjectionOutput {
            chunks: vec![encode_json_event(
                "response.failed",
                json!({
                    "type": "response.failed",
                    "sequence_number": event.sequence,
                    "response": {
                        "id": response_id,
                        "object": "response",
                        "status": "failed",
                        "output": [],
                        "error": {
                            "code": code,
                            "message": message,
                        },
                    },
                }),
            )?],
            events: vec![event],
        })
    }

    fn observe(&mut self, value: &Value, events: &mut Vec<OpsailEventV1>) {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "response.created" => {
                let response = value.get("response");
                let response_id = response
                    .and_then(|item| item.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("pending")
                    .to_owned();
                self.sequencer.set_run_id(response_id);
                events.push(
                    self.sequencer.push(OpsailEventKind::RunStarted {
                        model: response
                            .and_then(|item| item.get("model"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    }),
                );
            }
            "response.output_item.added" => {
                let item = value.get("item");
                let item_kind = item
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let active = ActiveItem {
                    id: item
                        .and_then(|item| item.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    phase: parse_phase(
                        item.and_then(|item| item.get("phase"))
                            .and_then(Value::as_str),
                    ),
                };
                if matches!(
                    item_kind.as_str(),
                    "function_call" | "custom_tool_call" | "tool_search_call"
                ) {
                    events.push(
                        self.sequencer.push(OpsailEventKind::ToolCallStarted {
                            item_id: active.id.clone(),
                            call_id: item
                                .and_then(|item| item.get("call_id"))
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            name: item
                                .and_then(|item| item.get("name"))
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            arguments: item
                                .and_then(|item| {
                                    item.get("arguments").or_else(|| item.get("input"))
                                })
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        }),
                    );
                }
                self.active_item = Some(active);
            }
            "response.output_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    events.push(self.sequencer.push(OpsailEventKind::AssistantTextDelta {
                        item_id: self.active_item.as_ref().and_then(|item| item.id.clone()),
                        phase: self.active_item.as_ref().and_then(|item| item.phase),
                        delta: delta.to_owned(),
                    }));
                }
            }
            "response.reasoning_summary_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    events.push(self.sequencer.push(OpsailEventKind::ReasoningSummaryDelta {
                        item_id:
                            event_item_id(value).or_else(|| {
                                self.active_item.as_ref().and_then(|item| item.id.clone())
                            }),
                        summary_index: event_summary_index(value),
                        delta: delta.to_owned(),
                    }));
                }
            }
            "response.reasoning_summary_text.done" => {
                if let (Some(item_id), Some(text), Some(summary_index)) = (
                    event_item_id(value),
                    value.get("text").and_then(Value::as_str),
                    event_summary_index(value),
                ) {
                    events.push(
                        self.sequencer
                            .push(OpsailEventKind::ReasoningSummaryCompleted {
                                item_id,
                                summary_index,
                                text: text.to_owned(),
                            }),
                    );
                }
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    events.push(
                        self.sequencer
                            .push(OpsailEventKind::ToolCallArgumentsDelta {
                                item_id: event_item_id(value),
                                call_id: value
                                    .get("call_id")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                                delta: delta.to_owned(),
                            }),
                    );
                }
            }
            "response.output_item.done" => {
                let item = value.get("item");
                let item_kind = item
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if matches!(
                    item_kind,
                    "function_call" | "custom_tool_call" | "tool_search_call"
                ) {
                    events.push(
                        self.sequencer.push(OpsailEventKind::ToolCallCompleted {
                            item_id: item
                                .and_then(|item| item.get("id"))
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            call_id: item
                                .and_then(|item| item.get("call_id"))
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            name: item
                                .and_then(|item| item.get("name"))
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            arguments: item
                                .and_then(|item| {
                                    item.get("arguments").or_else(|| item.get("input"))
                                })
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        }),
                    );
                }
                self.active_item = None;
            }
            "response.completed" => {
                let response = value.get("response");
                let usage = response.and_then(|response| response.get("usage"));
                let input_tokens = usage
                    .and_then(|usage| usage.get("input_tokens"))
                    .and_then(Value::as_u64);
                let output_tokens = usage
                    .and_then(|usage| usage.get("output_tokens"))
                    .and_then(Value::as_u64);
                let total_tokens = usage
                    .and_then(|usage| usage.get("total_tokens"))
                    .and_then(Value::as_u64);
                if input_tokens.is_some() || output_tokens.is_some() || total_tokens.is_some() {
                    events.push(self.sequencer.push(OpsailEventKind::UsageUpdated {
                        input_tokens,
                        output_tokens,
                        total_tokens,
                    }));
                }
                events.push(
                    self.sequencer.push(OpsailEventKind::RunCompleted {
                        response_id: response
                            .and_then(|response| response.get("id"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        model: response
                            .and_then(|response| response.get("model"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    }),
                );
            }
            "response.failed" | "response.incomplete" => {
                let error = value
                    .get("response")
                    .and_then(|response| response.get("error"));
                events.push(
                    self.sequencer.push(OpsailEventKind::RunFailed {
                        code: error
                            .and_then(|error| error.get("code"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        message: error
                            .and_then(|error| error.get("message"))
                            .and_then(Value::as_str)
                            .map(|message| message.chars().take(2048).collect()),
                    }),
                );
            }
            _ => {}
        }
    }

    fn project(&mut self, frame: SseFrame, value: Value) -> Result<Vec<Bytes>, GatewayError> {
        if self.mode == ReasoningDisplay::Strict {
            return Ok(vec![frame.raw]);
        }
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "response.output_item.added"
                if value
                    .get("item")
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    == Some("reasoning") =>
            {
                if self.reasoning.is_some() {
                    return Err(GatewayError::protocol(
                        "a reasoning item started before the previous one completed",
                    ));
                }
                let item_id = value
                    .get("item")
                    .and_then(|item| item.get("id"))
                    .and_then(Value::as_str)
                    .filter(|item_id| !item_id.is_empty())
                    .ok_or_else(|| {
                        GatewayError::protocol("a reasoning output item omitted its id")
                    })?
                    .to_owned();
                let output_index = value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        GatewayError::protocol(
                            "a reasoning output item omitted its numeric output_index",
                        )
                    })?;
                self.reasoning = Some(ReasoningProjection {
                    item_id,
                    output_index,
                    added_template: value,
                    added_emitted: false,
                    display_text: String::new(),
                    parts: BTreeMap::new(),
                    started_parts: BTreeSet::new(),
                });
                Ok(Vec::new())
            }
            "response.reasoning_summary_part.added" if self.reasoning.is_some() => Ok(Vec::new()),
            "response.reasoning_summary_text.delta" if self.reasoning.is_some() => {
                let summary_index = event_summary_index(&value).ok_or_else(|| {
                    GatewayError::protocol("a reasoning summary delta omitted summary_index")
                })?;
                let delta = value.get("delta").and_then(Value::as_str).ok_or_else(|| {
                    GatewayError::protocol("a reasoning summary delta omitted text")
                })?;
                self.emit_reasoning_text(summary_index, delta)
            }
            "response.reasoning_summary_text.done" if self.reasoning.is_some() => {
                let summary_index = event_summary_index(&value).ok_or_else(|| {
                    GatewayError::protocol("a completed reasoning summary omitted summary_index")
                })?;
                let text = value.get("text").and_then(Value::as_str).ok_or_else(|| {
                    GatewayError::protocol("a completed reasoning summary omitted text")
                })?;
                self.complete_reasoning_part(summary_index, text)
            }
            "response.reasoning_text.delta" if self.reasoning.is_some() => Ok(Vec::new()),
            kind if self.reasoning.is_some() && kind.starts_with("response.content_part.") => {
                Ok(Vec::new())
            }
            "response.output_item.done"
                if value
                    .get("item")
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    == Some("reasoning") =>
            {
                self.finish_reasoning_item(value)
            }
            "response.completed" if self.reasoning.is_some() => Err(GatewayError::protocol(
                "the response completed before its reasoning item completed",
            )),
            _ => Ok(vec![frame.raw]),
        }
    }

    fn emit_reasoning_text(
        &mut self,
        summary_index: u64,
        text: &str,
    ) -> Result<Vec<Bytes>, GatewayError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let state = self
            .reasoning
            .as_ref()
            .expect("reasoning state was checked by the caller");
        let separator_bytes = usize::from(
            !state.started_parts.contains(&summary_index) && !state.display_text.is_empty(),
        ) * 2;
        ensure_reasoning_summary_limit(
            state.display_text.len(),
            separator_bytes.saturating_add(text.len()),
        )?;
        let mut chunks = self.ensure_commentary_added()?;
        let state = self
            .reasoning
            .as_mut()
            .expect("reasoning state was checked by the caller");
        if state.started_parts.insert(summary_index) && !state.display_text.is_empty() {
            chunks.push(output_text_delta(state, "\n\n")?);
            state.display_text.push_str("\n\n");
        }
        state
            .parts
            .entry(summary_index)
            .or_default()
            .streamed
            .push_str(text);
        state.display_text.push_str(text);
        chunks.push(output_text_delta(state, text)?);
        Ok(chunks)
    }

    fn complete_reasoning_part(
        &mut self,
        summary_index: u64,
        text: &str,
    ) -> Result<Vec<Bytes>, GatewayError> {
        ensure_reasoning_summary_limit(0, text.len())?;
        let existing = self
            .reasoning
            .as_ref()
            .and_then(|state| state.parts.get(&summary_index))
            .map(|part| part.streamed.clone())
            .unwrap_or_default();
        let mut chunks = Vec::new();
        if existing.is_empty() {
            chunks.extend(self.emit_reasoning_text(summary_index, text)?);
        } else if let Some(suffix) = text.strip_prefix(&existing)
            && !suffix.is_empty()
        {
            chunks.extend(self.emit_reasoning_text(summary_index, suffix)?);
        }
        if let Some(state) = self.reasoning.as_mut() {
            state.parts.entry(summary_index).or_default().completed = Some(text.to_owned());
        }
        Ok(chunks)
    }

    fn ensure_commentary_added(&mut self) -> Result<Vec<Bytes>, GatewayError> {
        let state = self
            .reasoning
            .as_mut()
            .expect("reasoning state was checked by the caller");
        if state.added_emitted {
            return Ok(Vec::new());
        }
        state.added_emitted = true;
        let mut added = state.added_template.clone();
        added["item"] = commentary_item(&state.item_id, "in_progress", Vec::new());
        Ok(vec![
            encode_json_event("response.output_item.added", added)?,
            content_part_event(
                "response.content_part.added",
                state,
                json!({
                    "type": "output_text",
                    "text": "",
                    "annotations": [],
                }),
            )?,
        ])
    }

    fn finish_reasoning_item(&mut self, value: Value) -> Result<Vec<Bytes>, GatewayError> {
        if self.reasoning.is_none() {
            let item_id = value
                .get("item")
                .and_then(|item| item.get("id"))
                .and_then(Value::as_str)
                .filter(|item_id| !item_id.is_empty())
                .ok_or_else(|| GatewayError::protocol("a reasoning output item omitted its id"))?
                .to_owned();
            let output_index = value
                .get("output_index")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    GatewayError::protocol(
                        "a reasoning output item omitted its numeric output_index",
                    )
                })?;
            self.reasoning = Some(ReasoningProjection {
                item_id,
                output_index,
                added_template: json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                }),
                added_emitted: false,
                display_text: String::new(),
                parts: BTreeMap::new(),
                started_parts: BTreeSet::new(),
            });
        }

        let summaries = reasoning_summaries(&value);
        let mut chunks = Vec::new();
        for (index, text) in summaries.iter().enumerate() {
            chunks.extend(self.complete_reasoning_part(index as u64, text)?);
        }
        let state = self
            .reasoning
            .as_ref()
            .expect("reasoning state was initialized above");
        let completed_text = if summaries.is_empty() {
            state
                .parts
                .values()
                .map(|part| part.completed.as_deref().unwrap_or(part.streamed.as_str()))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        } else {
            summaries.join("\n\n")
        };
        ensure_reasoning_summary_limit(0, completed_text.len())?;
        if completed_text.trim().is_empty() {
            self.reasoning = None;
            return Ok(Vec::new());
        }
        chunks.extend(self.ensure_commentary_added()?);
        let state = self
            .reasoning
            .take()
            .expect("reasoning state was checked above");
        let item_id = state.item_id;
        let output_index = state.output_index;
        chunks.push(encode_json_event(
            "response.output_text.done",
            json!({
                "type": "response.output_text.done",
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "text": completed_text,
            }),
        )?);
        chunks.push(encode_json_event(
            "response.content_part.done",
            json!({
                "type": "response.content_part.done",
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": completed_text,
                    "annotations": [],
                },
            }),
        )?);

        let mut done = value;
        done["item"] = commentary_item(
            &item_id,
            "completed",
            vec![json!({
                "type": "output_text",
                "text": completed_text,
                "annotations": [],
            })],
        );
        chunks.push(encode_json_event("response.output_item.done", done)?);
        Ok(chunks)
    }
}

fn ensure_reasoning_summary_limit(current: usize, additional: usize) -> Result<(), GatewayError> {
    if current
        .checked_add(additional)
        .is_none_or(|total| total > MAX_REASONING_SUMMARY_BYTES)
    {
        return Err(GatewayError::protocol(
            "projected reasoning summary exceeded the 8 MiB limit",
        ));
    }
    Ok(())
}

fn output_text_delta(state: &ReasoningProjection, delta: &str) -> Result<Bytes, GatewayError> {
    encode_json_event(
        "response.output_text.delta",
        json!({
            "type": "response.output_text.delta",
            "item_id": state.item_id.clone(),
            "output_index": state.output_index,
            "content_index": 0,
            "delta": delta,
        }),
    )
}

fn content_part_event(
    kind: &str,
    state: &ReasoningProjection,
    part: Value,
) -> Result<Bytes, GatewayError> {
    encode_json_event(
        kind,
        json!({
            "type": kind,
            "item_id": state.item_id,
            "output_index": state.output_index,
            "content_index": 0,
            "part": part,
        }),
    )
}

fn commentary_item(item_id: &str, status: &str, content: Vec<Value>) -> Value {
    json!({
        "id": item_id,
        "type": "message",
        "status": status,
        "role": "assistant",
        "phase": "commentary",
        "content": content,
    })
}

fn reasoning_summaries(value: &Value) -> Vec<String> {
    value
        .get("item")
        .and_then(|item| item.get("summary"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|summary| summary.get("type").and_then(Value::as_str) == Some("summary_text"))
        .filter_map(|summary| summary.get("text").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn parse_json_frame(frame: &SseFrame) -> Result<Option<Value>, GatewayError> {
    if frame.data.is_empty() || frame.data == "[DONE]" {
        return Ok(None);
    }
    let value = serde_json::from_str::<Value>(&frame.data)
        .map_err(|_| GatewayError::protocol("an SSE data field was not valid JSON"))?;
    if !value.is_object() {
        return Err(GatewayError::protocol(
            "a Responses SSE event must be a JSON object",
        ));
    }
    if let (Some(sse_event), Some(json_event)) = (
        frame.event.as_deref(),
        value.get("type").and_then(Value::as_str),
    ) && sse_event != json_event
    {
        return Err(GatewayError::protocol(
            "the SSE event name did not match the JSON event type",
        ));
    }
    Ok(Some(value))
}

fn event_item_id(value: &Value) -> Option<String> {
    value
        .get("item_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn event_summary_index(value: &Value) -> Option<u64> {
    value.get("summary_index").and_then(Value::as_u64)
}

fn parse_phase(value: Option<&str>) -> Option<MessagePhase> {
    match value {
        Some("commentary") => Some(MessagePhase::Commentary),
        Some("final_answer") => Some(MessagePhase::FinalAnswer),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn event(kind: &str, body: Value) -> String {
        let mut object = match body {
            Value::Object(object) => object,
            _ => Map::new(),
        };
        object.insert("type".to_owned(), Value::String(kind.to_owned()));
        format!("event: {kind}\ndata: {}\n\n", Value::Object(object))
    }

    fn reasoning_stream(summary: &str) -> String {
        [
            event(
                "response.created",
                json!({"response": {"id": "resp-1", "model": "third-party"}}),
            ),
            event(
                "response.output_item.added",
                json!({
                    "output_index": 0,
                    "item": {
                        "id": "reasoning-1",
                        "type": "reasoning",
                        "summary": [],
                        "content": null,
                        "encrypted_content": null
                    }
                }),
            ),
            event(
                "response.reasoning_summary_part.added",
                json!({"item_id": "reasoning-1", "output_index": 0, "summary_index": 0}),
            ),
            event(
                "response.reasoning_summary_text.delta",
                json!({
                    "item_id": "reasoning-1",
                    "output_index": 0,
                    "summary_index": 0,
                    "delta": summary
                }),
            ),
            event(
                "response.reasoning_summary_text.done",
                json!({
                    "item_id": "reasoning-1",
                    "output_index": 0,
                    "summary_index": 0,
                    "text": summary
                }),
            ),
            event(
                "response.output_item.done",
                json!({
                    "output_index": 0,
                    "item": {
                        "id": "reasoning-1",
                        "type": "reasoning",
                        "summary": [{"type": "summary_text", "text": summary}],
                        "content": null,
                        "encrypted_content": null
                    }
                }),
            ),
            event(
                "response.completed",
                json!({"response": {"id": "resp-1", "model": "third-party"}}),
            ),
        ]
        .join("")
    }

    #[test]
    fn reasoning_summary_projection_has_a_cumulative_byte_limit() {
        assert!(ensure_reasoning_summary_limit(MAX_REASONING_SUMMARY_BYTES, 0).is_ok());
        assert!(ensure_reasoning_summary_limit(MAX_REASONING_SUMMARY_BYTES, 1).is_err());
        assert!(ensure_reasoning_summary_limit(usize::MAX, 1).is_err());
    }

    #[test]
    fn strict_projection_preserves_sse_bytes_exactly() {
        let source = reasoning_stream("Inspecting the code.");
        let mut projector = ResponsesProjector::new(ReasoningDisplay::Strict);
        let mut output = Vec::new();
        for chunk in source.as_bytes().chunks(7) {
            let projected = projector.push(chunk).unwrap();
            for bytes in projected.chunks {
                output.extend_from_slice(&bytes);
            }
        }
        projector.finish().unwrap();
        assert_eq!(output, source.as_bytes());
    }

    #[test]
    fn commentary_projection_converts_summary_without_exposing_raw_reasoning() {
        let source = reasoning_stream("Inspecting the code.");
        let mut projector = ResponsesProjector::new(ReasoningDisplay::Commentary);
        let projected = projector.push(source.as_bytes()).unwrap();
        projector.finish().unwrap();
        let output = projected
            .chunks
            .iter()
            .flat_map(|chunk| chunk.iter().copied())
            .collect::<Vec<_>>();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(r#""type":"message""#));
        assert!(output.contains(r#""phase":"commentary""#));
        assert!(output.contains(r#""status":"in_progress""#));
        assert!(output.contains("response.content_part.added"));
        assert!(output.contains("response.content_part.done"));
        assert!(output.contains(r#""status":"completed""#));
        assert!(output.contains("Inspecting the code."));
        assert!(!output.contains("response.reasoning_summary_text"));
        assert!(!output.contains(r#""type":"reasoning""#));

        assert!(projected.events.iter().any(|event| matches!(
            event.event,
            OpsailEventKind::ReasoningSummaryCompleted { .. }
        )));
    }

    #[test]
    fn commentary_projection_suppresses_empty_reasoning_items() {
        let source = [
            event(
                "response.output_item.added",
                json!({
                    "output_index": 0,
                    "item": {"id": "reasoning-1", "type": "reasoning", "summary": []}
                }),
            ),
            event(
                "response.output_item.done",
                json!({
                    "output_index": 0,
                    "item": {"id": "reasoning-1", "type": "reasoning", "summary": []}
                }),
            ),
            event(
                "response.completed",
                json!({"response": {"id": "resp-1", "model": "third-party"}}),
            ),
        ]
        .join("");
        let mut projector = ResponsesProjector::new(ReasoningDisplay::Commentary);
        let projected = projector.push(source.as_bytes()).unwrap();
        projector.finish().unwrap();
        let output = String::from_utf8(
            projected
                .chunks
                .iter()
                .flat_map(|chunk| chunk.iter().copied())
                .collect(),
        )
        .unwrap();
        assert!(!output.contains(r#""type":"message""#));
        assert!(!output.contains(r#""type":"reasoning""#));
        assert!(output.contains("response.completed"));
    }

    #[test]
    fn commentary_projection_keeps_summary_sections_separate() {
        let source = [
            event(
                "response.output_item.added",
                json!({
                    "output_index": 0,
                    "item": {"id": "reasoning-1", "type": "reasoning", "summary": []}
                }),
            ),
            event(
                "response.reasoning_summary_text.done",
                json!({
                    "item_id": "reasoning-1",
                    "summary_index": 0,
                    "text": "First"
                }),
            ),
            event(
                "response.reasoning_summary_text.done",
                json!({
                    "item_id": "reasoning-1",
                    "summary_index": 1,
                    "text": "Second"
                }),
            ),
            event(
                "response.output_item.done",
                json!({
                    "output_index": 0,
                    "item": {
                        "id": "reasoning-1",
                        "type": "reasoning",
                        "summary": [
                            {"type": "summary_text", "text": "First"},
                            {"type": "summary_text", "text": "Second"}
                        ]
                    }
                }),
            ),
            event(
                "response.completed",
                json!({"response": {"id": "resp-1", "model": "third-party"}}),
            ),
        ]
        .join("");
        let mut projector = ResponsesProjector::new(ReasoningDisplay::Commentary);
        let projected = projector.push(source.as_bytes()).unwrap();
        projector.finish().unwrap();
        let output = String::from_utf8(
            projected
                .chunks
                .iter()
                .flat_map(|chunk| chunk.iter().copied())
                .collect(),
        )
        .unwrap();
        assert!(output.contains(r#"First\n\nSecond"#));
    }
}
