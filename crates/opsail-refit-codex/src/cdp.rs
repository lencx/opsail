use std::collections::HashMap;
use std::sync::{Arc, Once, OnceLock};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};
use url::Url;

use crate::error::{CodexRefitError, CodexRefitErrorCode};

const DISCOVERY_MAX_BYTES: usize = 1024 * 1024;
// The current Codex renderer module scope can exceed 16 MiB when inspected
// through Runtime.getProperties. Keep a finite ceiling with enough room for
// the one provider-routing scope inspection.
const MAX_CDP_MESSAGE_BYTES: usize = 24 * 1024 * 1024;
const TARGET_ID_MAX_BYTES: usize = 200;
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const CDP_TIMEOUT: Duration = Duration::from_secs(10);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone)]
pub(crate) struct RendererTarget {
    pub id: String,
    pub websocket_url: Url,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveryTarget {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    url: String,
    web_socket_debugger_url: String,
}

pub(crate) struct CdpSession {
    target_id: String,
    writer_tx: mpsc::UnboundedSender<WriterCommand>,
    pending: Arc<Mutex<HashMap<u64, PendingCommand>>>,
    termination: watch::Receiver<SessionTermination>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    next_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTermination {
    Open,
    Closed,
    Failed,
}

struct PendingCommand {
    method: &'static str,
    response: oneshot::Sender<Result<Value, CodexRefitError>>,
}

enum WriterCommand {
    Message {
        message: Message,
        complete: Option<oneshot::Sender<bool>>,
    },
    PeerClosed,
    Abort,
    Close {
        complete: oneshot::Sender<()>,
    },
}

impl CdpSession {
    pub async fn connect(target: &RendererTarget) -> Result<Self, CodexRefitError> {
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(MAX_CDP_MESSAGE_BYTES);
        config.max_frame_size = Some(MAX_CDP_MESSAGE_BYTES);
        let connected = timeout(
            Duration::from_secs(5),
            connect_async_with_config(target.websocket_url.as_str(), Some(config), false),
        )
        .await
        .map_err(|_| session_error("timed out connecting to the verified renderer"))?
        .map_err(|_| session_error("could not connect to the verified renderer"))?;
        let (sink, stream) = connected.0.split();
        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let (termination_tx, termination) = watch::channel(SessionTermination::Open);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let writer = tokio::spawn(run_socket_writer(
            sink,
            writer_rx,
            termination_tx.clone(),
            Arc::clone(&pending),
        ));
        let reader = tokio::spawn(run_socket_reader(
            stream,
            writer_tx.clone(),
            termination_tx,
            Arc::clone(&pending),
        ));
        Ok(Self {
            target_id: target.id.clone(),
            writer_tx,
            pending,
            termination,
            reader: Some(reader),
            writer: Some(writer),
            next_id: 1,
        })
    }

    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    pub async fn evaluate(&mut self, expression: &str) -> Result<Value, CodexRefitError> {
        let result = self
            .command(
                "Runtime.evaluate",
                Some(json!({
                    "expression": expression,
                    "awaitPromise": true,
                    "returnByValue": true,
                    "userGesture": false
                })),
            )
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err(CodexRefitError::new(
                CodexRefitErrorCode::InjectionFailed,
                "the renderer rejected the refit expression",
            ));
        }
        Ok(result
            .get("result")
            .and_then(|value| value.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub(crate) async fn evaluate_remote_object(
        &mut self,
        expression: &str,
        object_group: &str,
    ) -> Result<Value, CodexRefitError> {
        let result = self
            .command(
                "Runtime.evaluate",
                Some(json!({
                    "expression": expression,
                    "awaitPromise": true,
                    "returnByValue": false,
                    "objectGroup": object_group,
                    "userGesture": false
                })),
            )
            .await?;
        checked_runtime_result(result, "the renderer rejected the refit expression")
    }

    pub(crate) async fn get_properties(
        &mut self,
        object_id: &str,
    ) -> Result<Value, CodexRefitError> {
        self.command(
            "Runtime.getProperties",
            Some(json!({
                "objectId": object_id,
                "ownProperties": true,
                "accessorPropertiesOnly": false,
                "generatePreview": false
            })),
        )
        .await
    }

    pub(crate) async fn call_function_on(
        &mut self,
        object_id: &str,
        function_declaration: &str,
        arguments: Value,
    ) -> Result<Value, CodexRefitError> {
        let result = self
            .command(
                "Runtime.callFunctionOn",
                Some(json!({
                    "objectId": object_id,
                    "functionDeclaration": function_declaration,
                    "arguments": arguments,
                    "awaitPromise": true,
                    "returnByValue": true,
                    "userGesture": false
                })),
            )
            .await?;
        checked_runtime_result(result, "the renderer rejected the refit function")
    }

    pub(crate) async fn release_object_group(
        &mut self,
        object_group: &str,
    ) -> Result<(), CodexRefitError> {
        self.command(
            "Runtime.releaseObjectGroup",
            Some(json!({ "objectGroup": object_group })),
        )
        .await
        .map(|_| ())
    }

    pub async fn add_script(&mut self, source: &str) -> Result<String, CodexRefitError> {
        let result = self
            .command(
                "Page.addScriptToEvaluateOnNewDocument",
                Some(json!({ "source": source })),
            )
            .await?;
        result
            .get("identifier")
            .and_then(Value::as_str)
            .filter(|identifier| !identifier.is_empty() && identifier.len() <= 512)
            .map(str::to_owned)
            .ok_or_else(|| {
                CodexRefitError::new(
                    CodexRefitErrorCode::InjectionFailed,
                    "the renderer did not return an early-script identifier",
                )
            })
    }

    pub async fn remove_script(&mut self, identifier: &str) -> Result<(), CodexRefitError> {
        self.command(
            "Page.removeScriptToEvaluateOnNewDocument",
            Some(json!({ "identifier": identifier })),
        )
        .await
        .map(|_| ())
        .map_err(|_| {
            CodexRefitError::new(
                CodexRefitErrorCode::CleanupFailed,
                "could not remove a registered renderer script",
            )
        })
    }

    pub async fn close(&mut self) {
        let (complete, completed) = oneshot::channel();
        let _ = self.writer_tx.send(WriterCommand::Close { complete });
        let _ = timeout(Duration::from_millis(250), completed).await;
        if let Some(mut reader) = self.reader.take()
            && timeout(Duration::from_millis(250), &mut reader)
                .await
                .is_err()
        {
            reader.abort();
        }
        if let Some(mut writer) = self.writer.take()
            && timeout(Duration::from_millis(250), &mut writer)
                .await
                .is_err()
        {
            writer.abort();
        }
    }

    pub fn termination_receiver(&self) -> watch::Receiver<SessionTermination> {
        self.termination.clone()
    }

    async fn command(
        &mut self,
        method: &'static str,
        params: Option<Value>,
    ) -> Result<Value, CodexRefitError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut command = json!({ "id": id, "method": method });
        if let Some(params) = params {
            command["params"] = params;
        }
        let text = serde_json::to_string(&command)
            .map_err(|_| session_error("could not serialize a renderer command"))?;
        let (response, receiver) = oneshot::channel();
        self.pending
            .lock()
            .await
            .insert(id, PendingCommand { method, response });
        let (sent, sent_result) = oneshot::channel();
        let deadline = Instant::now() + CDP_TIMEOUT;
        if self
            .writer_tx
            .send(WriterCommand::Message {
                message: Message::Text(text.into()),
                complete: Some(sent),
            })
            .is_err()
        {
            self.pending.lock().await.remove(&id);
            return Err(session_error("renderer session closed"));
        }
        match timeout_at(deadline, sent_result).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false) | Err(_)) => {
                self.pending.lock().await.remove(&id);
                return Err(session_error("renderer command failed"));
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                return Err(session_error("renderer command timed out"));
            }
        }
        match timeout_at(deadline, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(session_error("renderer session closed")),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(session_error("renderer response timed out"))
            }
        }
    }
}

fn checked_runtime_result(result: Value, message: &'static str) -> Result<Value, CodexRefitError> {
    if result.get("exceptionDetails").is_some() {
        return Err(CodexRefitError::new(
            CodexRefitErrorCode::InjectionFailed,
            message,
        ));
    }
    result.get("result").cloned().ok_or_else(|| {
        CodexRefitError::new(
            CodexRefitErrorCode::InjectionFailed,
            "the renderer returned an invalid runtime result",
        )
    })
}

impl Drop for CdpSession {
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
        if let Some(writer) = self.writer.take() {
            writer.abort();
        }
    }
}

pub(crate) async fn wait_for_termination(
    mut receiver: watch::Receiver<SessionTermination>,
) -> SessionTermination {
    loop {
        let state = *receiver.borrow_and_update();
        if state != SessionTermination::Open {
            return state;
        }
        if receiver.changed().await.is_err() {
            return SessionTermination::Closed;
        }
    }
}

async fn run_socket_reader(
    mut stream: SplitStream<Socket>,
    writer: mpsc::UnboundedSender<WriterCommand>,
    termination: watch::Sender<SessionTermination>,
    pending: Arc<Mutex<HashMap<u64, PendingCommand>>>,
) {
    let final_state = loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => {
                if route_protocol_message(text.as_bytes(), &pending)
                    .await
                    .is_err()
                {
                    break SessionTermination::Failed;
                }
            }
            Some(Ok(Message::Binary(bytes))) => {
                if route_protocol_message(bytes.as_ref(), &pending)
                    .await
                    .is_err()
                {
                    break SessionTermination::Failed;
                }
            }
            Some(Ok(Message::Ping(payload))) => {
                if writer
                    .send(WriterCommand::Message {
                        message: Message::Pong(payload),
                        complete: None,
                    })
                    .is_err()
                {
                    break SessionTermination::Failed;
                }
            }
            Some(Ok(Message::Close(_))) | None => break SessionTermination::Closed,
            Some(Err(_)) => break SessionTermination::Failed,
            Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
        }
    };
    let control = if final_state == SessionTermination::Closed {
        WriterCommand::PeerClosed
    } else {
        WriterCommand::Abort
    };
    let _ = writer.send(control);
    terminate_session(&termination, &pending, final_state).await;
}

async fn run_socket_writer(
    mut sink: SplitSink<Socket, Message>,
    mut commands: mpsc::UnboundedReceiver<WriterCommand>,
    termination: watch::Sender<SessionTermination>,
    pending: Arc<Mutex<HashMap<u64, PendingCommand>>>,
) {
    while let Some(command) = commands.recv().await {
        match command {
            WriterCommand::Message { message, complete } => {
                let sent = sink.send(message).await.is_ok();
                if let Some(complete) = complete {
                    let _ = complete.send(sent);
                }
                if !sent {
                    terminate_session(&termination, &pending, SessionTermination::Failed).await;
                    return;
                }
            }
            WriterCommand::PeerClosed => {
                let _ = sink.flush().await;
                terminate_session(&termination, &pending, SessionTermination::Closed).await;
                return;
            }
            WriterCommand::Abort => {
                terminate_session(&termination, &pending, SessionTermination::Failed).await;
                return;
            }
            WriterCommand::Close { complete } => {
                let state = if sink.send(Message::Close(None)).await.is_ok() {
                    SessionTermination::Closed
                } else {
                    SessionTermination::Failed
                };
                let _ = complete.send(());
                terminate_session(&termination, &pending, state).await;
                return;
            }
        }
    }
    let _ = sink.send(Message::Close(None)).await;
    terminate_session(&termination, &pending, SessionTermination::Closed).await;
}

async fn terminate_session(
    termination: &watch::Sender<SessionTermination>,
    pending: &Arc<Mutex<HashMap<u64, PendingCommand>>>,
    state: SessionTermination,
) {
    if *termination.borrow() == SessionTermination::Open {
        let _ = termination.send(state);
    }
    let message = if state == SessionTermination::Closed {
        "renderer session closed"
    } else {
        "renderer session failed"
    };
    for (_, command) in pending.lock().await.drain() {
        let _ = command.response.send(Err(session_error(message)));
    }
}

async fn route_protocol_message(
    bytes: &[u8],
    pending: &Arc<Mutex<HashMap<u64, PendingCommand>>>,
) -> Result<(), CodexRefitError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| session_error("renderer returned invalid protocol JSON"))?;
    let Some(id) = value.get("id").and_then(Value::as_u64) else {
        return Ok(());
    };
    let Some(command) = pending.lock().await.remove(&id) else {
        return Ok(());
    };
    let result = if value.get("error").is_some() {
        Err(session_error(format!(
            "renderer rejected the `{}` command",
            command.method
        )))
    } else {
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    };
    let _ = command.response.send(result);
    Ok(())
}

pub(crate) async fn discover_targets(port: u16) -> Result<Vec<RendererTarget>, CodexRefitError> {
    install_tls_provider();
    let client = http_client()?;
    let endpoint = format!("http://127.0.0.1:{port}/json/list");
    let response = timeout(HTTP_TIMEOUT, client.get(endpoint).send())
        .await
        .map_err(|_| session_error("the loopback debug endpoint timed out"))?
        .map_err(|_| session_error("the loopback debug endpoint is unavailable"))?;
    if !response.status().is_success() {
        return Err(session_error(
            "the loopback debug endpoint rejected discovery",
        ));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = timeout(HTTP_TIMEOUT, stream.next())
        .await
        .map_err(|_| session_error("renderer discovery timed out"))?
    {
        let chunk = chunk.map_err(|_| session_error("renderer discovery failed"))?;
        if bytes.len().saturating_add(chunk.len()) > DISCOVERY_MAX_BYTES {
            return Err(CodexRefitError::new(
                CodexRefitErrorCode::TargetValidationFailed,
                "renderer discovery exceeded its response limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let values: Vec<Value> = serde_json::from_slice(&bytes).map_err(|_| {
        CodexRefitError::new(
            CodexRefitErrorCode::TargetValidationFailed,
            "renderer discovery returned an invalid target list",
        )
    })?;
    let collected = collect_valid_targets(values, port);
    if collected.targets.is_empty() {
        return Err(CodexRefitError::new(
            if collected.rejected > 0 {
                CodexRefitErrorCode::TargetValidationFailed
            } else {
                CodexRefitErrorCode::TargetNotFound
            },
            if collected.transitional > 0 {
                "the app renderer is still starting"
            } else {
                "no app renderer matched the required local target shape"
            },
        ));
    }
    Ok(collected.targets)
}

#[derive(Default)]
struct TargetCollection {
    targets: Vec<RendererTarget>,
    rejected: usize,
    transitional: usize,
}

enum TargetDisposition {
    Valid(RendererTarget),
    Transitional,
    Rejected,
}

fn collect_valid_targets(values: Vec<Value>, port: u16) -> TargetCollection {
    let mut collection = TargetCollection::default();
    for value in values {
        if value.get("type").and_then(Value::as_str) != Some("page") {
            continue;
        }
        let Ok(value) = serde_json::from_value::<DiscoveryTarget>(value) else {
            collection.rejected = collection.rejected.saturating_add(1);
            continue;
        };
        match classify_target(value, port) {
            TargetDisposition::Valid(target) => collection.targets.push(target),
            TargetDisposition::Transitional => {
                collection.transitional = collection.transitional.saturating_add(1);
            }
            TargetDisposition::Rejected => {
                collection.rejected = collection.rejected.saturating_add(1);
            }
        }
    }
    collection
}

fn classify_target(value: DiscoveryTarget, port: u16) -> TargetDisposition {
    if value.kind != "page" || !valid_target_id(&value.id) {
        return TargetDisposition::Rejected;
    }
    let Some(mut websocket_url) = validated_websocket_url(&value, port) else {
        return TargetDisposition::Rejected;
    };
    if value.url == "about:blank" {
        return TargetDisposition::Transitional;
    }
    if !is_local_app_renderer_url(&value.url) {
        return TargetDisposition::Rejected;
    }
    if websocket_url.set_host(Some("127.0.0.1")).is_err() {
        return TargetDisposition::Rejected;
    }
    TargetDisposition::Valid(RendererTarget {
        id: value.id,
        websocket_url,
    })
}

fn is_local_app_renderer_url(value: &str) -> bool {
    if value.len() > 8_192 {
        return false;
    }
    let Ok(renderer_url) = Url::parse(value) else {
        return false;
    };
    if renderer_url.scheme() != "app"
        || renderer_url.host_str().is_none_or(str::is_empty)
        || renderer_url.port().is_some()
        || renderer_url.path().len() <= 1
        || renderer_url.path().len() > 4_096
        || !renderer_url.username().is_empty()
        || renderer_url.password().is_some()
    {
        return false;
    }
    true
}

fn validated_websocket_url(value: &DiscoveryTarget, port: u16) -> Option<Url> {
    let websocket_url = Url::parse(&value.web_socket_debugger_url).ok()?;
    if websocket_url.scheme() != "ws"
        || !is_loopback_host(websocket_url.host_str()?)
        || websocket_url.port() != Some(port)
        || !websocket_url.username().is_empty()
        || websocket_url.password().is_some()
        || websocket_url.query().is_some()
        || websocket_url.fragment().is_some()
        || websocket_url.path() != format!("/devtools/page/{}", value.id)
    {
        return None;
    }
    Some(websocket_url)
}

fn valid_target_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= TARGET_ID_MAX_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_loopback_host(value: &str) -> bool {
    value == "127.0.0.1"
}

fn http_client() -> Result<&'static reqwest::Client, CodexRefitError> {
    static CLIENT: OnceLock<Result<reqwest::Client, ()>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(HTTP_TIMEOUT)
                .timeout(HTTP_TIMEOUT)
                .redirect(Policy::none())
                .no_proxy()
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|()| session_error("could not initialize the loopback HTTP client"))
}

fn install_tls_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn session_error(message: impl Into<String>) -> CodexRefitError {
    CodexRefitError::new(CodexRefitErrorCode::SessionUnavailable, message)
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::accept_async;

    use super::*;

    fn target(websocket_url: &str, renderer_url: &str) -> DiscoveryTarget {
        DiscoveryTarget {
            kind: "page".to_owned(),
            id: "renderer-1".to_owned(),
            url: renderer_url.to_owned(),
            web_socket_debugger_url: websocket_url.to_owned(),
        }
    }

    async fn test_listener() -> (TcpListener, RendererTarget) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = RendererTarget {
            id: "renderer-test".to_owned(),
            websocket_url: Url::parse(&format!(
                "ws://127.0.0.1:{port}/devtools/page/renderer-test"
            ))
            .unwrap(),
        };
        (listener, target)
    }

    #[test]
    fn accepts_only_loopback_app_page_candidates_with_matching_ids() {
        assert!(matches!(
            classify_target(
                target(
                    "ws://127.0.0.1:55321/devtools/page/renderer-1",
                    "app://-/index.html"
                ),
                55321
            ),
            TargetDisposition::Valid(_)
        ));
        for websocket_url in [
            "ws://example.test:55321/devtools/page/renderer-1",
            "ws://localhost:55321/devtools/page/renderer-1",
            "ws://[::1]:55321/devtools/page/renderer-1",
            "ws://127.0.0.1:55322/devtools/page/renderer-1",
            "ws://127.0.0.1:55321/devtools/page/another",
            "ws://127.0.0.1:55321/devtools/page/renderer-1?token=secret",
        ] {
            assert!(matches!(
                classify_target(target(websocket_url, "app://-/index.html"), 55321),
                TargetDisposition::Rejected
            ));
        }
        for renderer_url in [
            "https://example.test",
            "file:///Applications/ChatGPT.app/index.html",
            "app:///index.html",
            "app://-/",
            "app://user@-/index.html",
            "app://-:55321/index.html",
        ] {
            assert!(matches!(
                classify_target(
                    target(
                        "ws://127.0.0.1:55321/devtools/page/renderer-1",
                        renderer_url,
                    ),
                    55321,
                ),
                TargetDisposition::Rejected
            ));
        }
        for renderer_url in [
            "app://-/index.html?initialRoute=%2Flocal%2Fthread-id",
            "app://-/index.html?mcpAppSandboxDevtools=1#route",
            "app://shell/main.html?route=%2Flocal%2Fthread-id",
        ] {
            assert!(matches!(
                classify_target(
                    target(
                        "ws://127.0.0.1:55321/devtools/page/renderer-1",
                        renderer_url,
                    ),
                    55321,
                ),
                TargetDisposition::Valid(_)
            ));
        }
    }

    #[test]
    fn accepts_the_packaged_chatgpt_renderer_url() {
        assert!(matches!(
            classify_target(
                target(
                    "ws://127.0.0.1:55321/devtools/page/renderer-1",
                    "app://-/index.html",
                ),
                55321,
            ),
            TargetDisposition::Valid(_)
        ));
    }

    #[test]
    fn accepts_a_safe_future_local_app_renderer_for_the_identity_probe() {
        assert!(matches!(
            classify_target(
                target(
                    "ws://127.0.0.1:55321/devtools/page/renderer-1",
                    "app://shell/main.html?route=%2Flocal%2Fthread-id",
                ),
                55321,
            ),
            TargetDisposition::Valid(_)
        ));
    }

    #[test]
    fn startup_blank_page_is_transitional_but_wrong_app_pages_fail_closed() {
        assert!(matches!(
            classify_target(
                target(
                    "ws://127.0.0.1:55321/devtools/page/renderer-1",
                    "about:blank",
                ),
                55321,
            ),
            TargetDisposition::Transitional
        ));
        for renderer_url in ["about:blank?unexpected=true", "about:blank#unexpected"] {
            assert!(matches!(
                classify_target(
                    target(
                        "ws://127.0.0.1:55321/devtools/page/renderer-1",
                        renderer_url,
                    ),
                    55321,
                ),
                TargetDisposition::Rejected
            ));
        }

        let transitional = collect_valid_targets(
            vec![serde_json::json!({
                "type": "page",
                "id": "renderer-1",
                "url": "about:blank",
                "webSocketDebuggerUrl": "ws://127.0.0.1:55321/devtools/page/renderer-1",
            })],
            55321,
        );
        assert!(transitional.targets.is_empty());
        assert_eq!(transitional.transitional, 1);
        assert_eq!(transitional.rejected, 0);
    }

    #[test]
    fn target_ids_are_strictly_bounded() {
        assert!(valid_target_id("renderer-1._"));
        assert!(!valid_target_id(""));
        assert!(!valid_target_id("renderer/1"));
        assert!(!valid_target_id(&"a".repeat(TARGET_ID_MAX_BYTES + 1)));
    }

    #[test]
    fn malformed_unrelated_discovery_entries_do_not_hide_a_valid_renderer() {
        let values = vec![
            json!({ "type": "worker", "id": "unrelated" }),
            json!({
                "type": "page",
                "id": "renderer-1",
                "url": "app://-/index.html",
                "webSocketDebuggerUrl": "ws://127.0.0.1:55321/devtools/page/renderer-1"
            }),
        ];
        let collection = collect_valid_targets(values, 55321);
        assert_eq!(collection.targets.len(), 1);
        assert_eq!(collection.rejected, 0);
    }

    #[tokio::test]
    async fn connect_enables_no_cdp_domain_and_idle_sessions_do_not_poll() {
        let (listener, target) = test_listener().await;
        let (observed_tx, observed_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let observed = timeout(Duration::from_millis(100), socket.next())
                .await
                .ok()
                .flatten()
                .and_then(Result::ok)
                .and_then(|message| match message {
                    Message::Text(text) => serde_json::from_str::<Value>(text.as_ref()).ok(),
                    _ => None,
                })
                .and_then(|value| value["method"].as_str().map(str::to_owned));
            let _ = observed_tx.send(observed);
        });

        let mut session = CdpSession::connect(&target).await.unwrap();
        let observed = observed_rx.await.unwrap();
        assert_eq!(observed, None);
        for forbidden in [
            "Runtime.enable",
            "Page.enable",
            "Network.enable",
            "Debugger.enable",
            "Profiler.enable",
            "Tracing.start",
            "Page.startScreencast",
        ] {
            assert_ne!(observed.as_deref(), Some(forbidden));
        }
        session.close().await;
    }

    #[tokio::test]
    async fn reader_drains_events_routes_responses_and_reports_peer_close() {
        let (listener, target) = test_listener().await;
        let (acknowledged_tx, acknowledged_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(
                    json!({ "method": "Runtime.executionContextCreated", "params": {} })
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let request = socket.next().await.unwrap().unwrap();
            let Message::Text(request) = request else {
                panic!("expected a text command");
            };
            let request: Value = serde_json::from_str(request.as_ref()).unwrap();
            assert_eq!(request["method"], "Runtime.evaluate");
            socket
                .send(Message::Text(
                    json!({ "method": "Console.messageAdded", "params": {} })
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "id": request["id"],
                        "result": { "result": { "value": { "ok": true } } }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket.send(Message::Close(None)).await.unwrap();
            let acknowledged = matches!(
                timeout(Duration::from_millis(250), socket.next()).await,
                Ok(Some(Ok(Message::Close(_))))
            );
            let _ = acknowledged_tx.send(acknowledged);
        });

        let mut session = CdpSession::connect(&target).await.unwrap();
        let termination = session.termination_receiver();
        assert_eq!(session.evaluate("1").await.unwrap(), json!({ "ok": true }));
        assert_eq!(
            wait_for_termination(termination).await,
            SessionTermination::Closed
        );
        assert!(acknowledged_rx.await.unwrap());
        session.close().await;
    }

    #[tokio::test]
    async fn explicit_close_sends_a_websocket_close_frame() {
        let (listener, target) = test_listener().await;
        let (closed_tx, closed_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let closed = matches!(socket.next().await, Some(Ok(Message::Close(_))));
            let _ = closed_tx.send(closed);
        });

        let mut session = CdpSession::connect(&target).await.unwrap();
        session.close().await;
        assert!(closed_rx.await.unwrap());
    }
}
