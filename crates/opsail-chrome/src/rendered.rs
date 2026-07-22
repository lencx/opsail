use std::collections::HashSet;
use std::fmt;

use serde_json::Value;

use crate::ChromeError;

pub(crate) const MAX_RENDERED_PROBES: usize = 16;
const MAX_SELECTOR_BYTES: usize = 256;
const MAX_MATCHES: u64 = u16::MAX as u64;
const MAX_PER_MILLE: u64 = 1_000;

/// A bounded CSS selector that Chrome can observe without executing caller
/// supplied JavaScript.
#[derive(Clone)]
pub struct RenderedProbe {
    id: u16,
    selector: String,
}

impl RenderedProbe {
    pub fn new(id: u16, selector: impl Into<String>) -> Result<Self, ChromeError> {
        let selector = selector.into();
        if selector.trim().is_empty() || selector.len() > MAX_SELECTOR_BYTES {
            return Err(ChromeError::InvalidRenderedProbe);
        }
        Ok(Self { id, selector })
    }

    pub fn id(&self) -> u16 {
        self.id
    }

    pub(crate) fn selector(&self) -> &str {
        &self.selector
    }
}

impl fmt::Debug for RenderedProbe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderedProbe")
            .field("id", &self.id)
            .field("selector_bytes", &self.selector.len())
            .finish()
    }
}

/// Privacy-bounded live layout evidence for a captured document.
#[derive(Clone, Default)]
pub struct RenderedPageEvidence {
    results: Vec<RenderedProbeResult>,
}

impl RenderedPageEvidence {
    pub fn result(&self, id: u16) -> Option<&RenderedProbeResult> {
        self.results.iter().find(|result| result.id == id)
    }

    pub fn len(&self) -> usize {
        self.results.len()
    }

    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }
}

impl fmt::Debug for RenderedPageEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderedPageEvidence")
            .field("results", &self.results.len())
            .finish()
    }
}

#[derive(Clone)]
pub struct RenderedProbeResult {
    id: u16,
    matches: u16,
    marker: Option<RenderedSurface>,
    takeover: Option<RenderedSurface>,
}

impl RenderedProbeResult {
    pub fn id(&self) -> u16 {
        self.id
    }

    pub fn matches(&self) -> u16 {
        self.matches
    }

    pub fn marker(&self) -> Option<&RenderedSurface> {
        self.marker.as_ref()
    }

    pub fn takeover(&self) -> Option<&RenderedSurface> {
        self.takeover.as_ref()
    }
}

impl fmt::Debug for RenderedProbeResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderedProbeResult")
            .field("id", &self.id)
            .field("matches", &self.matches)
            .field("has_marker", &self.marker.is_some())
            .field("has_takeover", &self.takeover.is_some())
            .finish()
    }
}

/// Geometry and paint ownership for one live DOM surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderedSurface {
    visible: bool,
    stable: bool,
    viewport_coverage_per_mille: u16,
    hit_coverage_per_mille: u16,
}

impl RenderedSurface {
    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn stable(&self) -> bool {
        self.stable
    }

    pub fn viewport_coverage_per_mille(&self) -> u16 {
        self.viewport_coverage_per_mille
    }

    pub fn hit_coverage_per_mille(&self) -> u16 {
        self.hit_coverage_per_mille
    }
}

pub(crate) fn validate_probes(probes: &[RenderedProbe]) -> Result<(), ChromeError> {
    if probes.len() > MAX_RENDERED_PROBES {
        return Err(ChromeError::InvalidRenderedProbe);
    }
    let mut ids = HashSet::with_capacity(probes.len());
    if probes.iter().any(|probe| !ids.insert(probe.id)) {
        return Err(ChromeError::InvalidRenderedProbe);
    }
    Ok(())
}

pub(crate) fn parse_evidence(
    value: &Value,
    probes: &[RenderedProbe],
) -> Option<RenderedPageEvidence> {
    if value.get("timedOut").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let raw_results = value.get("results")?.as_array()?;
    if raw_results.len() > probes.len() {
        return None;
    }

    let expected_ids = probes.iter().map(RenderedProbe::id).collect::<HashSet<_>>();
    let mut seen_ids = HashSet::with_capacity(raw_results.len());
    let mut results = Vec::with_capacity(raw_results.len());
    for raw in raw_results {
        let id = raw
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|id| u16::try_from(id).ok())?;
        if !expected_ids.contains(&id) || !seen_ids.insert(id) {
            return None;
        }
        let matches = raw.get("matches").and_then(Value::as_u64)?;
        if matches > MAX_MATCHES {
            return None;
        }
        results.push(RenderedProbeResult {
            id,
            matches: matches as u16,
            marker: parse_surface(raw.get("marker"))?,
            takeover: parse_surface(raw.get("takeover"))?,
        });
    }
    Some(RenderedPageEvidence { results })
}

fn parse_surface(value: Option<&Value>) -> Option<Option<RenderedSurface>> {
    let value = value?;
    if value.is_null() {
        return Some(None);
    }
    let viewport = value.get("viewportCoverage").and_then(Value::as_u64)?;
    let hit = value.get("hitCoverage").and_then(Value::as_u64)?;
    if viewport > MAX_PER_MILLE || hit > MAX_PER_MILLE {
        return None;
    }
    Some(Some(RenderedSurface {
        visible: value.get("visible").and_then(Value::as_bool)?,
        stable: value.get("stable").and_then(Value::as_bool)?,
        viewport_coverage_per_mille: viewport as u16,
        hit_coverage_per_mille: hit as u16,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn validates_probe_bounds_and_unique_ids() {
        assert!(RenderedProbe::new(1, "#gate").is_ok());
        assert!(RenderedProbe::new(1, " ").is_err());
        assert!(RenderedProbe::new(1, "x".repeat(MAX_SELECTOR_BYTES + 1)).is_err());

        let duplicate = [
            RenderedProbe::new(1, "#one").unwrap(),
            RenderedProbe::new(1, "#two").unwrap(),
        ];
        assert!(validate_probes(&duplicate).is_err());
    }

    #[test]
    fn parses_only_bounded_results_for_requested_probe_ids() {
        let probes = [RenderedProbe::new(7, "#gate").unwrap()];
        let value = json!({
            "results": [{
                "id": 7,
                "matches": 1,
                "marker": {
                    "visible": true,
                    "stable": true,
                    "viewportCoverage": 250,
                    "hitCoverage": 200
                },
                "takeover": {
                    "visible": true,
                    "stable": true,
                    "viewportCoverage": 900,
                    "hitCoverage": 800
                }
            }]
        });

        let evidence = parse_evidence(&value, &probes).unwrap();
        let result = evidence.result(7).unwrap();
        assert_eq!(result.matches(), 1);
        assert_eq!(result.marker().unwrap().viewport_coverage_per_mille(), 250);
        assert_eq!(result.takeover().unwrap().hit_coverage_per_mille(), 800);

        let wrong_id = json!({ "results": [{
            "id": 8,
            "matches": 0,
            "marker": null,
            "takeover": null
        }] });
        assert!(parse_evidence(&wrong_id, &probes).is_none());
    }

    #[test]
    fn discards_timed_out_or_out_of_range_evidence() {
        let probes = [RenderedProbe::new(1, "#gate").unwrap()];
        assert!(parse_evidence(&json!({ "timedOut": true, "results": [] }), &probes).is_none());
        assert!(
            parse_evidence(
                &json!({ "results": [{
                    "id": 1,
                    "matches": 1,
                    "marker": {
                        "visible": true,
                        "stable": true,
                        "viewportCoverage": 1001,
                        "hitCoverage": 0
                    },
                    "takeover": null
                }] }),
                &probes
            )
            .is_none()
        );
    }
}
