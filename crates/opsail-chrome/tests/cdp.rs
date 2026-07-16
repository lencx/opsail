use std::future::Future;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use opsail_chrome::{
    CaptureOptions, CapturedPage, CdpSource, CdpWaitUntil, ChromeError, RenderedProbe, capture_cdp,
    capture_cdp_with_probes,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

type TestResult = Result<(), String>;
type TestSocket = WebSocketStream<TcpStream>;

async fn websocket_server<F, Fut>(handler: F) -> (String, JoinHandle<TestResult>)
where
    F: FnOnce(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = TestResult> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        handler(stream).await
    });
    (format!("ws://{address}"), task)
}

async fn next_command(socket: &mut TestSocket) -> Result<Value, String> {
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

async fn respond(socket: &mut TestSocket, command: &Value, result: Value) -> TestResult {
    socket
        .send(Message::Text(
            json!({ "id": command["id"], "result": result })
                .to_string()
                .into(),
        ))
        .await
        .map_err(|error| error.to_string())
}

async fn respond_with_error(socket: &mut TestSocket, command: &Value, message: &str) -> TestResult {
    socket
        .send(Message::Text(
            json!({
                "id": command["id"],
                "error": {
                    "code": -32000,
                    "message": message,
                    "data": { "description": message }
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|error| error.to_string())
}

fn assert_capture(capture: &CapturedPage, html: &str, final_url: &str) {
    assert_eq!(capture.html, html);
    assert_eq!(capture.final_url.as_str(), final_url);
}

#[derive(Clone, Copy)]
struct MockMainDocument {
    frame_id: &'static str,
    loader_id: &'static str,
    url: &'static str,
}

fn frame_tree(document: MockMainDocument) -> Value {
    json!({
        "frameTree": {
            "frame": {
                "id": document.frame_id,
                "loaderId": document.loader_id,
                "url": document.url
            }
        }
    })
}

async fn assert_unstable_probe_identity_is_discarded(
    case: &'static str,
    snapshots: [MockMainDocument; 4],
    observed_urls: [&'static str; 2],
) {
    const FALLBACK_HTML: &str = "<html><body>identity fallback</body></html>";
    const FALLBACK_URL: &str = "https://example.test/identity-fallback";

    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        let mut frame_tree_calls = 0;
        let mut probe_calls = 0;
        let mut saw_fallback = false;

        while let Ok(command) = next_command(&mut socket).await {
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.getFrameTree" => {
                    let document = snapshots
                        .get(frame_tree_calls)
                        .copied()
                        .ok_or_else(|| format!("{case}: unexpected extra frame-tree request"))?;
                    frame_tree_calls += 1;
                    respond(&mut socket, &command, frame_tree(document)).await?;
                }
                "Page.createIsolatedWorld" => {
                    let before = snapshots
                        .get(probe_calls * 2)
                        .ok_or_else(|| format!("{case}: missing before identity"))?;
                    assert_eq!(command["params"]["frameId"], before.frame_id, "{case}");
                    respond(
                        &mut socket,
                        &command,
                        json!({ "executionContextId": 100 + probe_calls }),
                    )
                    .await?;
                }
                "Emulation.setFocusEmulationEnabled" => {
                    assert_eq!(command["params"]["enabled"], true, "{case}");
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Runtime.callFunctionOn" => {
                    let final_url = observed_urls
                        .get(probe_calls)
                        .ok_or_else(|| format!("{case}: unexpected extra probe call"))?;
                    probe_calls += 1;
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "result": {
                                "type": "object",
                                "value": {
                                    "html": "<html><body>unstable probe</body></html>",
                                    "finalUrl": final_url,
                                    "renderedEvidence": {
                                        "timedOut": false,
                                        "results": [{
                                            "id": 11,
                                            "matches": 1,
                                            "marker": {
                                                "visible": true,
                                                "stable": true,
                                                "viewportCoverage": 100,
                                                "hitCoverage": 80
                                            },
                                            "takeover": null
                                        }]
                                    }
                                }
                            }
                        }),
                    )
                    .await?;
                }
                "Runtime.evaluate" => {
                    assert_eq!(frame_tree_calls, 4, "{case}");
                    assert_eq!(probe_calls, 2, "{case}");
                    saw_fallback = true;
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "result": {
                                "type": "object",
                                "value": {
                                    "html": FALLBACK_HTML,
                                    "finalUrl": FALLBACK_URL
                                }
                            }
                        }),
                    )
                    .await?;
                }
                other => return Err(format!("{case}: unexpected CDP command: {other}")),
            }
        }

        assert_eq!(frame_tree_calls, 4, "{case}");
        assert_eq!(probe_calls, 2, "{case}");
        assert!(saw_fallback, "{case}");
        Ok(())
    })
    .await;

    let source = CdpSource::new(format!("{base_endpoint}/devtools/page/current"));
    let probes = [RenderedProbe::new(11, "#verification-gate").unwrap()];
    let capture = capture_cdp_with_probes(&source, &CaptureOptions::default(), &probes)
        .await
        .unwrap();

    assert_capture(&capture, FALLBACK_HTML, FALLBACK_URL);
    assert!(capture.rendered_evidence().is_none(), "{case}");
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn rendered_probes_are_data_only_and_expose_bounded_evidence() {
    let expected_html = "<html><body>rendered evidence</body></html>";
    let final_url = "https://example.test/rendered-evidence";
    let gate_selector = r#"[data-opsail-probe="selector-must-remain-data"]"#;
    let iframe_selector = r#"iframe[src*="captcha.example"]"#;
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        let mut frame_tree_calls = 0;
        let mut saw_probe_call = false;

        while let Ok(command) = next_command(&mut socket).await {
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.getFrameTree" => {
                    frame_tree_calls += 1;
                    respond(
                        &mut socket,
                        &command,
                        frame_tree(MockMainDocument {
                            frame_id: "main-frame",
                            loader_id: "main-loader",
                            url: final_url,
                        }),
                    )
                    .await?;
                }
                "Page.createIsolatedWorld" => {
                    assert_eq!(command["params"]["frameId"], "main-frame");
                    assert_eq!(command["params"]["worldName"], "opsail-render-observer");
                    respond(&mut socket, &command, json!({ "executionContextId": 77 })).await?;
                }
                "Emulation.setFocusEmulationEnabled" => {
                    assert_eq!(command["params"]["enabled"], true);
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Runtime.callFunctionOn" => {
                    let declaration = command["params"]["functionDeclaration"]
                        .as_str()
                        .ok_or_else(|| "missing function declaration".to_owned())?;
                    assert!(!declaration.contains(gate_selector));
                    assert!(!declaration.contains(iframe_selector));
                    assert_eq!(
                        command["params"]["arguments"],
                        json!([{ "value": [
                            { "id": 7, "selector": gate_selector },
                            { "id": 9, "selector": iframe_selector }
                        ] }])
                    );
                    assert_eq!(command["params"]["executionContextId"], 77);
                    saw_probe_call = true;
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "result": {
                                "type": "object",
                                "value": {
                                    "html": expected_html,
                                    "finalUrl": final_url,
                                    "renderedEvidence": {
                                        "timedOut": false,
                                        "results": [
                                            {
                                                "id": 7,
                                                "matches": 2,
                                                "marker": {
                                                    "visible": true,
                                                    "stable": true,
                                                    "viewportCoverage": 125,
                                                    "hitCoverage": 80
                                                },
                                                "takeover": {
                                                    "visible": true,
                                                    "stable": false,
                                                    "viewportCoverage": 900,
                                                    "hitCoverage": 840
                                                }
                                            },
                                            {
                                                "id": 9,
                                                "matches": 0,
                                                "marker": null,
                                                "takeover": null
                                            }
                                        ]
                                    }
                                }
                            }
                        }),
                    )
                    .await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }

        assert_eq!(frame_tree_calls, 2);
        assert!(saw_probe_call);
        Ok(())
    })
    .await;

    let source = CdpSource::new(format!("{base_endpoint}/devtools/page/current"));
    let probes = [
        RenderedProbe::new(7, gate_selector).unwrap(),
        RenderedProbe::new(9, iframe_selector).unwrap(),
    ];
    let capture = capture_cdp_with_probes(&source, &CaptureOptions::default(), &probes)
        .await
        .unwrap();

    assert_capture(&capture, expected_html, final_url);
    let evidence = capture.rendered_evidence().expect("rendered evidence");
    assert_eq!(evidence.len(), 2);
    assert!(!evidence.is_empty());

    let gate = evidence.result(7).expect("gate probe result");
    assert_eq!(gate.id(), 7);
    assert_eq!(gate.matches(), 2);
    let marker = gate.marker().expect("gate marker surface");
    assert!(marker.visible());
    assert!(marker.stable());
    assert_eq!(marker.viewport_coverage_per_mille(), 125);
    assert_eq!(marker.hit_coverage_per_mille(), 80);
    let takeover = gate.takeover().expect("gate takeover surface");
    assert!(takeover.visible());
    assert!(!takeover.stable());
    assert_eq!(takeover.viewport_coverage_per_mille(), 900);
    assert_eq!(takeover.hit_coverage_per_mille(), 840);

    let iframe = evidence.result(9).expect("iframe probe result");
    assert_eq!(iframe.matches(), 0);
    assert!(iframe.marker().is_none());
    assert!(iframe.takeover().is_none());
    assert!(evidence.result(10).is_none());
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn rendered_evidence_requires_a_stable_main_document_identity() {
    const BASE: MockMainDocument = MockMainDocument {
        frame_id: "frame-a",
        loader_id: "loader-a",
        url: "https://example.test/stable",
    };
    const CHANGED_FRAME: MockMainDocument = MockMainDocument {
        frame_id: "frame-b",
        ..BASE
    };
    const CHANGED_LOADER: MockMainDocument = MockMainDocument {
        loader_id: "loader-b",
        ..BASE
    };
    const CHANGED_URL: MockMainDocument = MockMainDocument {
        url: "https://example.test/navigated",
        ..BASE
    };

    for (case, snapshots, observed_urls) in [
        (
            "frame changed",
            [BASE, CHANGED_FRAME, CHANGED_FRAME, BASE],
            [BASE.url, CHANGED_FRAME.url],
        ),
        (
            "loader changed",
            [BASE, CHANGED_LOADER, CHANGED_LOADER, BASE],
            [BASE.url, CHANGED_LOADER.url],
        ),
        (
            "URL changed",
            [BASE, CHANGED_URL, CHANGED_URL, BASE],
            [BASE.url, CHANGED_URL.url],
        ),
        (
            "captured URL differs from the current main document",
            [BASE, BASE, BASE, BASE],
            [
                "https://example.test/not-the-main-document",
                "https://example.test/not-the-main-document",
            ],
        ),
    ] {
        assert_unstable_probe_identity_is_discarded(case, snapshots, observed_urls).await;
    }
}

#[tokio::test]
async fn duplicate_rendered_probe_ids_are_rejected_before_endpoint_resolution() {
    let source = CdpSource::new("://this-endpoint-must-never-be-resolved");
    let probes = [
        RenderedProbe::new(3, "#first").unwrap(),
        RenderedProbe::new(3, "#second").unwrap(),
    ];

    let error = capture_cdp_with_probes(&source, &CaptureOptions::default(), &probes)
        .await
        .unwrap_err();

    assert!(matches!(error, ChromeError::InvalidRenderedProbe));
}

#[tokio::test]
async fn browser_endpoint_navigates_with_request_profile_and_closes_owned_target() {
    let expected_html = "<html><body>browser endpoint</body></html>";
    let final_url = "https://example.test/rendered?final=1";
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        let mut saw_profile = false;
        let mut saw_navigation = false;
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
                    assert_eq!(command["params"]["url"], "about:blank");
                    assert_eq!(command["params"]["background"], true);
                    respond(&mut socket, &command, json!({ "targetId": "owned-target" })).await?;
                }
                "Target.attachToTarget" => {
                    assert_eq!(command["params"]["targetId"], "owned-target");
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
                    assert_eq!(command["sessionId"], "session-1");
                    assert_eq!(command["params"]["userAgent"], "opsail-contract/1");
                    assert_eq!(command["params"]["acceptLanguage"], "zh-CN,en-US;q=0.8");
                    saw_profile = true;
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.navigate" => {
                    assert_eq!(command["sessionId"], "session-1");
                    assert_eq!(command["params"]["url"], "https://example.test/requested");
                    saw_navigation = true;
                    socket
                        .send(Message::Text(
                            json!({
                                "method": "Network.responseReceived",
                                "sessionId": "session-1",
                                "params": {
                                    "loaderId": "loader-1",
                                    "frameId": "frame-1",
                                    "type": "Document",
                                    "response": {
                                        "status": 403,
                                        "url": final_url,
                                        "headers": {
                                            "CF-Mitigated": "challenge",
                                            "set-cookie": "session=must-not-be-retained"
                                        }
                                    }
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
                                    "timestamp": 1
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
                        json!({ "loaderId": "loader-1", "frameId": "frame-1" }),
                    )
                    .await?;
                }
                "Runtime.evaluate" => {
                    assert_eq!(command["sessionId"], "session-1");
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "result": {
                                "type": "object",
                                "value": {
                                    "html": expected_html,
                                    "finalUrl": final_url
                                }
                            }
                        }),
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
                                    "id": "frame-1",
                                    "loaderId": "loader-1",
                                    "url": final_url
                                }
                            }
                        }),
                    )
                    .await?;
                }
                "Target.closeTarget" => {
                    assert_eq!(command["params"]["targetId"], "owned-target");
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
    let options = CaptureOptions {
        user_agent: Some("opsail-contract/1".to_owned()),
        accept_language: Some("zh-CN,en-US;q=0.8".to_owned()),
        ..CaptureOptions::default()
    };

    let capture = capture_cdp(&source, &options).await.unwrap();
    assert_capture(&capture, expected_html, final_url);
    let response = capture.response().expect("main response metadata");
    assert_eq!(response.status(), 403);
    assert_eq!(response.header("cf-mitigated"), Some("challenge"));
    assert_eq!(response.header("set-cookie"), None);
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn borrowed_direct_page_preserves_its_user_agent_without_profile_options() {
    let expected_html = "<html><body>preserved profile</body></html>";
    let final_url = "https://example.test/preserved";
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        let mut saw_navigation = false;

        while let Ok(command) = next_command(&mut socket).await {
            assert!(command.get("sessionId").is_none());
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" | "Network.enable" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.navigate" => {
                    assert_eq!(command["params"]["url"], "https://example.test/requested");
                    saw_navigation = true;
                    respond(&mut socket, &command, json!({})).await?;
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
                                    "finalUrl": final_url
                                }
                            }
                        }),
                    )
                    .await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }

        assert!(saw_navigation);
        Ok(())
    })
    .await;

    let mut source = CdpSource::new(format!("{base_endpoint}/devtools/page/current"));
    source.url = Some(Url::parse("https://example.test/requested").unwrap());
    source.wait_until = CdpWaitUntil::None;

    let capture = capture_cdp(&source, &CaptureOptions::default())
        .await
        .unwrap();
    assert_capture(&capture, expected_html, final_url);
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn borrowed_direct_page_uses_its_current_user_agent_for_accept_language_override() {
    let browser_user_agent =
        "Mozilla/5.0 AppleWebKit/537.36 HeadlessChrome/150.0.0.0 Safari/537.36";
    let expected_html = "<html><body>language profile</body></html>";
    let final_url = "https://example.test/language";
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        let mut saw_profile = false;

        while let Ok(command) = next_command(&mut socket).await {
            assert!(command.get("sessionId").is_none());
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" | "Network.enable" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Browser.getVersion" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({ "userAgent": browser_user_agent }),
                    )
                    .await?;
                }
                "Emulation.setUserAgentOverride" => {
                    assert_eq!(command["params"]["userAgent"], browser_user_agent);
                    assert_eq!(command["params"]["acceptLanguage"], "fr-FR,fr;q=0.9");
                    saw_profile = true;
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Page.navigate" => {
                    assert_eq!(command["params"]["url"], "https://example.test/requested");
                    respond(&mut socket, &command, json!({})).await?;
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
                                    "finalUrl": final_url
                                }
                            }
                        }),
                    )
                    .await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }

        assert!(saw_profile);
        Ok(())
    })
    .await;

    let mut source = CdpSource::new(format!("{base_endpoint}/devtools/page/current"));
    source.url = Some(Url::parse("https://example.test/requested").unwrap());
    source.wait_until = CdpWaitUntil::None;
    let options = CaptureOptions {
        accept_language: Some("fr-FR,fr;q=0.9".to_owned()),
        ..CaptureOptions::default()
    };

    let capture = capture_cdp(&source, &options).await.unwrap();
    assert_capture(&capture, expected_html, final_url);
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn existing_caller_owned_target_is_detached_but_never_closed() {
    let expected_html = "<html><body>caller owned</body></html>";
    let final_url = "https://example.test/existing";
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        let mut saw_detach = false;
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
                "Target.getTargets" => {
                    respond(
                        &mut socket,
                        &command,
                        json!({
                            "targetInfos": [{
                                "targetId": "caller-target",
                                "type": "page",
                                "url": final_url
                            }]
                        }),
                    )
                    .await?;
                }
                "Target.attachToTarget" => {
                    assert_eq!(command["params"]["targetId"], "caller-target");
                    respond(
                        &mut socket,
                        &command,
                        json!({ "sessionId": "caller-session" }),
                    )
                    .await?;
                }
                "Page.enable" | "Runtime.enable" => {
                    assert_eq!(command["sessionId"], "caller-session");
                    respond(&mut socket, &command, json!({})).await?;
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
                                    "finalUrl": final_url
                                }
                            }
                        }),
                    )
                    .await?;
                }
                "Target.detachFromTarget" => {
                    assert_eq!(command["params"]["sessionId"], "caller-session");
                    assert!(command.get("sessionId").is_none());
                    saw_detach = true;
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Target.closeTarget" => {
                    saw_close = true;
                    respond(&mut socket, &command, json!({ "success": true })).await?;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }

        assert!(saw_detach);
        assert!(!saw_close);
        Ok(())
    })
    .await;

    let mut source = CdpSource::new(format!("{base_endpoint}/devtools/browser/mock"));
    source.target_id = Some("caller-target".to_owned());
    let capture = capture_cdp(&source, &CaptureOptions::default())
        .await
        .unwrap();

    assert_capture(&capture, expected_html, final_url);
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn direct_page_falls_back_to_dom_when_runtime_evaluation_is_rejected() {
    let expected_html = "<html><body>DOM fallback</body></html>";
    let final_url = "https://example.test/dom-fallback";
    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;

        while let Ok(command) = next_command(&mut socket).await {
            assert!(command.get("sessionId").is_none());
            match command["method"].as_str().unwrap_or_default() {
                "Page.enable" | "Runtime.enable" => {
                    respond(&mut socket, &command, json!({})).await?;
                }
                "Runtime.evaluate" => {
                    respond_with_error(&mut socket, &command, "Runtime domain unavailable").await?;
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
                            "entries": [{ "url": final_url }]
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
    let capture = capture_cdp(&source, &CaptureOptions::default())
        .await
        .unwrap();

    assert_capture(&capture, expected_html, final_url);
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn discovery_does_not_select_an_arbitrary_page_from_json_list() {
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
    let error = capture_cdp(&source, &CaptureOptions::default())
        .await
        .unwrap_err();

    assert!(matches!(error, ChromeError::CdpDiscovery));
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn public_error_redacts_remote_urls_and_tokens() {
    const REMOTE_SECRET: &str = "remote-token-do-not-publish";
    const ENDPOINT_SECRET: &str = "endpoint-token-do-not-publish";
    const PRIVATE_URL: &str = "https://private.example.test/account";

    let (base_endpoint, server) = websocket_server(move |stream| async move {
        let mut socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;
        let command = next_command(&mut socket).await?;
        assert_eq!(command["method"], "Page.enable");
        respond_with_error(
            &mut socket,
            &command,
            &format!("request to {PRIVATE_URL}?token={REMOTE_SECRET} was rejected"),
        )
        .await
    })
    .await;

    let source = CdpSource::new(format!(
        "{base_endpoint}/devtools/page/current?token={ENDPOINT_SECRET}"
    ));
    let error = capture_cdp(&source, &CaptureOptions::default())
        .await
        .unwrap_err();

    assert!(matches!(
        &error,
        ChromeError::CdpCommand {
            method: "Page.enable",
            ..
        }
    ));
    let public_text = format!("{error}\n{error:?}");
    assert!(!public_text.contains(REMOTE_SECRET));
    assert!(!public_text.contains(ENDPOINT_SECRET));
    assert!(!public_text.contains(PRIVATE_URL));
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn false_close_response_keeps_owned_target_cleanup_armed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        let mut capture_socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;

        while let Ok(command) = next_command(&mut capture_socket).await {
            match command["method"].as_str().unwrap_or_default() {
                "Browser.getVersion" => {
                    respond(
                        &mut capture_socket,
                        &command,
                        json!({ "userAgent": "MockChrome/1.0" }),
                    )
                    .await?;
                }
                "Target.createTarget" => {
                    respond(
                        &mut capture_socket,
                        &command,
                        json!({ "targetId": "retry-close-target" }),
                    )
                    .await?;
                }
                "Target.attachToTarget" => {
                    respond(
                        &mut capture_socket,
                        &command,
                        json!({ "sessionId": "retry-close-session" }),
                    )
                    .await?;
                }
                "Page.enable"
                | "Runtime.enable"
                | "Runtime.runIfWaitingForDebugger"
                | "Network.enable" => {
                    respond(&mut capture_socket, &command, json!({})).await?;
                }
                "Page.navigate" => {
                    respond(&mut capture_socket, &command, json!({})).await?;
                }
                "Runtime.evaluate" => {
                    respond(
                        &mut capture_socket,
                        &command,
                        json!({
                            "result": {
                                "type": "object",
                                "value": {
                                    "html": "<html><body>retry close</body></html>",
                                    "finalUrl": "https://example.test/retry-close"
                                }
                            }
                        }),
                    )
                    .await?;
                }
                "Target.closeTarget" => {
                    assert_eq!(command["params"]["targetId"], "retry-close-target");
                    respond(&mut capture_socket, &command, json!({ "success": false })).await?;
                    break;
                }
                other => return Err(format!("unexpected CDP command: {other}")),
            }
        }
        drop(capture_socket);

        let (cleanup_stream, _) = timeout(Duration::from_secs(2), listener.accept())
            .await
            .map_err(|_| "cleanup guard did not retry after closeTarget returned false".to_owned())?
            .map_err(|error| error.to_string())?;
        let mut cleanup_socket = accept_async(cleanup_stream)
            .await
            .map_err(|error| error.to_string())?;
        let command = next_command(&mut cleanup_socket).await?;
        assert_eq!(command["method"], "Target.closeTarget");
        assert_eq!(command["params"]["targetId"], "retry-close-target");
        respond(&mut cleanup_socket, &command, json!({ "success": true })).await?;
        Ok::<(), String>(())
    });

    let mut source = CdpSource::new(format!("ws://{address}/devtools/browser/mock"));
    source.url = Some(Url::parse("https://example.test/retry-close").unwrap());
    source.wait_until = CdpWaitUntil::None;

    let capture = capture_cdp(&source, &CaptureOptions::default())
        .await
        .unwrap();
    assert_capture(
        &capture,
        "<html><body>retry close</body></html>",
        "https://example.test/retry-close",
    );
    timeout(Duration::from_secs(3), server)
        .await
        .expect("cleanup retry server timed out")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn abort_after_target_creation_reconnects_and_closes_the_owned_target() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (attach_seen_tx, attach_seen_rx) = oneshot::channel();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        let mut capture_socket = accept_async(stream)
            .await
            .map_err(|error| error.to_string())?;

        let command = next_command(&mut capture_socket).await?;
        assert_eq!(command["method"], "Browser.getVersion");
        respond(
            &mut capture_socket,
            &command,
            json!({ "userAgent": "MockChrome/1.0" }),
        )
        .await?;

        let command = next_command(&mut capture_socket).await?;
        assert_eq!(command["method"], "Target.createTarget");
        respond(
            &mut capture_socket,
            &command,
            json!({ "targetId": "aborted-owned-target" }),
        )
        .await?;

        let command = next_command(&mut capture_socket).await?;
        assert_eq!(command["method"], "Target.attachToTarget");
        assert_eq!(command["params"]["targetId"], "aborted-owned-target");
        attach_seen_tx
            .send(())
            .map_err(|()| "capture task disappeared before attach was observed".to_owned())?;

        // Keep the original connection and attach command pending. Dropping the capture future
        // must use the owned-target guard to establish a fresh connection for cleanup.
        let (cleanup_stream, _) = timeout(Duration::from_secs(2), listener.accept())
            .await
            .map_err(|_| "cleanup guard did not reconnect".to_owned())?
            .map_err(|error| error.to_string())?;
        let mut cleanup_socket = accept_async(cleanup_stream)
            .await
            .map_err(|error| error.to_string())?;
        let command = next_command(&mut cleanup_socket).await?;
        assert_eq!(command["method"], "Target.closeTarget");
        assert_eq!(command["params"]["targetId"], "aborted-owned-target");
        respond(&mut cleanup_socket, &command, json!({ "success": true })).await?;
        Ok::<(), String>(())
    });

    let mut source = CdpSource::new(format!("ws://{address}/devtools/browser/mock"));
    source.url = Some(Url::parse("https://example.test/abort-after-create").unwrap());
    source.wait_until = CdpWaitUntil::None;
    let capture_task =
        tokio::spawn(async move { capture_cdp(&source, &CaptureOptions::default()).await });

    timeout(Duration::from_secs(2), attach_seen_rx)
        .await
        .expect("server did not observe Target.attachToTarget")
        .expect("server exited before observing Target.attachToTarget");
    capture_task.abort();
    assert!(capture_task.await.unwrap_err().is_cancelled());
    timeout(Duration::from_secs(3), server)
        .await
        .expect("cleanup server timed out")
        .unwrap()
        .unwrap();
}
