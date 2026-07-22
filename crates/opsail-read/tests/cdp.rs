use std::future::Future;

use futures_util::{SinkExt, StreamExt};
use opsail_read::{
    CdpSource, CdpWaitUntil, ChromeError, ReadError, ReadOptions, ReadSource, SourceKind, read,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

fn article_html(title: &str) -> String {
    let words = (0..140)
        .map(|index| format!("browser{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<!doctype html><html><head><title>{title}</title></head><body><main><article><p>{words}</p></article></main></body></html>"
    )
}

async fn websocket_server<F, Fut>(handler: F) -> (String, JoinHandle<Result<(), String>>)
where
    F: FnOnce(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        handler(stream).await
    });
    (format!("ws://{address}"), task)
}

async fn next_command(
    socket: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
) -> Result<Value, String> {
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(|| "CDP client disconnected".to_owned())?
            .map_err(|error| error.to_string())?;
        match message {
            Message::Text(text) => {
                return serde_json::from_str(text.as_ref()).map_err(|error| error.to_string());
            }
            Message::Binary(bytes) => {
                return serde_json::from_slice(bytes.as_ref()).map_err(|error| error.to_string());
            }
            Message::Close(_) => return Err("CDP client closed the connection".to_owned()),
            Message::Ping(payload) => socket
                .send(Message::Pong(payload))
                .await
                .map_err(|error| error.to_string())?,
            Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

async fn respond(
    socket: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
    command: &Value,
    result: Value,
) -> Result<(), String> {
    socket
        .send(Message::Text(
            json!({ "id": command["id"], "result": result })
                .to_string()
                .into(),
        ))
        .await
        .map_err(|error| error.to_string())
}

fn frame_tree(frame_id: &str, loader_id: &str, url: &str) -> Value {
    json!({
        "frameTree": {
            "frame": {
                "id": frame_id,
                "loaderId": loader_id,
                "url": url
            }
        }
    })
}

fn rendered_capture(html: &str, final_url: &str, results: Value) -> Value {
    json!({
        "result": {
            "type": "object",
            "value": {
                "html": html,
                "finalUrl": final_url,
                "renderedEvidence": {
                    "timedOut": false,
                    "results": results
                }
            }
        }
    })
}

async fn reject_command(
    socket: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
    command: &Value,
) -> Result<(), String> {
    socket
        .send(Message::Text(
            json!({
                "id": command["id"],
                "error": { "code": -32601, "message": "unsupported in this fixture" }
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|error| error.to_string())
}

#[tokio::test]
async fn navigates_and_captures_through_a_browser_cdp_endpoint() {
    let html = article_html("Rendered through CDP");
    let final_url = "https://example.test/rendered?final=1";
    let expected_html = html.clone();
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        let mut saw_profile = false;
        let mut saw_navigation = false;
        let mut saw_close = false;

        while let Ok(command) = next_command(&mut socket).await {
            let method = command["method"].as_str().unwrap_or_default();
            match method {
                "Browser.getVersion" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({ "userAgent": "MockChrome/1.0" }),
                    )
                    .await?;
                }
                "Target.createTarget" => {
                    assert_eq!(command["params"]["url"], "about:blank");
                    assert_eq!(command["params"]["background"], true);
                    respond(&mut socket, &command, json!({ "targetId": "target-1" })).await?;
                }
                "Target.attachToTarget" => {
                    assert_eq!(command["params"]["targetId"], "target-1");
                    assert_eq!(command["params"]["flatten"], true);
                    respond(&mut socket, &command, json!({ "sessionId": "session-1" })).await?;
                }
                "Page.enable"
                | "Runtime.enable"
                | "Runtime.runIfWaitingForDebugger"
                | "Network.enable" => {
                    assert_eq!(command["sessionId"], "session-1");
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.setLifecycleEventsEnabled" => {
                    assert_eq!(command["sessionId"], "session-1");
                    assert_eq!(command["params"]["enabled"], true);
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Emulation.setUserAgentOverride" => {
                    assert_eq!(command["params"]["userAgent"], "opsail-test/1");
                    assert_eq!(command["params"]["acceptLanguage"], "en-US");
                    saw_profile = true;
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.navigate" => {
                    assert_eq!(command["params"]["url"], "https://example.test/requested");
                    saw_navigation = true;
                    socket
                        .send(Message::Text(
                            json!({
                                "method": "Page.lifecycleEvent",
                                "sessionId": "session-1",
                                "params": {
                                    "name": "load",
                                    "loaderId": "previous-loader",
                                    "timestamp": 1
                                }
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .map_err(|error| error.to_string())?;
                    socket
                        .send(Message::Text(
                            json!({
                                "method": "Page.lifecycleEvent",
                                "sessionId": "session-1",
                                "params": {
                                    "name": "load",
                                    "loaderId": "loader-1",
                                    "timestamp": 2
                                }
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .map_err(|error| error.to_string())?;
                    respond(&mut socket, &command, json!({ "loaderId": "loader-1" })).await?;
                }
                "Page.getFrameTree" => {
                    respond(
                        &mut socket,
                        &command,
                        frame_tree("main-frame", "loader-1", final_url),
                    )
                    .await?;
                }
                "Page.createIsolatedWorld" => {
                    assert_eq!(command["params"]["frameId"], "main-frame");
                    assert_eq!(command["params"]["grantUniveralAccess"], false);
                    respond(&mut socket, &command, json!({ "executionContextId": 17 })).await?;
                }
                "Emulation.setFocusEmulationEnabled" => {
                    assert_eq!(command["params"]["enabled"], true);
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Runtime.callFunctionOn" => {
                    assert_eq!(command["params"]["executionContextId"], 17);
                    assert_eq!(
                        command["params"]["arguments"][0]["value"]
                            .as_array()
                            .map(Vec::len),
                        Some(4)
                    );
                    respond(
                        &mut socket,
                        &command,
                        rendered_capture(&expected_html, final_url, json!([])),
                    )
                    .await?;
                }
                "Runtime.evaluate" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "result": {
                                "type": "object",
                                "value": { "html": expected_html, "finalUrl": final_url }
                            }
                        }),
                    )
                    .await?;
                }
                "Target.closeTarget" => {
                    assert_eq!(command["params"]["targetId"], "target-1");
                    saw_close = true;
                    respond(&mut socket, &command, json!({ "success": true })).await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }

        assert!(saw_profile);
        assert!(saw_navigation);
        assert!(saw_close);
        Ok(())
    })
    .await;

    let mut source = CdpSource::new(format!("{base_endpoint}/devtools/browser/mock"));
    source.url = Some(Url::parse("https://example.test/requested").unwrap());
    source.wait_until = CdpWaitUntil::Load;
    let options = ReadOptions {
        user_agent: Some("opsail-test/1".to_owned()),
        accept_language: Some("en-US".to_owned()),
        ..ReadOptions::default()
    };

    let result = read(ReadSource::Cdp(source), &options).await.unwrap();
    assert_eq!(result.source.kind, SourceKind::Cdp);
    assert_eq!(result.source.requested, "https://example.test/requested");
    assert_eq!(
        result.source.resolved_url.as_ref().map(Url::as_str),
        Some(final_url)
    );
    assert_eq!(result.metadata.title, "Rendered through CDP");
    assert!(result.content.contains("browser139"));
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn maps_a_cdp_main_document_challenge_header_to_verification_required() {
    let html = article_html("Response header must win over article-like markup");
    let expected_html = html.clone();
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;

        while let Ok(command) = next_command(&mut socket).await {
            assert!(command.get("sessionId").is_none());
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" | "Network.enable" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.navigate" => {
                    socket
                        .send(Message::Text(
                            json!({
                                "method": "Network.responseReceived",
                                "params": {
                                    "loaderId": "challenge-loader",
                                    "frameId": "main-frame",
                                    "type": "Document",
                                    "response": {
                                        "status": 403,
                                        "url": "https://example.test/requested?token=request-secret",
                                        "headers": {
                                            "cf-mitigated": "challenge",
                                            "set-cookie": "private=must-not-be-retained"
                                        }
                                    }
                                }
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .map_err(|error| error.to_string())?;
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "loaderId": "challenge-loader",
                            "frameId": "main-frame"
                        }),
                    )
                    .await?;
                }
                "Runtime.evaluate" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "result": {
                                "type": "object",
                                "value": {
                                    "html": expected_html,
                                    "finalUrl": "https://example.test/requested?token=request-secret"
                                }
                            }
                        }),
                    )
                    .await?;
                }
                "Page.createIsolatedWorld" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({ "executionContextId": 21 }),
                    )
                    .await?;
                }
                "Emulation.setFocusEmulationEnabled" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Runtime.callFunctionOn" => {
                    respond(
                        &mut socket,
                        &command,
                        rendered_capture(
                            &expected_html,
                            "https://example.test/requested?token=request-secret",
                            json!([]),
                        ),
                    )
                    .await?;
                }
                "Page.getFrameTree" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "frameTree": {
                                "frame": {
                                    "id": "main-frame",
                                    "loaderId": "challenge-loader",
                                    "url": "https://example.test/requested?token=request-secret"
                                }
                            }
                        }),
                    )
                    .await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }
        Ok(())
    })
    .await;

    let mut source = CdpSource::new(format!("{base_endpoint}/devtools/page/current"));
    source.url =
        Some(Url::parse("https://example.test/requested?token=request-secret#fragment").unwrap());
    source.wait_until = CdpWaitUntil::None;

    let error = read(ReadSource::Cdp(source), &ReadOptions::default())
        .await
        .unwrap_err();
    let diagnostic = format!("{error:?}");
    assert!(matches!(
        error,
        ReadError::VerificationRequired { url }
            if url == "https://example.test/requested"
    ));
    assert!(!diagnostic.contains("request-secret"));
    assert!(!diagnostic.contains("must-not-be-retained"));
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn requires_live_rendered_evidence_for_a_browser_dom_fallback() {
    let html = r#"<!doctype html><html><body>
      <form id="challenge-form" action="/?__cf_chl_rt_tk=secret"></form>
      <script>window._cf_chl_opt = { cType: 'managed' };</script>
    </body></html>"#;
    let expected_html = html.to_owned();
    let final_url = "https://protected.example.test/article?token=request-secret";
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        while let Ok(command) = next_command(&mut socket).await {
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" | "Network.enable" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.navigate" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({ "frameId": "main-frame", "loaderId": "gate-loader" }),
                    )
                    .await?;
                }
                "Page.getFrameTree" => {
                    respond(
                        &mut socket,
                        &command,
                        frame_tree("main-frame", "gate-loader", final_url),
                    )
                    .await?;
                }
                "Page.createIsolatedWorld" => {
                    respond(&mut socket, &command, json!({ "executionContextId": 31 })).await?;
                }
                "Emulation.setFocusEmulationEnabled" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Runtime.callFunctionOn" => {
                    let declaration = command["params"]["functionDeclaration"]
                        .as_str()
                        .unwrap_or_default();
                    assert!(!declaration.contains("challenge-form"));
                    assert!(
                        command["params"]["arguments"][0]["value"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|probe| {
                                probe["selector"].as_str().is_some_and(|selector| {
                                    selector.starts_with("form#challenge-form[action]")
                                })
                            })
                    );
                    respond(
                        &mut socket,
                        &command,
                        rendered_capture(
                            &expected_html,
                            final_url,
                            json!([{
                                "id": 2,
                                "matches": 1,
                                "marker": {
                                    "visible": true,
                                    "stable": true,
                                    "viewportCoverage": 160,
                                    "hitCoverage": 120
                                },
                                "takeover": {
                                    "visible": true,
                                    "stable": true,
                                    "viewportCoverage": 920,
                                    "hitCoverage": 840
                                }
                            }]),
                        ),
                    )
                    .await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }
        Ok(())
    })
    .await;

    let mut source = CdpSource::new(format!("{base_endpoint}/devtools/page/current"));
    source.url = Some(Url::parse(final_url).unwrap());
    source.wait_until = CdpWaitUntil::None;

    let error = read(ReadSource::Cdp(source), &ReadOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ReadError::VerificationRequired { url }
            if url == "https://protected.example.test/article"
    ));
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn hidden_browser_markers_do_not_become_verification_errors() {
    let prose = (0..80)
        .map(|index| format!("content{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let html = format!(
        r#"<!doctype html><html><head><title>Normal page</title></head><body>
          <section><h1>Normal page</h1><p>{prose}</p></section>
          <form id="challenge-form" action="/?__cf_chl_rt_tk=example" hidden></form>
          <script>window._cf_chl_opt = {{}};</script>
        </body></html>"#
    );
    let expected_html = html.clone();
    let final_url = "https://example.test/normal";
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        while let Ok(command) = next_command(&mut socket).await {
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" | "Network.enable" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.navigate" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({ "frameId": "main-frame", "loaderId": "normal-loader" }),
                    )
                    .await?;
                }
                "Page.getFrameTree" => {
                    respond(
                        &mut socket,
                        &command,
                        frame_tree("main-frame", "normal-loader", final_url),
                    )
                    .await?;
                }
                "Page.createIsolatedWorld" => {
                    respond(&mut socket, &command, json!({ "executionContextId": 32 })).await?;
                }
                "Emulation.setFocusEmulationEnabled" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Runtime.callFunctionOn" => {
                    respond(
                        &mut socket,
                        &command,
                        rendered_capture(
                            &expected_html,
                            final_url,
                            json!([{
                                "id": 2,
                                "matches": 1,
                                "marker": {
                                    "visible": false,
                                    "stable": true,
                                    "viewportCoverage": 0,
                                    "hitCoverage": 0
                                },
                                "takeover": null
                            }]),
                        ),
                    )
                    .await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }
        Ok(())
    })
    .await;

    let mut source = CdpSource::new(format!("{base_endpoint}/devtools/page/current"));
    source.url = Some(Url::parse(final_url).unwrap());
    source.wait_until = CdpWaitUntil::None;

    let result = read(ReadSource::Cdp(source), &ReadOptions::default())
        .await
        .unwrap();
    assert_eq!(result.metadata.title, "Normal page");
    assert!(result.content.contains("content79"));
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn discards_rendered_gate_evidence_from_a_superseded_document() {
    let gate_html = r#"<!doctype html><html><body>
      <form id="challenge-form" action="/?__cf_chl_rt_tk=secret"></form>
      <script>window._cf_chl_opt = {};</script>
    </body></html>"#;
    let clean_html = article_html("Client navigation finished");
    let expected_gate = gate_html.to_owned();
    let expected_clean = clean_html.clone();
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        let mut frame_calls = 0;
        let mut probe_calls = 0;
        while let Ok(command) = next_command(&mut socket).await {
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" | "Network.enable" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.navigate" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({ "frameId": "main-frame", "loaderId": "gate-loader" }),
                    )
                    .await?;
                }
                "Page.getFrameTree" => {
                    frame_calls += 1;
                    let (loader, url) = if frame_calls == 1 {
                        ("gate-loader", "https://example.test/gate")
                    } else {
                        ("article-loader", "https://example.test/final")
                    };
                    respond(&mut socket, &command, frame_tree("main-frame", loader, url)).await?;
                }
                "Page.createIsolatedWorld" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({ "executionContextId": 40 + probe_calls }),
                    )
                    .await?;
                }
                "Emulation.setFocusEmulationEnabled" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Runtime.callFunctionOn" => {
                    probe_calls += 1;
                    let capture = if probe_calls == 1 {
                        rendered_capture(
                            &expected_gate,
                            "https://example.test/gate",
                            json!([{
                                "id": 2,
                                "matches": 1,
                                "marker": {
                                    "visible": true,
                                    "stable": true,
                                    "viewportCoverage": 200,
                                    "hitCoverage": 160
                                },
                                "takeover": {
                                    "visible": true,
                                    "stable": true,
                                    "viewportCoverage": 900,
                                    "hitCoverage": 800
                                }
                            }]),
                        )
                    } else {
                        rendered_capture(&expected_clean, "https://example.test/final", json!([]))
                    };
                    respond(&mut socket, &command, capture).await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }
        assert_eq!(probe_calls, 2);
        Ok(())
    })
    .await;

    let mut source = CdpSource::new(format!("{base_endpoint}/devtools/page/current"));
    source.url = Some(Url::parse("https://example.test/start").unwrap());
    source.wait_until = CdpWaitUntil::None;

    let result = read(ReadSource::Cdp(source), &ReadOptions::default())
        .await
        .unwrap();
    assert_eq!(result.metadata.title, "Client navigation finished");
    assert_eq!(
        result.source.resolved_url.as_ref().map(Url::as_str),
        Some("https://example.test/final")
    );
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn binds_cdp_response_evidence_to_the_captured_final_main_document() {
    let html = article_html("Clean final document");
    let expected_html = html.clone();
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;

        while let Ok(command) = next_command(&mut socket).await {
            assert!(command.get("sessionId").is_none());
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" | "Network.enable" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.navigate" => {
                    for event in [
                        json!({
                            "method": "Network.responseReceived",
                            "params": {
                                "loaderId": "initial-loader",
                                "frameId": "main-frame",
                                "type": "Document",
                                "response": {
                                    "status": 403,
                                    "url": "https://example.test/start",
                                    "headers": { "cf-mitigated": "challenge" }
                                }
                            }
                        }),
                        json!({
                            "method": "Network.responseReceived",
                            "params": {
                                "loaderId": "iframe-loader",
                                "frameId": "child-frame",
                                "type": "Document",
                                "response": {
                                    "status": 403,
                                    "url": "https://example.test/final",
                                    "headers": { "cf-mitigated": "challenge" }
                                }
                            }
                        }),
                        json!({
                            "method": "Network.responseReceived",
                            "params": {
                                "loaderId": "final-loader",
                                "frameId": "main-frame",
                                "type": "Document",
                                "response": {
                                    "status": 200,
                                    "url": "https://example.test/final",
                                    "headers": {}
                                }
                            }
                        }),
                    ] {
                        socket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "loaderId": "initial-loader",
                            "frameId": "main-frame"
                        }),
                    )
                    .await?;
                }
                "Runtime.evaluate" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "result": {
                                "type": "object",
                                "value": {
                                    "html": expected_html,
                                    "finalUrl": "https://example.test/final"
                                }
                            }
                        }),
                    )
                    .await?;
                }
                "Page.createIsolatedWorld" => {
                    respond(&mut socket, &command, json!({ "executionContextId": 22 })).await?;
                }
                "Emulation.setFocusEmulationEnabled" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Runtime.callFunctionOn" => {
                    respond(
                        &mut socket,
                        &command,
                        rendered_capture(&expected_html, "https://example.test/final", json!([])),
                    )
                    .await?;
                }
                "Page.getFrameTree" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "frameTree": {
                                "frame": {
                                    "id": "main-frame",
                                    "loaderId": "final-loader",
                                    "url": "https://example.test/final"
                                }
                            }
                        }),
                    )
                    .await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }
        Ok(())
    })
    .await;

    let mut source = CdpSource::new(format!("{base_endpoint}/devtools/page/current"));
    source.url = Some(Url::parse("https://example.test/start").unwrap());
    source.wait_until = CdpWaitUntil::None;

    let result = read(ReadSource::Cdp(source), &ReadOptions::default())
        .await
        .unwrap();
    assert_eq!(result.metadata.title, "Clean final document");
    assert_eq!(
        result.source.resolved_url.as_ref().map(Url::as_str),
        Some("https://example.test/final")
    );
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn captures_a_direct_page_and_falls_back_to_the_dom_domain() {
    let html = article_html("DOM fallback");
    let expected_html = html.clone();
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        while let Ok(command) = next_command(&mut socket).await {
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" => {
                    assert!(command.get("sessionId").is_none());
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.getFrameTree" => {
                    respond(
                        &mut socket,
                        &command,
                        frame_tree(
                            "existing-frame",
                            "existing-loader",
                            "https://example.test/existing",
                        ),
                    )
                    .await?;
                }
                "Page.createIsolatedWorld" => reject_command(&mut socket, &command).await?,
                "Runtime.evaluate" => {
                    socket
                        .send(Message::Text(
                            json!({
                                "id": command["id"],
                                "error": { "code": -32601, "message": "Runtime unavailable" }
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .map_err(|error| error.to_string())?;
                }
                "DOM.getDocument" => {
                    respond(&mut socket, &command, json!({ "root": { "nodeId": 42 } })).await?;
                }
                "DOM.getOuterHTML" => {
                    assert_eq!(command["params"]["nodeId"], 42);
                    respond(&mut socket, &command, json!({ "outerHTML": expected_html })).await?;
                }
                "Page.getNavigationHistory" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "currentIndex": 0,
                            "entries": [{ "url": "https://example.test/existing" }]
                        }),
                    )
                    .await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }
        Ok(())
    })
    .await;

    let source = CdpSource::new(format!("{base_endpoint}/devtools/page/current"));
    let result = read(ReadSource::Cdp(source), &ReadOptions::default())
        .await
        .unwrap();

    assert_eq!(result.source.kind, SourceKind::Cdp);
    assert_eq!(result.metadata.title, "DOM fallback");
    assert_eq!(
        result.source.resolved_url.as_ref().map(Url::as_str),
        Some("https://example.test/existing")
    );
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn discovers_a_chrome_page_endpoint_without_publishing_endpoint_secrets() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let html = article_html("Discovered Chrome page");
    let expected_html = html.clone();
    let server = tokio::spawn(async move {
        let (mut discovery, _) = listener.accept().await.map_err(|error| error.to_string())?;
        let mut request = vec![0; 4096];
        let read = discovery
            .read(&mut request)
            .await
            .map_err(|error| error.to_string())?;
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.starts_with("GET /json/version?token=endpoint-secret HTTP/1.1"));
        let body = r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/page/discovered"}"#;
        discovery
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .map_err(|error| error.to_string())?;
        discovery
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;

        let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        while let Ok(command) = next_command(&mut socket).await {
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.getFrameTree" => {
                    respond(
                        &mut socket,
                        &command,
                        frame_tree(
                            "discovered-frame",
                            "discovered-loader",
                            "https://example.test/discovered",
                        ),
                    )
                    .await?;
                }
                "Page.createIsolatedWorld" => {
                    respond(&mut socket, &command, json!({ "executionContextId": 23 })).await?;
                }
                "Emulation.setFocusEmulationEnabled" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Runtime.callFunctionOn" => {
                    respond(
                        &mut socket,
                        &command,
                        rendered_capture(
                            &expected_html,
                            "https://example.test/discovered",
                            json!([]),
                        ),
                    )
                    .await?;
                }
                "Runtime.evaluate" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "result": {
                                "type": "object",
                                "value": {
                                    "html": expected_html,
                                    "finalUrl": "https://example.test/discovered"
                                }
                            }
                        }),
                    )
                    .await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }
        Ok::<(), String>(())
    });

    let source = CdpSource::new(format!("http://{address}?token=endpoint-secret"));
    let result = read(ReadSource::Cdp(source), &ReadOptions::default())
        .await
        .unwrap();

    assert_eq!(result.metadata.title, "Discovered Chrome page");
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("endpoint-secret")
    );
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn discovery_never_uses_an_arbitrary_page_from_json_list() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for (expected_path, status, body) in [
            ("/json/version", "404 Not Found", ""),
            (
                "/json/list",
                "200 OK",
                r#"[{"id":"page-1","type":"page","webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/page/page-1"},{"id":"page-2","type":"page","webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/page/page-2"}]"#,
            ),
        ] {
            let (mut stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
            let mut request = vec![0; 4096];
            let read = stream
                .read(&mut request)
                .await
                .map_err(|error| error.to_string())?;
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with(&format!("GET {expected_path} HTTP/1.1")));
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok::<(), String>(())
    });

    let source = CdpSource::new(format!("http://{address}"));
    let error = read(ReadSource::Cdp(source), &ReadOptions::default())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ReadError::Chrome(ChromeError::CdpDiscovery)
    ));
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn rejects_ambiguous_browser_page_selection_without_a_target_id() {
    let (base_endpoint, server) = websocket_server(|stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;

        while let Ok(command) = next_command(&mut socket).await {
            match command["method"].as_str().unwrap_or_default() {
                "Browser.getVersion" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({ "userAgent": "MockChrome/1.0" }),
                    )
                    .await?;
                }
                "Target.getTargets" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "targetInfos": [
                                {
                                    "targetId": "page-1",
                                    "type": "page",
                                    "url": "https://example.test/one"
                                },
                                {
                                    "targetId": "page-2",
                                    "type": "page",
                                    "url": "https://example.test/two"
                                }
                            ]
                        }),
                    )
                    .await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }
        Ok(())
    })
    .await;

    let source = CdpSource::new(format!("{base_endpoint}/devtools/browser/mock"));
    let error = read(ReadSource::Cdp(source), &ReadOptions::default())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ReadError::Chrome(ChromeError::CdpTargetAmbiguous)
    ));
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn closes_an_owned_target_when_attach_response_has_no_session_id() {
    let (base_endpoint, server) = websocket_server(|stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        let mut saw_close = false;

        while let Ok(command) = next_command(&mut socket).await {
            match command["method"].as_str().unwrap_or_default() {
                "Browser.getVersion" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({ "userAgent": "MockChrome/1.0" }),
                    )
                    .await?;
                }
                "Target.createTarget" => {
                    assert_eq!(command["params"]["background"], true);
                    respond(&mut socket, &command, json!({ "targetId": "owned-target" })).await?;
                }
                "Target.attachToTarget" => {
                    assert_eq!(command["params"]["targetId"], "owned-target");
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Target.closeTarget" => {
                    assert_eq!(command["params"]["targetId"], "owned-target");
                    saw_close = true;
                    respond(&mut socket, &command, json!({ "success": true })).await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }

        assert!(saw_close);
        Ok(())
    })
    .await;

    let mut source = CdpSource::new(format!("{base_endpoint}/devtools/browser/mock"));
    source.url = Some(Url::parse("https://example.test/requested").unwrap());
    source.wait_until = CdpWaitUntil::None;
    let error = read(ReadSource::Cdp(source), &ReadOptions::default())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ReadError::Chrome(ChromeError::CdpCommand {
            method: "Target.attachToTarget",
            ..
        })
    ));
    server.await.unwrap().unwrap();
}
