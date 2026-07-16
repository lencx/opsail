use std::collections::VecDeque;
use std::sync::Once;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use reqwest::redirect::Policy;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout_at};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};
use url::Url;

use crate::rendered;
use crate::{
    CaptureOptions, CapturedPage, CapturedResponse, CdpSource, CdpWaitUntil, ChromeError,
    RenderedProbe,
};

const DISCOVERY_MAX_BYTES: usize = 1024 * 1024;
const MAX_CDP_CAPTURE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CDP_MESSAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_EVENT_FIELD_BYTES: usize = 512;
const MAX_EVENT_URL_BYTES: usize = 16 * 1024;
const MAX_QUEUED_EVENTS: usize = 4_096;
const CLEANUP_TIMEOUT: Duration = Duration::from_millis(500);
const CAPTURE_EXPRESSION: &str = r#"(() => ({
    html: document.documentElement?.outerHTML ?? "",
    finalUrl: location.href
}))()"#;
const RENDER_OBSERVER_WORLD: &str = "opsail-render-observer";
const RENDER_OBSERVER_FUNCTION: &str = r#"function(probes) {
  const MAX_ELEMENTS_PER_PROBE = 32;
  const GRID_SIZE = 5;
  const visual = window.visualViewport;
  const viewport = {
    left: visual ? visual.offsetLeft : 0,
    top: visual ? visual.offsetTop : 0,
    width: visual ? visual.width : window.innerWidth,
    height: visual ? visual.height : window.innerHeight
  };
  const clampPerMille = value => Math.max(0, Math.min(1000, Math.round(value * 1000)));
  const intersect = (a, b) => {
    const left = Math.max(a.left, b.left);
    const top = Math.max(a.top, b.top);
    const right = Math.min(a.right, b.right);
    const bottom = Math.min(a.bottom, b.bottom);
    return { left, top, right, bottom, width: Math.max(0, right - left), height: Math.max(0, bottom - top) };
  };
  const viewportRect = {
    left: viewport.left,
    top: viewport.top,
    right: viewport.left + viewport.width,
    bottom: viewport.top + viewport.height,
    width: viewport.width,
    height: viewport.height
  };
  const measurement = element => {
    if (!(element instanceof Element) || !element.isConnected || viewport.width <= 0 || viewport.height <= 0) {
      return { visible: false, x: 0, y: 0, width: 0, height: 0, viewportCoverage: 0, hitCoverage: 0, position: "" };
    }
    let opacity = 1;
    for (let ancestor = element; ancestor; ancestor = ancestor.parentElement) {
      const style = getComputedStyle(ancestor);
      if (style.display === "none" || style.visibility === "hidden" || style.visibility === "collapse" || style.contentVisibility === "hidden") {
        return { visible: false, x: 0, y: 0, width: 0, height: 0, viewportCoverage: 0, hitCoverage: 0, position: "" };
      }
      const value = Number.parseFloat(style.opacity);
      if (Number.isFinite(value)) opacity *= value;
      if (opacity <= 0.01) {
        return { visible: false, x: 0, y: 0, width: 0, height: 0, viewportCoverage: 0, hitCoverage: 0, position: "" };
      }
    }
    const raw = element.getBoundingClientRect();
    let clipped = intersect(
      { left: raw.left, top: raw.top, right: raw.right, bottom: raw.bottom, width: raw.width, height: raw.height },
      viewportRect
    );
    for (let ancestor = element.parentElement; ancestor && clipped.width > 0 && clipped.height > 0; ancestor = ancestor.parentElement) {
      const style = getComputedStyle(ancestor);
      const clipsX = ["hidden", "clip", "scroll", "auto"].includes(style.overflowX);
      const clipsY = ["hidden", "clip", "scroll", "auto"].includes(style.overflowY);
      if (clipsX || clipsY) {
        const box = ancestor.getBoundingClientRect();
        const clip = {
          left: clipsX ? box.left : viewportRect.left,
          right: clipsX ? box.right : viewportRect.right,
          top: clipsY ? box.top : viewportRect.top,
          bottom: clipsY ? box.bottom : viewportRect.bottom
        };
        clipped = intersect(clipped, {
          ...clip,
          width: Math.max(0, clip.right - clip.left),
          height: Math.max(0, clip.bottom - clip.top)
        });
      }
    }
    const visible = raw.width > 0 && raw.height > 0 && clipped.width > 0 && clipped.height > 0;
    if (!visible) {
      return { visible: false, x: raw.left, y: raw.top, width: raw.width, height: raw.height, viewportCoverage: 0, hitCoverage: 0, position: getComputedStyle(element).position };
    }
    const viewportArea = viewport.width * viewport.height;
    let owned = 0;
    const samples = GRID_SIZE * GRID_SIZE;
    for (let row = 0; row < GRID_SIZE; row += 1) {
      for (let column = 0; column < GRID_SIZE; column += 1) {
        const x = viewport.left + viewport.width * (column + 0.5) / GRID_SIZE;
        const y = viewport.top + viewport.height * (row + 0.5) / GRID_SIZE;
        const hit = document.elementFromPoint(x, y);
        if (hit && (hit === element || element.contains(hit))) owned += 1;
      }
    }
    return {
      visible: true,
      x: raw.left,
      y: raw.top,
      width: raw.width,
      height: raw.height,
      viewportCoverage: clampPerMille((clipped.width * clipped.height) / viewportArea),
      hitCoverage: clampPerMille(owned / samples),
      position: getComputedStyle(element).position
    };
  };
  const score = surface => (surface.visible ? 1000000 : 0) + surface.viewportCoverage * 1000 + surface.hitCoverage;
  const sampleProbe = probe => {
    let matches = [];
    try { matches = Array.from(document.querySelectorAll(probe.selector)); } catch (_) { matches = []; }
    const bounded = matches.slice(0, MAX_ELEMENTS_PER_PROBE);
    let markerElement = null;
    let marker = null;
    let takeoverElement = null;
    let takeover = null;
    for (const element of bounded) {
      const measured = measurement(element);
      if (!marker || score(measured) > score(marker)) {
        markerElement = element;
        marker = measured;
      }
      for (let surface = element; surface && surface !== document.body && surface !== document.documentElement; surface = surface.parentElement) {
        const candidate = measurement(surface);
        const eligible = candidate.visible && (candidate.position === "fixed" || (candidate.position === "absolute" && candidate.viewportCoverage >= 800));
        if (eligible && (!takeover || score(candidate) > score(takeover))) {
          takeoverElement = surface;
          takeover = candidate;
        }
      }
    }
    return {
      id: probe.id,
      matches: Math.min(matches.length, 65535),
      markerElement,
      marker,
      takeoverElement,
      takeover
    };
  };
  const sample = () => probes.map(sampleProbe);
  const firstUrl = location.href;
  const first = sample();
  const stableSurface = (beforeElement, before, afterElement, after) => {
    if (!before || !after) return null;
    const stable = beforeElement === afterElement && firstUrl === location.href &&
      before.visible === after.visible && Math.abs(before.x - after.x) <= 1 &&
      Math.abs(before.y - after.y) <= 1 && Math.abs(before.width - after.width) <= 1 &&
      Math.abs(before.height - after.height) <= 1 &&
      Math.abs(before.hitCoverage - after.hitCoverage) <= 40;
    return {
      visible: after.visible,
      stable,
      viewportCoverage: after.viewportCoverage,
      hitCoverage: after.hitCoverage
    };
  };
  return new Promise(resolve => {
    let completed = false;
    const finish = timedOut => {
      if (completed) return;
      completed = true;
      const second = sample();
      const results = second.map((current, index) => {
        const previous = first[index];
        return {
          id: current.id,
          matches: current.matches,
          marker: stableSurface(previous.markerElement, previous.marker, current.markerElement, current.marker),
          takeover: stableSurface(previous.takeoverElement, previous.takeover, current.takeoverElement, current.takeover)
        };
      });
      resolve({
        html: document.documentElement ? document.documentElement.outerHTML : "",
        finalUrl: location.href,
        renderedEvidence: { timedOut, results }
      });
    };
    const timer = setTimeout(() => finish(true), 250);
    requestAnimationFrame(() => requestAnimationFrame(() => {
      clearTimeout(timer);
      finish(false);
    }));
  });
}"#;
static INSTALL_TLS_PROVIDER: Once = Once::new();

struct ResolvedEndpoint {
    url: Url,
    direct_page: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserAgentPolicy {
    Preserve,
    BrowserCompatible,
}

struct AttachedPage {
    session_id: Option<String>,
    target_id: Option<String>,
    owned_target: bool,
    owned_cleanup: Option<OwnedTargetCleanup>,
}

struct OwnedTargetCleanup {
    endpoint: Url,
    target_id: Option<String>,
}

impl OwnedTargetCleanup {
    fn new(endpoint: Url, target_id: String) -> Self {
        Self {
            endpoint,
            target_id: Some(target_id),
        }
    }

    fn disarm(&mut self) {
        self.target_id = None;
    }
}

impl Drop for OwnedTargetCleanup {
    fn drop(&mut self) {
        let Some(target_id) = self.target_id.take() else {
            return;
        };
        let endpoint = self.endpoint.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            close_target_at_endpoint(endpoint, target_id).await;
        });
    }
}

enum CdpEvent {
    Lifecycle(LifecycleEvent),
    DocumentResponse(DocumentResponseEvent),
}

struct LifecycleEvent {
    session_id: Option<String>,
    name: String,
    loader_id: String,
}

struct DocumentResponseEvent {
    session_id: Option<String>,
    loader_id: String,
    frame_id: String,
    url: Url,
    response: CapturedResponse,
}

#[derive(Clone, Eq, PartialEq)]
struct MainDocumentIdentity {
    frame_id: String,
    loader_id: String,
    url: Url,
}

type CdpSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct CdpConnection {
    socket: CdpSocket,
    next_id: u64,
    events: VecDeque<CdpEvent>,
    deadline: Instant,
}

pub(crate) async fn capture(
    source: &CdpSource,
    options: &CaptureOptions,
    user_agent_policy: UserAgentPolicy,
    probes: &[RenderedProbe],
) -> Result<CapturedPage, ChromeError> {
    let deadline = Instant::now()
        .checked_add(options.timeout)
        .ok_or(ChromeError::CdpTimeout)?;
    install_tls_provider();
    let endpoint = resolve_endpoint(source, options, deadline).await?;
    if endpoint.direct_page && source.target_id.is_some() {
        return Err(ChromeError::CdpTargetNotFound);
    }
    let capture_limit = options.max_bytes.min(MAX_CDP_CAPTURE_BYTES);
    let max_message_size = capture_limit
        .saturating_mul(6)
        .saturating_add(DISCOVERY_MAX_BYTES)
        .min(MAX_CDP_MESSAGE_BYTES);
    let mut connection = CdpConnection::connect(
        &endpoint.url,
        max_message_size,
        options.connect_timeout,
        deadline,
    )
    .await?;

    let (mut page, result) = if endpoint.direct_page {
        let page = AttachedPage {
            session_id: None,
            target_id: None,
            owned_target: false,
            owned_cleanup: None,
        };
        let result = capture_page(
            &mut connection,
            source,
            options,
            user_agent_policy,
            None,
            false,
            probes,
        )
        .await;
        (page, result)
    } else {
        connection.command("Browser.getVersion", None, None).await?;
        let page = attach_page(
            &mut connection,
            source,
            &endpoint.url,
            user_agent_policy == UserAgentPolicy::Preserve,
        )
        .await?;
        let result = capture_page(
            &mut connection,
            source,
            options,
            user_agent_policy,
            page.session_id.as_deref(),
            page.owned_target,
            probes,
        )
        .await;
        (page, result)
    };

    connection.deadline = Instant::now() + CLEANUP_TIMEOUT;
    cleanup_page(&mut connection, &mut page).await;
    let _ = timeout_at(connection.deadline, connection.socket.close(None)).await;

    let captured = result?;
    if captured.html.len() > capture_limit {
        return Err(ChromeError::CaptureTooLarge {
            limit: capture_limit,
        });
    }
    Ok(captured)
}

pub(crate) async fn close_browser(endpoint: &str, timeout: Duration) {
    let Ok(endpoint) = Url::parse(endpoint) else {
        return;
    };
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return;
    };
    let Ok(mut connection) =
        CdpConnection::connect(&endpoint, DISCOVERY_MAX_BYTES, timeout, deadline).await
    else {
        return;
    };
    let _ = connection.command("Browser.close", None, None).await;
    let _ = timeout_at(deadline, connection.socket.close(None)).await;
}

async fn attach_page(
    connection: &mut CdpConnection,
    source: &CdpSource,
    endpoint: &Url,
    create_in_background: bool,
) -> Result<AttachedPage, ChromeError> {
    let (target_id, owned_target) = if source.url.is_some() && source.target_id.is_none() {
        let result = connection
            .command(
                "Target.createTarget",
                Some(json!({
                    "url": "about:blank",
                    "background": create_in_background
                })),
                None,
            )
            .await?;
        (
            required_string(&result, "targetId", "Target.createTarget")?,
            true,
        )
    } else {
        (
            select_target(connection, source.target_id.as_deref()).await?,
            false,
        )
    };
    let mut owned_cleanup =
        owned_target.then(|| OwnedTargetCleanup::new(endpoint.clone(), target_id.clone()));

    let result = connection
        .command(
            "Target.attachToTarget",
            Some(json!({ "targetId": target_id, "flatten": true })),
            None,
        )
        .await;

    match result {
        Ok(result) => match required_string(&result, "sessionId", "Target.attachToTarget") {
            Ok(session_id) => Ok(AttachedPage {
                session_id: Some(session_id),
                target_id: Some(target_id),
                owned_target,
                owned_cleanup,
            }),
            Err(error) => {
                if owned_target {
                    close_owned_target(connection, &target_id, &mut owned_cleanup).await;
                }
                Err(error)
            }
        },
        Err(error) => {
            if owned_target {
                close_owned_target(connection, &target_id, &mut owned_cleanup).await;
            }
            Err(error)
        }
    }
}

async fn select_target(
    connection: &mut CdpConnection,
    requested_target_id: Option<&str>,
) -> Result<String, ChromeError> {
    let result = connection.command("Target.getTargets", None, None).await?;
    let targets = result
        .get("targetInfos")
        .and_then(Value::as_array)
        .ok_or_else(|| command_shape_error("Target.getTargets"))?;

    let is_page = |target: &&Value| {
        matches!(
            target.get("type").and_then(Value::as_str),
            Some("page" | "webview")
        )
    };
    let selected = match requested_target_id {
        Some(target_id) => targets
            .iter()
            .find(|target| {
                is_page(target) && target.get("targetId").and_then(Value::as_str) == Some(target_id)
            })
            .ok_or(ChromeError::CdpTargetNotFound)?,
        None => {
            let mut pages = targets.iter().filter(is_page);
            let selected = pages.next().ok_or(ChromeError::CdpTargetNotFound)?;
            if pages.next().is_some() {
                return Err(ChromeError::CdpTargetAmbiguous);
            }
            selected
        }
    };

    selected
        .get("targetId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| command_shape_error("Target.getTargets"))
}

async fn capture_page(
    connection: &mut CdpConnection,
    source: &CdpSource,
    options: &CaptureOptions,
    user_agent_policy: UserAgentPolicy,
    session_id: Option<&str>,
    resume_if_waiting: bool,
    probes: &[RenderedProbe],
) -> Result<CapturedPage, ChromeError> {
    connection.command("Page.enable", None, session_id).await?;
    connection
        .command("Runtime.enable", None, session_id)
        .await?;
    let mut network_enabled_for_navigation = false;
    if source.url.is_some() && source.wait_until != CdpWaitUntil::None {
        connection
            .command(
                "Page.setLifecycleEventsEnabled",
                Some(json!({ "enabled": true })),
                session_id,
            )
            .await?;
    }
    if resume_if_waiting {
        let _ = connection
            .command("Runtime.runIfWaitingForDebugger", None, session_id)
            .await;
    }

    if let Some(url) = source.url.as_ref() {
        // Some hosted CDP endpoints intentionally restrict the Network domain.
        // Capture remains useful without it, so a rejected enable command only
        // disables authoritative response metadata for this navigation.
        let network_enabled = connection
            .command("Network.enable", None, session_id)
            .await
            .is_ok();
        network_enabled_for_navigation = network_enabled;
        apply_request_profile(connection, options, user_agent_policy, session_id).await?;
        connection.events.clear();
        let result = connection
            .command(
                "Page.navigate",
                Some(json!({ "url": url.as_str() })),
                session_id,
            )
            .await?;
        if result.get("errorText").and_then(Value::as_str).is_some() {
            return Err(ChromeError::CdpNavigation(
                "the browser reported a navigation error".to_owned(),
            ));
        }
        if source.wait_until != CdpWaitUntil::None
            && let Some(loader_id) = result
                .get("loaderId")
                .and_then(Value::as_str)
                .and_then(bounded_event_field)
        {
            wait_for_page(connection, source.wait_until, &loader_id, session_id).await?;
        }
    }

    let observed = if probes.is_empty() {
        None
    } else {
        capture_observed_consistently(connection, session_id, probes)
            .await
            .ok()
            .flatten()
    };
    let (mut captured, observed_identity) = match observed {
        Some(observed) => (observed.page, Some(observed.identity)),
        None => {
            let captured = capture_page_html(connection, session_id).await?;
            (captured, None)
        }
    };
    if network_enabled_for_navigation && connection.has_document_response() {
        let identity = match observed_identity {
            Some(identity) => Some(identity),
            None => current_main_document(connection, session_id).await.ok(),
        };
        if let Some(identity) = identity
            && urls_equal_ignoring_fragment(&identity.url, &captured.final_url)
        {
            captured.response = connection.take_document_response(&identity, session_id);
        }
    }
    Ok(captured)
}

struct ObservedCapture {
    page: CapturedPage,
    identity: MainDocumentIdentity,
}

async fn capture_observed_consistently(
    connection: &mut CdpConnection,
    session_id: Option<&str>,
    probes: &[RenderedProbe],
) -> Result<Option<ObservedCapture>, ChromeError> {
    for _ in 0..2 {
        let before = current_main_document(connection, session_id).await?;
        let page =
            capture_runtime_with_probes(connection, session_id, &before.frame_id, probes).await?;
        let after = current_main_document(connection, session_id).await?;
        if before == after && urls_equal_ignoring_fragment(&after.url, &page.final_url) {
            return Ok(Some(ObservedCapture {
                page,
                identity: after,
            }));
        }
    }
    Ok(None)
}

async fn capture_page_html(
    connection: &mut CdpConnection,
    session_id: Option<&str>,
) -> Result<CapturedPage, ChromeError> {
    match capture_runtime(connection, session_id).await {
        Ok(captured) => Ok(captured),
        Err(ChromeError::InvalidCdpCapture | ChromeError::CdpCommand { .. }) => {
            capture_dom(connection, session_id).await
        }
        Err(error) => Err(error),
    }
}

async fn current_main_document(
    connection: &mut CdpConnection,
    session_id: Option<&str>,
) -> Result<MainDocumentIdentity, ChromeError> {
    let frame_tree = connection
        .command("Page.getFrameTree", None, session_id)
        .await?;
    let frame = frame_tree
        .pointer("/frameTree/frame")
        .ok_or_else(|| command_shape_error("Page.getFrameTree"))?;
    let frame_id = required_string(frame, "id", "Page.getFrameTree")?;
    let loader_id = required_string(frame, "loaderId", "Page.getFrameTree")?;
    let url = required_string(frame, "url", "Page.getFrameTree")?;
    if frame_id.len() > MAX_EVENT_FIELD_BYTES
        || loader_id.len() > MAX_EVENT_FIELD_BYTES
        || url.len() > MAX_EVENT_URL_BYTES
    {
        return Err(command_shape_error("Page.getFrameTree"));
    }
    let url = Url::parse(&url).map_err(|_| command_shape_error("Page.getFrameTree"))?;
    Ok(MainDocumentIdentity {
        frame_id,
        loader_id,
        url,
    })
}

async fn apply_request_profile(
    connection: &mut CdpConnection,
    options: &CaptureOptions,
    user_agent_policy: UserAgentPolicy,
    session_id: Option<&str>,
) -> Result<(), ChromeError> {
    if options.user_agent.is_none()
        && options.accept_language.is_none()
        && user_agent_policy == UserAgentPolicy::Preserve
    {
        return Ok(());
    }

    let user_agent = match options.user_agent.as_deref() {
        Some(value) => Some(value.to_owned()),
        None => {
            let result = connection.command("Browser.getVersion", None, None).await?;
            let current = required_string(&result, "userAgent", "Browser.getVersion")?;
            match user_agent_policy {
                UserAgentPolicy::Preserve => Some(current),
                UserAgentPolicy::BrowserCompatible => browser_compatible_user_agent(&current)
                    .or_else(|| options.accept_language.is_some().then_some(current)),
            }
        }
    };
    let Some(user_agent) = user_agent else {
        return Ok(());
    };
    let mut params = json!({ "userAgent": user_agent });
    if let Some(language) = options.accept_language.as_deref() {
        params["acceptLanguage"] = Value::String(language.to_owned());
    }
    connection
        .command("Emulation.setUserAgentOverride", Some(params), session_id)
        .await?;
    Ok(())
}

fn browser_compatible_user_agent(user_agent: &str) -> Option<String> {
    let mut changed = false;
    let mut normalized = String::with_capacity(user_agent.len());
    for (index, product) in user_agent.split(' ').enumerate() {
        if index > 0 {
            normalized.push(' ');
        }
        if let Some(version) = product.strip_prefix("HeadlessChrome/")
            && !version.is_empty()
        {
            normalized.push_str("Chrome/");
            normalized.push_str(version);
            changed = true;
        } else {
            normalized.push_str(product);
        }
    }
    changed.then_some(normalized)
}

async fn wait_for_page(
    connection: &mut CdpConnection,
    wait_until: CdpWaitUntil,
    loader_id: &str,
    session_id: Option<&str>,
) -> Result<(), ChromeError> {
    let name = match wait_until {
        CdpWaitUntil::None => return Ok(()),
        CdpWaitUntil::DomContentLoaded => "DOMContentLoaded",
        CdpWaitUntil::Load => "load",
        CdpWaitUntil::NetworkIdle => "networkIdle",
    };
    connection
        .wait_for_lifecycle(name, loader_id, session_id)
        .await
}

async fn capture_runtime(
    connection: &mut CdpConnection,
    session_id: Option<&str>,
) -> Result<CapturedPage, ChromeError> {
    let result = connection
        .command(
            "Runtime.evaluate",
            Some(json!({
                "expression": CAPTURE_EXPRESSION,
                "returnByValue": true,
                "awaitPromise": true
            })),
            session_id,
        )
        .await?;
    if result.get("exceptionDetails").is_some() {
        return Err(ChromeError::InvalidCdpCapture);
    }
    let value = result
        .pointer("/result/value")
        .ok_or(ChromeError::InvalidCdpCapture)?;
    capture_from_values(
        value.get("html").and_then(Value::as_str),
        value.get("finalUrl").and_then(Value::as_str),
    )
}

async fn capture_runtime_with_probes(
    connection: &mut CdpConnection,
    session_id: Option<&str>,
    frame_id: &str,
    probes: &[RenderedProbe],
) -> Result<CapturedPage, ChromeError> {
    let world = connection
        .command(
            "Page.createIsolatedWorld",
            Some(json!({
                "frameId": frame_id,
                "worldName": RENDER_OBSERVER_WORLD,
                "grantUniveralAccess": false
            })),
            session_id,
        )
        .await?;
    let context_id = world
        .get("executionContextId")
        .and_then(Value::as_u64)
        .ok_or_else(|| command_shape_error("Page.createIsolatedWorld"))?;
    // A caller-managed browser may keep Opsail's temporary target in the
    // background, where requestAnimationFrame is suspended. Focus emulation
    // makes the document active without bringing the user's browser window to
    // the foreground. Older/restricted endpoints may reject this experimental
    // command, so it is deliberately best effort.
    let _ = connection
        .command(
            "Emulation.setFocusEmulationEnabled",
            Some(json!({ "enabled": true })),
            session_id,
        )
        .await;
    let requested = probes
        .iter()
        .map(|probe| json!({ "id": probe.id(), "selector": probe.selector() }))
        .collect::<Vec<_>>();
    let result = connection
        .command(
            "Runtime.callFunctionOn",
            Some(json!({
                "functionDeclaration": RENDER_OBSERVER_FUNCTION,
                "executionContextId": context_id,
                "arguments": [{ "value": requested }],
                "returnByValue": true,
                "awaitPromise": true
            })),
            session_id,
        )
        .await?;
    if result.get("exceptionDetails").is_some() {
        return Err(ChromeError::InvalidCdpCapture);
    }
    let value = result
        .pointer("/result/value")
        .ok_or(ChromeError::InvalidCdpCapture)?;
    let mut captured = capture_from_values(
        value.get("html").and_then(Value::as_str),
        value.get("finalUrl").and_then(Value::as_str),
    )?;
    captured.rendered_evidence = value
        .get("renderedEvidence")
        .and_then(|value| rendered::parse_evidence(value, probes));
    Ok(captured)
}

async fn capture_dom(
    connection: &mut CdpConnection,
    session_id: Option<&str>,
) -> Result<CapturedPage, ChromeError> {
    let document = connection
        .command(
            "DOM.getDocument",
            Some(json!({ "depth": 0, "pierce": false })),
            session_id,
        )
        .await?;
    let node_id = document
        .pointer("/root/nodeId")
        .and_then(Value::as_u64)
        .ok_or(ChromeError::InvalidCdpCapture)?;
    let outer = connection
        .command(
            "DOM.getOuterHTML",
            Some(json!({ "nodeId": node_id })),
            session_id,
        )
        .await?;
    let history = connection
        .command("Page.getNavigationHistory", None, session_id)
        .await?;
    let index = history
        .get("currentIndex")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(ChromeError::InvalidCdpCapture)?;
    let final_url = history
        .get("entries")
        .and_then(Value::as_array)
        .and_then(|entries| entries.get(index))
        .and_then(|entry| entry.get("url"))
        .and_then(Value::as_str);
    capture_from_values(outer.get("outerHTML").and_then(Value::as_str), final_url)
}

fn capture_from_values(
    html: Option<&str>,
    final_url: Option<&str>,
) -> Result<CapturedPage, ChromeError> {
    let html = html
        .filter(|value| !value.is_empty())
        .ok_or(ChromeError::InvalidCdpCapture)?;
    let final_url = final_url
        .and_then(|value| Url::parse(value).ok())
        .ok_or(ChromeError::InvalidCdpCapture)?;
    Ok(CapturedPage {
        html: html.to_owned(),
        final_url,
        response: None,
        rendered_evidence: None,
    })
}

async fn cleanup_page(connection: &mut CdpConnection, page: &mut AttachedPage) {
    if page.owned_target {
        if let Some(target_id) = page.target_id.as_deref() {
            close_owned_target(connection, target_id, &mut page.owned_cleanup).await;
        }
    } else if let Some(session_id) = page.session_id.as_deref() {
        let _ = connection
            .command(
                "Target.detachFromTarget",
                Some(json!({ "sessionId": session_id })),
                None,
            )
            .await;
    }
}

async fn close_owned_target(
    connection: &mut CdpConnection,
    target_id: &str,
    cleanup: &mut Option<OwnedTargetCleanup>,
) {
    if best_effort_close_target(connection, target_id).await
        && let Some(cleanup) = cleanup
    {
        cleanup.disarm();
    }
}

async fn best_effort_close_target(connection: &mut CdpConnection, target_id: &str) -> bool {
    connection.deadline = Instant::now() + CLEANUP_TIMEOUT;
    let Ok(result) = connection
        .command(
            "Target.closeTarget",
            Some(json!({ "targetId": target_id })),
            None,
        )
        .await
    else {
        return false;
    };
    result.get("success").and_then(Value::as_bool) == Some(true)
}

async fn close_target_at_endpoint(endpoint: Url, target_id: String) {
    let Some(deadline) = Instant::now().checked_add(CLEANUP_TIMEOUT) else {
        return;
    };
    let Ok(mut connection) =
        CdpConnection::connect(&endpoint, DISCOVERY_MAX_BYTES, CLEANUP_TIMEOUT, deadline).await
    else {
        return;
    };
    let _ = connection
        .command(
            "Target.closeTarget",
            Some(json!({ "targetId": target_id })),
            None,
        )
        .await;
    let _ = timeout_at(deadline, connection.socket.close(None)).await;
}

impl CdpConnection {
    async fn connect(
        endpoint: &Url,
        max_message_size: usize,
        connect_timeout: Duration,
        deadline: Instant,
    ) -> Result<Self, ChromeError> {
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(max_message_size);
        config.max_frame_size = Some(max_message_size);
        let connect = connect_async_with_config(endpoint.as_str(), Some(config), false);
        let connect_deadline = Instant::now()
            .checked_add(connect_timeout)
            .map_or(deadline, |value| value.min(deadline));
        let (socket, _) = timeout_at(connect_deadline, connect)
            .await
            .map_err(|_| ChromeError::CdpTimeout)?
            .map_err(|_| ChromeError::CdpConnection)?;
        Ok(Self {
            socket,
            next_id: 1,
            events: VecDeque::new(),
            deadline,
        })
    }

    async fn command(
        &mut self,
        method: &'static str,
        params: Option<Value>,
        session_id: Option<&str>,
    ) -> Result<Value, ChromeError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut command = json!({ "id": id, "method": method });
        if let Some(params) = params {
            command["params"] = params;
        }
        if let Some(session_id) = session_id {
            command["sessionId"] = Value::String(session_id.to_owned());
        }
        let message = serde_json::to_string(&command).map_err(|_| ChromeError::CdpCommand {
            method,
            message: "could not serialize the command".to_owned(),
        })?;
        timeout_at(
            self.deadline,
            self.socket.send(Message::Text(message.into())),
        )
        .await
        .map_err(|_| ChromeError::CdpTimeout)?
        .map_err(|_| ChromeError::CdpConnection)?;

        loop {
            let value = self.next_json().await?;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                if value.get("error").is_some() {
                    return Err(ChromeError::CdpCommand {
                        method,
                        message: "the endpoint rejected the command".to_owned(),
                    });
                }
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
            if let Some(event) = parse_event(&value) {
                self.push_event(event);
            }
        }
    }

    async fn wait_for_lifecycle(
        &mut self,
        name: &str,
        loader_id: &str,
        session_id: Option<&str>,
    ) -> Result<(), ChromeError> {
        loop {
            if let Some(index) = self
                .events
                .iter()
                .position(|event| lifecycle_matches(event, name, loader_id, session_id))
            {
                self.events.remove(index);
                return Ok(());
            }
            let event = self.next_event().await?;
            if lifecycle_matches(&event, name, loader_id, session_id) {
                return Ok(());
            }
            self.push_event(event);
        }
    }

    fn take_document_response(
        &mut self,
        identity: &MainDocumentIdentity,
        session_id: Option<&str>,
    ) -> Option<CapturedResponse> {
        let index = self.events.iter().rposition(|event| {
            matches!(
                event,
                CdpEvent::DocumentResponse(event)
                    if session_matches(session_id, event.session_id.as_deref())
                        && response_belongs_to_capture(event, identity)
            )
        })?;
        match self.events.remove(index)? {
            CdpEvent::DocumentResponse(event) => Some(event.response),
            CdpEvent::Lifecycle(_) => None,
        }
    }

    fn has_document_response(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event, CdpEvent::DocumentResponse(_)))
    }

    async fn next_event(&mut self) -> Result<CdpEvent, ChromeError> {
        loop {
            let value = self.next_json().await?;
            if let Some(event) = parse_event(&value) {
                return Ok(event);
            }
        }
    }

    async fn next_json(&mut self) -> Result<Value, ChromeError> {
        loop {
            let next = timeout_at(self.deadline, self.socket.next())
                .await
                .map_err(|_| ChromeError::CdpTimeout)?
                .ok_or(ChromeError::CdpConnection)?
                .map_err(|_| ChromeError::CdpConnection)?;
            let parsed = match next {
                Message::Text(text) => serde_json::from_str(text.as_ref()).ok(),
                Message::Binary(bytes) => serde_json::from_slice(bytes.as_ref()).ok(),
                Message::Ping(payload) => {
                    timeout_at(self.deadline, self.socket.send(Message::Pong(payload)))
                        .await
                        .map_err(|_| ChromeError::CdpTimeout)?
                        .map_err(|_| ChromeError::CdpConnection)?;
                    None
                }
                Message::Close(_) => return Err(ChromeError::CdpConnection),
                Message::Pong(_) | Message::Frame(_) => None,
            };
            if let Some(value) = parsed {
                return Ok(value);
            }
        }
    }

    fn push_event(&mut self, event: CdpEvent) {
        if self.events.len() == MAX_QUEUED_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
}

fn response_belongs_to_capture(
    event: &DocumentResponseEvent,
    identity: &MainDocumentIdentity,
) -> bool {
    event.frame_id == identity.frame_id
        && event.loader_id == identity.loader_id
        && urls_equal_ignoring_fragment(&event.url, &identity.url)
}

fn urls_equal_ignoring_fragment(left: &Url, right: &Url) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.set_fragment(None);
    right.set_fragment(None);
    left == right
}

fn parse_event(value: &Value) -> Option<CdpEvent> {
    let method = value.get("method")?.as_str()?;
    let session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .and_then(bounded_event_field);
    match method {
        "Page.lifecycleEvent" => Some(CdpEvent::Lifecycle(LifecycleEvent {
            session_id,
            name: value
                .pointer("/params/name")
                .and_then(Value::as_str)
                .and_then(bounded_event_field)?,
            loader_id: value
                .pointer("/params/loaderId")
                .and_then(Value::as_str)
                .and_then(bounded_event_field)?,
        })),
        "Network.responseReceived"
            if value.pointer("/params/type").and_then(Value::as_str) == Some("Document") =>
        {
            let response = value.pointer("/params/response")?;
            let status = response.get("status").and_then(cdp_status_code)?;
            let headers = response.get("headers").and_then(Value::as_object);
            Some(CdpEvent::DocumentResponse(DocumentResponseEvent {
                session_id,
                loader_id: value
                    .pointer("/params/loaderId")
                    .and_then(Value::as_str)
                    .and_then(bounded_event_field)?,
                frame_id: value
                    .pointer("/params/frameId")
                    .and_then(Value::as_str)
                    .and_then(bounded_event_field)?,
                url: response
                    .get("url")
                    .and_then(Value::as_str)
                    .filter(|url| url.len() <= MAX_EVENT_URL_BYTES)
                    .and_then(|url| Url::parse(url).ok())?,
                response: CapturedResponse::new(
                    status,
                    retained_header(headers, "cf-mitigated"),
                    retained_header(headers, "x-amzn-waf-action"),
                ),
            }))
        }
        _ => None,
    }
}

fn lifecycle_matches(
    event: &CdpEvent,
    name: &str,
    loader_id: &str,
    session_id: Option<&str>,
) -> bool {
    matches!(
        event,
        CdpEvent::Lifecycle(event)
            if event.name == name
                && event.loader_id == loader_id
                && session_matches(session_id, event.session_id.as_deref())
    )
}

fn cdp_status_code(value: &Value) -> Option<u16> {
    if let Some(status) = value.as_u64() {
        return u16::try_from(status).ok();
    }
    let status = value.as_f64()?;
    (status.is_finite() && status.fract() == 0.0 && status >= 0.0 && status <= u16::MAX as f64)
        .then_some(status as u16)
}

fn retained_header(
    headers: Option<&serde_json::Map<String, Value>>,
    requested: &str,
) -> Option<String> {
    let value = headers?.iter().find_map(|(name, value)| {
        name.eq_ignore_ascii_case(requested)
            .then(|| value.as_str())
            .flatten()
    })?;
    (value.len() <= MAX_EVENT_FIELD_BYTES).then(|| value.to_owned())
}

fn bounded_event_field(value: &str) -> Option<String> {
    (value.len() <= MAX_EVENT_FIELD_BYTES).then(|| value.to_owned())
}

fn session_matches(expected: Option<&str>, actual: Option<&str>) -> bool {
    match expected {
        Some(expected) => actual == Some(expected),
        None => actual.is_none(),
    }
}

async fn resolve_endpoint(
    source: &CdpSource,
    options: &CaptureOptions,
    deadline: Instant,
) -> Result<ResolvedEndpoint, ChromeError> {
    if let Ok(port) = source.endpoint.parse::<u16>() {
        if port == 0 {
            return Err(ChromeError::InvalidCdpEndpoint);
        }
        let url = Url::parse(&format!("http://127.0.0.1:{port}/"))
            .map_err(|_| ChromeError::InvalidCdpEndpoint)?;
        return discover_endpoint(url, source.direct_page, options, deadline).await;
    }

    let mut endpoint = Url::parse(&source.endpoint).map_err(|_| ChromeError::InvalidCdpEndpoint)?;
    if endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(ChromeError::InvalidCdpEndpoint);
    }

    match endpoint.scheme() {
        "ws" | "wss" => {
            let direct_page = source.direct_page || is_page_endpoint(&endpoint);
            Ok(ResolvedEndpoint {
                url: endpoint,
                direct_page,
            })
        }
        "http" | "https" if endpoint.path().contains("/devtools/") => {
            let scheme = if endpoint.scheme() == "https" {
                "wss"
            } else {
                "ws"
            };
            endpoint
                .set_scheme(scheme)
                .map_err(|()| ChromeError::InvalidCdpEndpoint)?;
            let direct_page = source.direct_page || is_page_endpoint(&endpoint);
            Ok(ResolvedEndpoint {
                url: endpoint,
                direct_page,
            })
        }
        "http" | "https" => {
            discover_endpoint(endpoint, source.direct_page, options, deadline).await
        }
        _ => Err(ChromeError::InvalidCdpEndpoint),
    }
}

async fn discover_endpoint(
    endpoint: Url,
    direct_page: bool,
    options: &CaptureOptions,
    deadline: Instant,
) -> Result<ResolvedEndpoint, ChromeError> {
    let client = reqwest::Client::builder()
        .connect_timeout(options.connect_timeout)
        .redirect(Policy::none())
        .build()
        .map_err(|_| ChromeError::CdpDiscovery)?;

    let mut version_url = endpoint.clone();
    version_url.set_path("/json/version");
    let mut discovered = match fetch_discovery_json(&client, version_url, deadline).await {
        Ok(value) => value
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .map(str::to_owned),
        Err(ChromeError::CdpTimeout) => return Err(ChromeError::CdpTimeout),
        Err(_) => None,
    };

    if discovered.is_none() {
        let mut list_url = endpoint.clone();
        list_url.set_path("/json/list");
        discovered = match fetch_discovery_json(&client, list_url, deadline).await {
            Ok(value) => value.as_array().and_then(|targets| {
                targets
                    .iter()
                    .find(|target| target.get("type").and_then(Value::as_str) == Some("browser"))
                    .and_then(|target| target.get("webSocketDebuggerUrl"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }),
            Err(ChromeError::CdpTimeout) => return Err(ChromeError::CdpTimeout),
            Err(_) => None,
        };
    }

    let discovered = discovered.ok_or(ChromeError::CdpDiscovery)?;
    let url = normalize_discovered_url(&discovered, &endpoint)?;
    Ok(ResolvedEndpoint {
        direct_page: direct_page || is_page_endpoint(&url),
        url,
    })
}

async fn fetch_discovery_json(
    client: &reqwest::Client,
    url: Url,
    deadline: Instant,
) -> Result<Value, ChromeError> {
    let response = timeout_at(deadline, client.get(url).send())
        .await
        .map_err(|_| ChromeError::CdpTimeout)?
        .map_err(|_| ChromeError::CdpDiscovery)?;
    if !response.status().is_success() {
        return Err(ChromeError::CdpDiscovery);
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = timeout_at(deadline, stream.next())
        .await
        .map_err(|_| ChromeError::CdpTimeout)?
    {
        let chunk = chunk.map_err(|_| ChromeError::CdpDiscovery)?;
        if bytes.len().saturating_add(chunk.len()) > DISCOVERY_MAX_BYTES {
            return Err(ChromeError::CdpDiscovery);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| ChromeError::CdpDiscovery)
}

fn normalize_discovered_url(value: &str, endpoint: &Url) -> Result<Url, ChromeError> {
    let mut url = Url::parse(value).map_err(|_| ChromeError::CdpDiscovery)?;
    if !matches!(url.scheme(), "ws" | "wss")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(ChromeError::CdpDiscovery);
    }
    let host = endpoint.host_str().ok_or(ChromeError::CdpDiscovery)?;
    url.set_host(Some(host))
        .map_err(|_| ChromeError::CdpDiscovery)?;
    url.set_port(endpoint.port())
        .map_err(|()| ChromeError::CdpDiscovery)?;
    url.set_scheme(if endpoint.scheme() == "https" {
        "wss"
    } else {
        "ws"
    })
    .map_err(|()| ChromeError::CdpDiscovery)?;
    if url.query().is_none()
        && let Some(query) = endpoint.query()
    {
        url.set_query(Some(query));
    } else if let Some(query) = endpoint.query()
        && url.query() != Some(query)
    {
        let pairs = url::form_urlencoded::parse(query.as_bytes());
        url.query_pairs_mut().extend_pairs(pairs);
    }
    Ok(url)
}

fn is_page_endpoint(url: &Url) -> bool {
    url.path().contains("/devtools/page/")
}

fn required_string(
    value: &Value,
    field: &str,
    method: &'static str,
) -> Result<String, ChromeError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| command_shape_error(method))
}

fn command_shape_error(method: &'static str) -> ChromeError {
    ChromeError::CdpCommand {
        method,
        message: "the endpoint returned an invalid response".to_owned(),
    }
}

fn install_tls_provider() {
    INSTALL_TLS_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_only_the_headless_chrome_product_in_an_automatic_user_agent() {
        assert_eq!(
            browser_compatible_user_agent(
                "Mozilla/5.0 AppleWebKit/537.36 HeadlessChrome/150.0.0.0 Safari/537.36"
            ),
            Some("Mozilla/5.0 AppleWebKit/537.36 Chrome/150.0.0.0 Safari/537.36".to_owned())
        );
        assert_eq!(
            browser_compatible_user_agent(
                "Mozilla/5.0 AppleWebKit/537.36 Chrome/150.0.0.0 Safari/537.36"
            ),
            None
        );
        assert_eq!(
            browser_compatible_user_agent("Mozilla/5.0 NotHeadlessChrome/150.0.0.0 Safari/537.36"),
            None
        );
    }

    #[test]
    fn parses_only_privacy_bounded_main_document_response_metadata() {
        let event = parse_event(&json!({
            "method": "Network.responseReceived",
            "sessionId": "session-1",
            "params": {
                "loaderId": "loader-1",
                "frameId": "frame-1",
                "type": "Document",
                "response": {
                    "status": 403,
                    "url": "https://example.test/protected",
                    "headers": {
                        "CF-Mitigated": "challenge",
                        "x-amzn-waf-action": "captcha",
                        "set-cookie": "session=must-not-be-retained"
                    }
                }
            }
        }))
        .expect("document response should be recognized");

        let CdpEvent::DocumentResponse(event) = event else {
            panic!("expected document response event");
        };
        assert_eq!(event.loader_id, "loader-1");
        assert_eq!(event.frame_id, "frame-1");
        assert_eq!(event.url.as_str(), "https://example.test/protected");
        assert_eq!(event.response.status(), 403);
        assert_eq!(event.response.header("cf-mitigated"), Some("challenge"));
        assert_eq!(event.response.header("X-Amzn-Waf-Action"), Some("captcha"));
        assert_eq!(event.response.header("set-cookie"), None);
        assert!(!format!("{:?}", event.response).contains("session="));
    }

    #[test]
    fn ignores_subresource_response_metadata() {
        assert!(
            parse_event(&json!({
                "method": "Network.responseReceived",
                "params": {
                    "loaderId": "loader-1",
                    "type": "Script",
                    "response": {
                        "status": 403,
                        "headers": { "cf-mitigated": "challenge" }
                    }
                }
            }))
            .is_none()
        );
    }
}
