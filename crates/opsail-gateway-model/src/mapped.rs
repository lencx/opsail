//! Projection from declaratively mapped events to Responses SSE.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{Value, json};

use crate::GatewayError;
use crate::event::{MessagePhase, OpsailEventKind, OpsailEventV1};
use crate::mapping::{EventMapper, EventMappingProfileV1, MappingInputV1};
use crate::responses::{ProjectionOutput, ReasoningDisplay};
use crate::sse::{SseDecoder, encode_json_event};

const MAX_PROJECTED_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

/// Converts a configured JSON-SSE dialect into canonical Opsail events and
/// then into the Responses event shapes consumed by Codex.
pub struct MappedSseProjector {
    decoder: SseDecoder,
    mapper: EventMapper,
    input: MappingInputV1,
    target: CanonicalResponsesProjector,
}

/// Stateful Codex Responses projection for provider-neutral Opsail events.
///
/// This is intentionally separate from the declarative mapper: mappings
/// describe field locations, while this projector owns ordered lifecycle
/// repair, generated identifiers, bounded aggregation, and target syntax.
pub struct CanonicalResponsesProjector {
    reasoning_display: ReasoningDisplay,
    response_id: Option<String>,
    model: Option<String>,
    next_sequence: u64,
    next_output_index: u64,
    next_generated_id: u64,
    started: bool,
    terminal: bool,
    active: Option<ActiveOutput>,
    output: Vec<Value>,
    usage: UsageSnapshot,
    buffered_output_bytes: usize,
}

enum ActiveOutput {
    Message {
        id: String,
        output_index: u64,
        phase: MessagePhase,
        source: MessageSource,
        text: String,
    },
    Reasoning {
        id: String,
        source_id: Option<String>,
        output_index: u64,
        parts: BTreeMap<u64, ReasoningPart>,
    },
    Tool {
        id: String,
        output_index: u64,
        call_id: String,
        name: String,
        arguments: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MessageSource {
    Assistant,
    Reasoning {
        source_id: Option<String>,
        summary_index: u64,
    },
}

#[derive(Default)]
struct ReasoningPart {
    text: String,
    done: bool,
}

#[derive(Default)]
struct UsageSnapshot {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl MappedSseProjector {
    pub fn new(
        profile: EventMappingProfileV1,
        reasoning_display: ReasoningDisplay,
    ) -> Result<Self, GatewayError> {
        let input = profile.input;
        Ok(Self {
            decoder: SseDecoder::default(),
            mapper: EventMapper::new(profile)?,
            input,
            target: CanonicalResponsesProjector::new(reasoning_display),
        })
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<ProjectionOutput, GatewayError> {
        let frames = self.decoder.push(chunk)?;
        let mut output = ProjectionOutput::default();
        for frame in frames {
            if frame.data.trim().is_empty() || frame.data.trim() == "[DONE]" {
                continue;
            }
            let value: Value = serde_json::from_str(&frame.data).map_err(|_| {
                GatewayError::protocol(
                    "a configured event-mapping stream contained non-JSON SSE data",
                )
            })?;
            let input = match self.input {
                MappingInputV1::JsonData => value,
                MappingInputV1::SseEnvelope => json!({
                    "event": frame.event,
                    "data": value,
                }),
            };
            let events = self.mapper.map(&input)?;
            for event in &events {
                output.chunks.extend(self.target.push(event)?);
            }
            output.events.extend(events);
        }
        Ok(output)
    }

    pub fn finish(&mut self) -> Result<(), GatewayError> {
        self.decoder.finish()?;
        self.target.finish()
    }

    pub fn abort(
        &mut self,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<ProjectionOutput, GatewayError> {
        self.decoder = SseDecoder::default();
        let event = self.mapper.gateway_failure(
            code.into().chars().take(128).collect(),
            message.into().chars().take(2048).collect(),
        );
        let chunks = self.target.push(&event)?;
        Ok(ProjectionOutput {
            chunks,
            events: vec![event],
        })
    }
}

impl CanonicalResponsesProjector {
    pub fn new(reasoning_display: ReasoningDisplay) -> Self {
        Self {
            reasoning_display,
            response_id: None,
            model: None,
            next_sequence: 0,
            next_output_index: 0,
            next_generated_id: 0,
            started: false,
            terminal: false,
            active: None,
            output: Vec::new(),
            usage: UsageSnapshot::default(),
            buffered_output_bytes: 0,
        }
    }

    pub fn push(&mut self, event: &OpsailEventV1) -> Result<Vec<Bytes>, GatewayError> {
        if self.terminal {
            return Err(GatewayError::protocol(
                "a mapped event arrived after the terminal response event",
            ));
        }

        match &event.event {
            OpsailEventKind::RunStarted { model } => self.start(event, model.clone(), None),
            OpsailEventKind::ReasoningSummaryDelta {
                item_id,
                summary_index,
                delta,
            } => {
                let mut chunks = self.ensure_started(event, None)?;
                chunks.extend(self.reasoning_delta(
                    item_id.as_deref(),
                    summary_index.unwrap_or(0),
                    delta,
                )?);
                Ok(chunks)
            }
            OpsailEventKind::ReasoningSummaryCompleted {
                item_id,
                summary_index,
                text,
            } => {
                let mut chunks = self.ensure_started(event, None)?;
                chunks.extend(self.reasoning_done(item_id, *summary_index, text)?);
                Ok(chunks)
            }
            OpsailEventKind::AssistantTextDelta {
                item_id,
                phase,
                delta,
            } => {
                let mut chunks = self.ensure_started(event, None)?;
                chunks.extend(self.assistant_delta(
                    item_id.as_deref(),
                    phase.unwrap_or(MessagePhase::FinalAnswer),
                    delta,
                )?);
                Ok(chunks)
            }
            OpsailEventKind::ToolCallStarted {
                item_id,
                call_id,
                name,
                arguments,
            } => {
                let mut chunks = self.ensure_started(event, None)?;
                chunks.extend(self.start_tool(
                    item_id.as_deref(),
                    call_id.as_deref(),
                    name.as_deref(),
                    arguments.as_deref(),
                )?);
                Ok(chunks)
            }
            OpsailEventKind::ToolCallArgumentsDelta {
                item_id,
                call_id,
                delta,
            } => {
                let mut chunks = self.ensure_started(event, None)?;
                chunks.extend(self.tool_arguments_delta(
                    item_id.as_deref(),
                    call_id.as_deref(),
                    delta,
                )?);
                Ok(chunks)
            }
            OpsailEventKind::ToolCallCompleted {
                item_id,
                call_id,
                name,
                arguments,
            } => {
                let mut chunks = self.ensure_started(event, None)?;
                chunks.extend(self.complete_tool(
                    item_id.as_deref(),
                    call_id.as_deref(),
                    name.as_deref(),
                    arguments.as_deref(),
                )?);
                Ok(chunks)
            }
            OpsailEventKind::UsageUpdated {
                input_tokens,
                output_tokens,
                total_tokens,
            } => {
                self.usage.input_tokens = input_tokens.or(self.usage.input_tokens);
                self.usage.output_tokens = output_tokens.or(self.usage.output_tokens);
                self.usage.total_tokens = total_tokens.or(self.usage.total_tokens);
                Ok(Vec::new())
            }
            OpsailEventKind::RunCompleted { response_id, model } => {
                let preferred_id = response_id.as_deref();
                let mut chunks = self.ensure_started(event, preferred_id)?;
                chunks.extend(self.close_active()?);
                if let Some(model) = model {
                    self.model = Some(model.clone());
                }
                let response_id = self
                    .response_id
                    .clone()
                    .expect("ensure_started establishes a response id");
                let mut response = json!({
                    "id": response_id,
                    "object": "response",
                    "status": "completed",
                    "model": self.model,
                    "output": self.output,
                    "end_turn": true,
                });
                if let Some(usage) = self.usage_json() {
                    response["usage"] = usage;
                }
                chunks.push(self.emit(
                    "response.completed",
                    json!({
                        "response": response,
                    }),
                )?);
                self.terminal = true;
                Ok(chunks)
            }
            OpsailEventKind::RunFailed { code, message } => {
                let mut chunks = self.ensure_started(event, None)?;
                chunks.extend(self.close_active()?);
                let response_id = self
                    .response_id
                    .clone()
                    .expect("ensure_started establishes a response id");
                chunks.push(self.emit(
                    "response.failed",
                    json!({
                        "response": {
                            "id": response_id,
                            "object": "response",
                            "status": "failed",
                            "output": self.output,
                            "error": {
                                "type": "opsail_gateway_error",
                                "code": code,
                                "message": message,
                            },
                        },
                    }),
                )?);
                self.terminal = true;
                Ok(chunks)
            }
        }
    }

    pub fn finish(&mut self) -> Result<(), GatewayError> {
        if !self.terminal {
            return Err(GatewayError::protocol(
                "the mapped stream ended before a run-completed or run-failed event",
            ));
        }
        Ok(())
    }

    fn start(
        &mut self,
        event: &OpsailEventV1,
        model: Option<String>,
        preferred_id: Option<&str>,
    ) -> Result<Vec<Bytes>, GatewayError> {
        if self.started {
            return Err(GatewayError::protocol(
                "the mapped stream emitted more than one run-started event",
            ));
        }
        self.started = true;
        self.response_id = Some(self.response_id(event, preferred_id));
        self.model = model;
        let response_id = self
            .response_id
            .clone()
            .expect("start establishes a response id");
        Ok(vec![self.emit(
            "response.created",
            json!({
                "response": {
                    "id": response_id,
                    "object": "response",
                    "status": "in_progress",
                    "model": self.model,
                    "output": [],
                },
            }),
        )?])
    }

    fn ensure_started(
        &mut self,
        event: &OpsailEventV1,
        preferred_id: Option<&str>,
    ) -> Result<Vec<Bytes>, GatewayError> {
        if self.started {
            if let Some(preferred_id) = preferred_id
                && !preferred_id.is_empty()
                && self.response_id.as_deref() != Some(preferred_id)
            {
                return Err(GatewayError::protocol(
                    "the mapped stream changed response identifiers during one run",
                ));
            }
            return Ok(Vec::new());
        }
        self.start(event, None, preferred_id)
    }

    fn response_id(&mut self, event: &OpsailEventV1, preferred_id: Option<&str>) -> String {
        preferred_id
            .filter(|value| !value.is_empty())
            .or_else(|| (event.run_id != "pending").then_some(event.run_id.as_str()))
            .map(str::to_owned)
            .unwrap_or_else(|| self.generated_id("resp"))
    }

    fn assistant_delta(
        &mut self,
        item_id: Option<&str>,
        phase: MessagePhase,
        delta: &str,
    ) -> Result<Vec<Bytes>, GatewayError> {
        if delta.is_empty() {
            return Ok(Vec::new());
        }
        let matches_active = matches!(
            &self.active,
            Some(ActiveOutput::Message {
                id,
                phase: active_phase,
                source: MessageSource::Assistant,
                ..
            }) if *active_phase == phase && item_id.is_none_or(|item_id| item_id == id)
        );
        let mut chunks = Vec::new();
        if !matches_active {
            chunks.extend(self.close_active()?);
            chunks.extend(self.open_message(item_id, phase, MessageSource::Assistant)?);
        }
        let (id, output_index) = self.append_active_message(delta)?;
        chunks.push(self.emit(
            "response.output_text.delta",
            json!({
                "item_id": id,
                "output_index": output_index,
                "content_index": 0,
                "delta": delta,
            }),
        )?);
        Ok(chunks)
    }

    fn reasoning_delta(
        &mut self,
        item_id: Option<&str>,
        summary_index: u64,
        delta: &str,
    ) -> Result<Vec<Bytes>, GatewayError> {
        if delta.is_empty() {
            return Ok(Vec::new());
        }
        match self.reasoning_display {
            ReasoningDisplay::Strict => {
                let mut chunks = self.ensure_reasoning(item_id)?;
                chunks.extend(self.ensure_reasoning_part(summary_index)?);
                let (id, output_index) = self.append_reasoning(summary_index, delta)?;
                chunks.push(self.emit(
                    "response.reasoning_summary_text.delta",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "summary_index": summary_index,
                        "delta": delta,
                    }),
                )?);
                Ok(chunks)
            }
            ReasoningDisplay::Commentary => {
                let source = MessageSource::Reasoning {
                    source_id: item_id.map(str::to_owned),
                    summary_index,
                };
                let matches_active = matches!(
                    &self.active,
                    Some(ActiveOutput::Message {
                        source: MessageSource::Reasoning {
                            source_id: active_source_id,
                            summary_index: active_summary_index,
                        },
                        ..
                    }) if *active_summary_index == summary_index
                        && sources_compatible(active_source_id, item_id)
                );
                let mut chunks = Vec::new();
                if !matches_active {
                    chunks.extend(self.close_active()?);
                    chunks.extend(self.open_message(None, MessagePhase::Commentary, source)?);
                } else {
                    self.bind_active_reasoning_source(item_id);
                }
                let (id, output_index) = self.append_active_message(delta)?;
                chunks.push(self.emit(
                    "response.output_text.delta",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "content_index": 0,
                        "delta": delta,
                    }),
                )?);
                Ok(chunks)
            }
        }
    }

    fn reasoning_done(
        &mut self,
        item_id: &str,
        summary_index: u64,
        text: &str,
    ) -> Result<Vec<Bytes>, GatewayError> {
        if text.is_empty() && self.active.is_none() {
            return Ok(Vec::new());
        }
        match self.reasoning_display {
            ReasoningDisplay::Strict => {
                let mut chunks = self.ensure_reasoning(Some(item_id))?;
                chunks.extend(self.ensure_reasoning_part(summary_index)?);
                let existing = self.reasoning_part_text(summary_index)?;
                if let Some(suffix) = text.strip_prefix(&existing) {
                    if !suffix.is_empty() {
                        let (id, output_index) = self.append_reasoning(summary_index, suffix)?;
                        chunks.push(self.emit(
                            "response.reasoning_summary_text.delta",
                            json!({
                                "item_id": id,
                                "output_index": output_index,
                                "summary_index": summary_index,
                                "delta": suffix,
                            }),
                        )?);
                    }
                } else {
                    return Err(GatewayError::protocol(
                        "a mapped reasoning completion disagreed with its streamed prefix",
                    ));
                }
                let (id, output_index, already_done) = self.mark_reasoning_done(summary_index)?;
                if !already_done {
                    chunks.push(self.emit(
                        "response.reasoning_summary_text.done",
                        json!({
                            "item_id": id,
                            "output_index": output_index,
                            "summary_index": summary_index,
                            "text": text,
                        }),
                    )?);
                }
                Ok(chunks)
            }
            ReasoningDisplay::Commentary => {
                let source = MessageSource::Reasoning {
                    source_id: Some(item_id.to_owned()),
                    summary_index,
                };
                let matches_active = matches!(
                    &self.active,
                    Some(ActiveOutput::Message {
                        source: MessageSource::Reasoning {
                            source_id: active_source_id,
                            summary_index: active_summary_index,
                        },
                        ..
                    }) if *active_summary_index == summary_index
                        && sources_compatible(active_source_id, Some(item_id))
                );
                let mut chunks = Vec::new();
                if !matches_active {
                    chunks.extend(self.close_active()?);
                    if text.is_empty() {
                        return Ok(chunks);
                    }
                    chunks.extend(self.open_message(None, MessagePhase::Commentary, source)?);
                } else {
                    self.bind_active_reasoning_source(Some(item_id));
                }
                let existing = self.active_message_text()?;
                if let Some(suffix) = text.strip_prefix(existing) {
                    if !suffix.is_empty() {
                        let (id, output_index) = self.append_active_message(suffix)?;
                        chunks.push(self.emit(
                            "response.output_text.delta",
                            json!({
                                "item_id": id,
                                "output_index": output_index,
                                "content_index": 0,
                                "delta": suffix,
                            }),
                        )?);
                    }
                } else {
                    return Err(GatewayError::protocol(
                        "a mapped reasoning completion disagreed with its streamed prefix",
                    ));
                }
                chunks.extend(self.close_active()?);
                Ok(chunks)
            }
        }
    }

    fn open_message(
        &mut self,
        item_id: Option<&str>,
        phase: MessagePhase,
        source: MessageSource,
    ) -> Result<Vec<Bytes>, GatewayError> {
        let id = item_id
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| self.generated_id("msg"));
        let output_index = self.take_output_index();
        let item = message_item(&id, phase, "", "in_progress");
        self.active = Some(ActiveOutput::Message {
            id: id.clone(),
            output_index,
            phase,
            source,
            text: String::new(),
        });
        Ok(vec![
            self.emit(
                "response.output_item.added",
                json!({
                    "output_index": output_index,
                    "item": item,
                }),
            )?,
            self.emit(
                "response.content_part.added",
                json!({
                    "item_id": id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": "",
                        "annotations": [],
                    },
                }),
            )?,
        ])
    }

    fn ensure_reasoning(&mut self, item_id: Option<&str>) -> Result<Vec<Bytes>, GatewayError> {
        let matches_active = matches!(
            &self.active,
            Some(ActiveOutput::Reasoning { source_id, .. })
                if sources_compatible(source_id, item_id)
        );
        if matches_active {
            if let Some(item_id) = item_id
                && let Some(ActiveOutput::Reasoning { source_id, .. }) = self.active.as_mut()
                && source_id.is_none()
            {
                *source_id = Some(item_id.to_owned());
            }
            return Ok(Vec::new());
        }
        let mut chunks = self.close_active()?;
        let id = item_id
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| self.generated_id("rs"));
        let output_index = self.take_output_index();
        self.active = Some(ActiveOutput::Reasoning {
            id: id.clone(),
            source_id: item_id.map(str::to_owned),
            output_index,
            parts: BTreeMap::new(),
        });
        chunks.push(self.emit(
            "response.output_item.added",
            json!({
                "output_index": output_index,
                "item": reasoning_item(&id, &BTreeMap::new(), "in_progress"),
            }),
        )?);
        Ok(chunks)
    }

    fn ensure_reasoning_part(&mut self, summary_index: u64) -> Result<Vec<Bytes>, GatewayError> {
        let ActiveOutput::Reasoning {
            id,
            output_index,
            parts,
            ..
        } = self
            .active
            .as_mut()
            .ok_or_else(|| GatewayError::protocol("reasoning state was not initialized"))?
        else {
            return Err(GatewayError::protocol(
                "reasoning state was not initialized",
            ));
        };
        if parts.contains_key(&summary_index) {
            return Ok(Vec::new());
        }
        parts.insert(summary_index, ReasoningPart::default());
        let id = id.clone();
        let output_index = *output_index;
        Ok(vec![self.emit(
            "response.reasoning_summary_part.added",
            json!({
                "item_id": id,
                "output_index": output_index,
                "summary_index": summary_index,
                "part": {
                    "type": "summary_text",
                    "text": "",
                },
            }),
        )?])
    }

    fn start_tool(
        &mut self,
        item_id: Option<&str>,
        call_id: Option<&str>,
        name: Option<&str>,
        arguments: Option<&str>,
    ) -> Result<Vec<Bytes>, GatewayError> {
        let mut chunks = self.close_active()?;
        let id = item_id
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| self.generated_id("fc"));
        let call_id = call_id
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| self.generated_id("call"));
        let name = name
            .filter(|value| !value.is_empty())
            .unwrap_or("tool")
            .to_owned();
        let arguments = arguments.unwrap_or_default().to_owned();
        self.add_buffered_bytes(arguments.len())?;
        let output_index = self.take_output_index();
        let item = tool_item(&id, &call_id, &name, &arguments, "in_progress");
        self.active = Some(ActiveOutput::Tool {
            id,
            output_index,
            call_id,
            name,
            arguments,
        });
        chunks.push(self.emit(
            "response.output_item.added",
            json!({
                "output_index": output_index,
                "item": item,
            }),
        )?);
        Ok(chunks)
    }

    fn tool_arguments_delta(
        &mut self,
        item_id: Option<&str>,
        call_id: Option<&str>,
        delta: &str,
    ) -> Result<Vec<Bytes>, GatewayError> {
        let ActiveOutput::Tool {
            id,
            output_index,
            call_id: active_call_id,
            arguments,
            ..
        } = self
            .active
            .as_mut()
            .ok_or_else(|| GatewayError::protocol("tool arguments arrived before tool start"))?
        else {
            return Err(GatewayError::protocol(
                "tool arguments arrived outside an active tool call",
            ));
        };
        if item_id.is_some_and(|item_id| item_id != id)
            || call_id.is_some_and(|call_id| call_id != active_call_id)
        {
            return Err(GatewayError::protocol(
                "tool argument identifiers did not match the active tool call",
            ));
        }
        self.buffered_output_bytes = checked_total(self.buffered_output_bytes, delta.len())?;
        arguments.push_str(delta);
        let id = id.clone();
        let call_id = active_call_id.clone();
        let output_index = *output_index;
        Ok(vec![self.emit(
            "response.function_call_arguments.delta",
            json!({
                "item_id": id,
                "call_id": call_id,
                "output_index": output_index,
                "delta": delta,
            }),
        )?])
    }

    fn complete_tool(
        &mut self,
        item_id: Option<&str>,
        call_id: Option<&str>,
        name: Option<&str>,
        arguments: Option<&str>,
    ) -> Result<Vec<Bytes>, GatewayError> {
        let matches_active = matches!(
            &self.active,
            Some(ActiveOutput::Tool {
                id,
                call_id: active_call_id,
                ..
            }) if item_id.is_none_or(|item_id| item_id == id)
                && call_id.is_none_or(|call_id| call_id == active_call_id)
        );
        let mut chunks = Vec::new();
        if !matches_active {
            chunks.extend(self.start_tool(item_id, call_id, name, arguments)?);
        } else if let Some(arguments) = arguments {
            self.replace_tool_arguments(arguments)?;
        }
        if let Some(name) = name
            && !name.is_empty()
            && let Some(ActiveOutput::Tool {
                name: active_name, ..
            }) = self.active.as_mut()
        {
            *active_name = name.to_owned();
        }
        chunks.extend(self.close_active()?);
        Ok(chunks)
    }

    fn close_active(&mut self) -> Result<Vec<Bytes>, GatewayError> {
        let Some(active) = self.active.take() else {
            return Ok(Vec::new());
        };
        match active {
            ActiveOutput::Message {
                id,
                output_index,
                phase,
                text,
                ..
            } => {
                let item = message_item(&id, phase, &text, "completed");
                self.output.push(item.clone());
                Ok(vec![
                    self.emit(
                        "response.output_text.done",
                        json!({
                            "item_id": id,
                            "output_index": output_index,
                            "content_index": 0,
                            "text": text,
                        }),
                    )?,
                    self.emit(
                        "response.content_part.done",
                        json!({
                            "item_id": id,
                            "output_index": output_index,
                            "content_index": 0,
                            "part": {
                                "type": "output_text",
                                "text": text,
                                "annotations": [],
                            },
                        }),
                    )?,
                    self.emit(
                        "response.output_item.done",
                        json!({
                            "output_index": output_index,
                            "item": item,
                        }),
                    )?,
                ])
            }
            ActiveOutput::Reasoning {
                id,
                source_id: _,
                output_index,
                mut parts,
            } => {
                let mut chunks = Vec::new();
                for (summary_index, part) in &mut parts {
                    if !part.done {
                        chunks.push(self.emit(
                            "response.reasoning_summary_text.done",
                            json!({
                                "item_id": id,
                                "output_index": output_index,
                                "summary_index": summary_index,
                                "text": part.text,
                            }),
                        )?);
                        part.done = true;
                    }
                }
                let item = reasoning_item(&id, &parts, "completed");
                self.output.push(item.clone());
                chunks.push(self.emit(
                    "response.output_item.done",
                    json!({
                        "output_index": output_index,
                        "item": item,
                    }),
                )?);
                Ok(chunks)
            }
            ActiveOutput::Tool {
                id,
                output_index,
                call_id,
                name,
                arguments,
            } => {
                let item = tool_item(&id, &call_id, &name, &arguments, "completed");
                self.output.push(item.clone());
                Ok(vec![
                    self.emit(
                        "response.function_call_arguments.done",
                        json!({
                            "item_id": id,
                            "call_id": call_id,
                            "output_index": output_index,
                            "arguments": arguments,
                        }),
                    )?,
                    self.emit(
                        "response.output_item.done",
                        json!({
                            "output_index": output_index,
                            "item": item,
                        }),
                    )?,
                ])
            }
        }
    }

    fn append_active_message(&mut self, delta: &str) -> Result<(String, u64), GatewayError> {
        let ActiveOutput::Message {
            id,
            output_index,
            text,
            ..
        } = self
            .active
            .as_mut()
            .ok_or_else(|| GatewayError::protocol("message state was not initialized"))?
        else {
            return Err(GatewayError::protocol("message state was not initialized"));
        };
        self.buffered_output_bytes = checked_total(self.buffered_output_bytes, delta.len())?;
        text.push_str(delta);
        Ok((id.clone(), *output_index))
    }

    fn active_message_text(&self) -> Result<&str, GatewayError> {
        match self.active.as_ref() {
            Some(ActiveOutput::Message { text, .. }) => Ok(text),
            _ => Err(GatewayError::protocol("message state was not initialized")),
        }
    }

    fn bind_active_reasoning_source(&mut self, item_id: Option<&str>) {
        let Some(item_id) = item_id else {
            return;
        };
        if let Some(ActiveOutput::Message {
            source: MessageSource::Reasoning { source_id, .. },
            ..
        }) = self.active.as_mut()
            && source_id.is_none()
        {
            *source_id = Some(item_id.to_owned());
        }
    }

    fn append_reasoning(
        &mut self,
        summary_index: u64,
        delta: &str,
    ) -> Result<(String, u64), GatewayError> {
        let ActiveOutput::Reasoning {
            id,
            output_index,
            parts,
            ..
        } = self
            .active
            .as_mut()
            .ok_or_else(|| GatewayError::protocol("reasoning state was not initialized"))?
        else {
            return Err(GatewayError::protocol(
                "reasoning state was not initialized",
            ));
        };
        let part = parts
            .get_mut(&summary_index)
            .ok_or_else(|| GatewayError::protocol("reasoning summary part was not initialized"))?;
        if part.done {
            return Err(GatewayError::protocol(
                "reasoning delta arrived after its completion",
            ));
        }
        self.buffered_output_bytes = checked_total(self.buffered_output_bytes, delta.len())?;
        part.text.push_str(delta);
        Ok((id.clone(), *output_index))
    }

    fn reasoning_part_text(&self, summary_index: u64) -> Result<String, GatewayError> {
        match self.active.as_ref() {
            Some(ActiveOutput::Reasoning { parts, .. }) => parts
                .get(&summary_index)
                .map(|part| part.text.clone())
                .ok_or_else(|| {
                    GatewayError::protocol("reasoning summary part was not initialized")
                }),
            _ => Err(GatewayError::protocol(
                "reasoning state was not initialized",
            )),
        }
    }

    fn mark_reasoning_done(
        &mut self,
        summary_index: u64,
    ) -> Result<(String, u64, bool), GatewayError> {
        let ActiveOutput::Reasoning {
            id,
            output_index,
            parts,
            ..
        } = self
            .active
            .as_mut()
            .ok_or_else(|| GatewayError::protocol("reasoning state was not initialized"))?
        else {
            return Err(GatewayError::protocol(
                "reasoning state was not initialized",
            ));
        };
        let part = parts
            .get_mut(&summary_index)
            .ok_or_else(|| GatewayError::protocol("reasoning summary part was not initialized"))?;
        let already_done = part.done;
        part.done = true;
        Ok((id.clone(), *output_index, already_done))
    }

    fn replace_tool_arguments(&mut self, value: &str) -> Result<(), GatewayError> {
        let Some(ActiveOutput::Tool { arguments, .. }) = self.active.as_mut() else {
            return Err(GatewayError::protocol("tool state was not initialized"));
        };
        let without_existing = self
            .buffered_output_bytes
            .checked_sub(arguments.len())
            .ok_or_else(|| GatewayError::protocol("projected output accounting underflowed"))?;
        self.buffered_output_bytes = checked_total(without_existing, value.len())?;
        arguments.clear();
        arguments.push_str(value);
        Ok(())
    }

    fn add_buffered_bytes(&mut self, additional: usize) -> Result<(), GatewayError> {
        self.buffered_output_bytes = checked_total(self.buffered_output_bytes, additional)?;
        Ok(())
    }

    fn usage_json(&self) -> Option<Value> {
        if self.usage.input_tokens.is_none()
            && self.usage.output_tokens.is_none()
            && self.usage.total_tokens.is_none()
        {
            return None;
        }
        let input_tokens = self.usage.input_tokens.unwrap_or(0);
        let output_tokens = self.usage.output_tokens.unwrap_or(0);
        let total_tokens = self
            .usage
            .total_tokens
            .unwrap_or_else(|| input_tokens.saturating_add(output_tokens));
        Some(json!({
            "input_tokens": input_tokens,
            "input_tokens_details": null,
            "output_tokens": output_tokens,
            "output_tokens_details": null,
            "total_tokens": total_tokens,
        }))
    }

    fn take_output_index(&mut self) -> u64 {
        let index = self.next_output_index;
        self.next_output_index = self.next_output_index.saturating_add(1);
        index
    }

    fn generated_id(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}_opsail_{}", self.next_generated_id);
        self.next_generated_id = self.next_generated_id.saturating_add(1);
        id
    }

    fn emit(&mut self, kind: &str, mut value: Value) -> Result<Bytes, GatewayError> {
        value["sequence_number"] = Value::from(self.next_sequence);
        self.next_sequence = self.next_sequence.saturating_add(1);
        encode_json_event(kind, value)
    }
}

fn checked_total(current: usize, additional: usize) -> Result<usize, GatewayError> {
    let total = current
        .checked_add(additional)
        .ok_or_else(|| GatewayError::protocol("projected output size overflowed"))?;
    if total > MAX_PROJECTED_OUTPUT_BYTES {
        return Err(GatewayError::protocol(
            "mapped output exceeded the 32 MiB aggregation limit",
        ));
    }
    Ok(total)
}

fn sources_compatible(active: &Option<String>, incoming: Option<&str>) -> bool {
    active.is_none() || incoming.is_none() || active.as_deref() == incoming
}

fn message_item(id: &str, phase: MessagePhase, text: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "status": status,
        "role": "assistant",
        "phase": phase,
        "content": [{
            "type": "output_text",
            "text": text,
            "annotations": [],
        }],
    })
}

fn reasoning_item(id: &str, parts: &BTreeMap<u64, ReasoningPart>, status: &str) -> Value {
    let summary = parts
        .values()
        .filter(|part| !part.text.is_empty())
        .map(|part| {
            json!({
                "type": "summary_text",
                "text": part.text,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "id": id,
        "type": "reasoning",
        "status": status,
        "summary": summary,
        "content": null,
        "encrypted_content": null,
    })
}

fn tool_item(id: &str, call_id: &str, name: &str, arguments: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "function_call",
        "status": status,
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::OPSAIL_EVENT_SCHEMA_VERSION;

    fn event(sequence: u64, event: OpsailEventKind) -> OpsailEventV1 {
        OpsailEventV1 {
            schema_version: OPSAIL_EVENT_SCHEMA_VERSION,
            run_id: "resp-1".to_owned(),
            sequence,
            event,
        }
    }

    fn as_text(chunks: Vec<Bytes>) -> String {
        chunks
            .into_iter()
            .map(|chunk| String::from_utf8(chunk.to_vec()).unwrap())
            .collect()
    }

    #[test]
    fn canonical_projection_emits_codex_message_lifecycle_and_usage() {
        let mut projector = CanonicalResponsesProjector::new(ReasoningDisplay::Commentary);
        let mut output = String::new();
        output.push_str(&as_text(
            projector
                .push(&event(
                    0,
                    OpsailEventKind::RunStarted {
                        model: Some("third-party".to_owned()),
                    },
                ))
                .unwrap(),
        ));
        output.push_str(&as_text(
            projector
                .push(&event(
                    1,
                    OpsailEventKind::AssistantTextDelta {
                        item_id: Some("msg-1".to_owned()),
                        phase: Some(MessagePhase::FinalAnswer),
                        delta: "hello".to_owned(),
                    },
                ))
                .unwrap(),
        ));
        projector
            .push(&event(
                2,
                OpsailEventKind::UsageUpdated {
                    input_tokens: Some(3),
                    output_tokens: Some(2),
                    total_tokens: Some(5),
                },
            ))
            .unwrap();
        output.push_str(&as_text(
            projector
                .push(&event(
                    3,
                    OpsailEventKind::RunCompleted {
                        response_id: Some("resp-1".to_owned()),
                        model: Some("third-party".to_owned()),
                    },
                ))
                .unwrap(),
        ));

        assert!(output.contains("response.created"));
        assert!(output.contains("response.output_text.delta"));
        assert!(output.contains("response.output_item.done"));
        assert!(output.contains(r#""phase":"final_answer""#));
        assert!(output.contains(r#""total_tokens":5"#));
        projector.finish().unwrap();
    }

    #[test]
    fn commentary_projection_exposes_only_reasoning_summary_text() {
        let mut projector = CanonicalResponsesProjector::new(ReasoningDisplay::Commentary);
        projector
            .push(&event(0, OpsailEventKind::RunStarted { model: None }))
            .unwrap();
        let delta = as_text(
            projector
                .push(&event(
                    1,
                    OpsailEventKind::ReasoningSummaryDelta {
                        item_id: Some("reasoning-1".to_owned()),
                        summary_index: Some(0),
                        delta: "Visible".to_owned(),
                    },
                ))
                .unwrap(),
        );
        let done = as_text(
            projector
                .push(&event(
                    2,
                    OpsailEventKind::ReasoningSummaryCompleted {
                        item_id: "reasoning-1".to_owned(),
                        summary_index: 0,
                        text: "Visible summary".to_owned(),
                    },
                ))
                .unwrap(),
        );
        assert!(delta.contains(r#""phase":"commentary""#));
        assert!(done.contains("Visible summary"));
        assert!(!format!("{delta}{done}").contains(r#""type":"reasoning""#));
    }

    #[test]
    fn strict_projection_preserves_reasoning_and_tool_arguments() {
        let mut projector = CanonicalResponsesProjector::new(ReasoningDisplay::Strict);
        projector
            .push(&event(0, OpsailEventKind::RunStarted { model: None }))
            .unwrap();
        let reasoning = as_text(
            projector
                .push(&event(
                    1,
                    OpsailEventKind::ReasoningSummaryCompleted {
                        item_id: "reasoning-1".to_owned(),
                        summary_index: 0,
                        text: "summary".to_owned(),
                    },
                ))
                .unwrap(),
        );
        assert!(reasoning.contains("response.reasoning_summary_text.done"));
        let mut tool = as_text(
            projector
                .push(&event(
                    2,
                    OpsailEventKind::ToolCallStarted {
                        item_id: Some("fc-1".to_owned()),
                        call_id: Some("call-1".to_owned()),
                        name: Some("shell".to_owned()),
                        arguments: None,
                    },
                ))
                .unwrap(),
        );
        tool.push_str(&as_text(
            projector
                .push(&event(
                    3,
                    OpsailEventKind::ToolCallArgumentsDelta {
                        item_id: Some("fc-1".to_owned()),
                        call_id: Some("call-1".to_owned()),
                        delta: "{\"cmd\":\"pwd\"}".to_owned(),
                    },
                ))
                .unwrap(),
        ));
        tool.push_str(&as_text(
            projector
                .push(&event(
                    4,
                    OpsailEventKind::ToolCallCompleted {
                        item_id: Some("fc-1".to_owned()),
                        call_id: Some("call-1".to_owned()),
                        name: Some("shell".to_owned()),
                        arguments: None,
                    },
                ))
                .unwrap(),
        ));
        assert!(tool.contains(r#""arguments":"{\"cmd\":\"pwd\"}""#));
        assert!(tool.contains("response.output_item.done"));
    }

    #[test]
    fn mapping_can_discriminate_on_the_sse_event_name() {
        let profile = EventMappingProfileV1::from_toml(
            r#"
version = 1
input = "sse-envelope"
discriminator = "/event"

[[rules]]
match = "vendor.begin"
emit = "run-started"
[rules.fields.run_id]
pointer = "/data/id"

[[rules]]
match = "vendor.end"
emit = "run-completed"
[rules.fields.response_id]
pointer = "/data/id"
"#,
        )
        .unwrap();
        let mut projector = MappedSseProjector::new(profile, ReasoningDisplay::Commentary).unwrap();
        let output = projector
            .push(
                b"event: vendor.begin\ndata: {\"id\":\"resp-envelope\"}\n\nevent: vendor.end\ndata: {\"id\":\"resp-envelope\"}\n\n",
            )
            .unwrap();
        let output = as_text(output.chunks);
        assert!(output.contains("response.created"));
        assert!(output.contains("response.completed"));
        projector.finish().unwrap();
    }
}
