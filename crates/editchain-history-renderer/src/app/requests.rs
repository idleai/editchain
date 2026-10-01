//! Request ownership and bounded diagnostics for the pure renderer reducer.

use editchain_protocol::{ErrorCode, RequestBody, ServiceError};
use idle_history::requests::{RequestError, RequestTracker};
use serde_json::json;

use super::host::{LoggedRequest, Send};

const MAX_RETAINED_REQUESTS: usize = 128;
const MAX_LOGGED_REQUESTS: usize = 128;

#[derive(Debug, Clone)]
pub(super) struct InFlight {
    pub(super) body: RequestBody,
    pub(super) gen_tag: u64,
    pub(super) search_epoch: Option<u64>,
}

/// The registry owns both correlation and the single pending-window slot.
/// Old search envelopes are retained only for bounded stale-response diagnostics;
/// the latest request and the pending window are never evicted by that bound.
#[derive(Debug, Clone)]
pub(crate) struct RequestRegistry {
    in_flight: RequestTracker<InFlight>,
    log: Vec<LoggedRequest>,
}

impl Default for RequestRegistry {
    fn default() -> Self {
        Self {
            in_flight: RequestTracker::bounded(MAX_RETAINED_REQUESTS),
            log: Vec::new(),
        }
    }
}

impl RequestRegistry {
    pub(super) fn contains(&self, id: u64) -> bool {
        self.in_flight.contains(id)
    }
    pub(super) fn register(
        &mut self,
        body: &RequestBody,
        generation: u64,
        search_epoch: Option<u64>,
    ) -> Result<(u64, Send), ServiceError> {
        body.validate()?;
        let is_window = matches!(
            body,
            RequestBody::GetWindow(_) | RequestBody::ReconcileRows(_)
        );
        let id = self
            .in_flight
            .register(
                InFlight {
                    body: body.clone(),
                    gen_tag: generation,
                    search_epoch,
                },
                is_window,
            )
            .map_err(|error| match error {
                RequestError::WindowBusy => invalid("A history window is already in flight."),
                RequestError::Exhausted => {
                    invalid("History request IDs are exhausted. Reopen the webview.")
                }
            })?;
        let body = json!(body);
        if self.log.len() == MAX_LOGGED_REQUESTS {
            drop(self.log.remove(0));
        }
        self.log.push(LoggedRequest {
            id,
            body: body.clone(),
        });
        // All ownership is established before the send can reach a synchronous
        // fixture bridge and enqueue its response.
        Ok((id, Send::Request { id, body }))
    }

    pub(super) fn take(&mut self, id: u64) -> Option<(InFlight, bool)> {
        self.in_flight.take(id)
    }

    pub(super) fn clear(&mut self) {
        self.in_flight.clear();
    }

    pub(super) const fn pending_window(&self) -> Option<u64> {
        self.in_flight.pending_window()
    }

    pub(crate) fn len(&self) -> usize {
        self.in_flight.len()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.in_flight.is_empty()
    }

    #[cfg(test)]
    pub(super) fn get(&self, id: u64) -> Option<&InFlight> {
        self.in_flight.get(id)
    }

    #[cfg(test)]
    pub(super) fn log(&self) -> &[LoggedRequest] {
        &self.log
    }
}

fn invalid(message: &str) -> ServiceError {
    ServiceError::new(ErrorCode::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::host::{find_in_history, get_window};
    use editchain_protocol::SnapshotId;

    #[test]
    fn correlation_and_history_are_bounded_without_retiring_the_pending_window() {
        let mut requests = RequestRegistry::default();
        let snapshot = SnapshotId::new("fixture");
        let (window, _) = requests
            .register(&get_window(&snapshot, 0, 500, false), 1, None)
            .unwrap();
        let mut last = 0;
        for epoch in 0..1000 {
            let (id, _) = requests
                .register(&find_in_history(&snapshot, "needle", 50), 1, Some(epoch))
                .unwrap();
            assert!(id > last);
            last = id;
        }
        assert_eq!(requests.len(), MAX_RETAINED_REQUESTS);
        assert_eq!(requests.log().len(), MAX_LOGGED_REQUESTS);
        assert_eq!(requests.pending_window(), Some(window));
        assert!(
            requests.take(2).is_none(),
            "retired stale responses cannot disturb current ownership"
        );
        assert_eq!(requests.take(last).unwrap().0.search_epoch, Some(999));
        assert!(requests.take(window).unwrap().1);
        assert_eq!(requests.pending_window(), None);
        requests.clear();
        assert!(requests.is_empty());
        let (next, _) = requests
            .register(&get_window(&snapshot, 0, 500, false), 2, None)
            .unwrap();
        assert!(next > last, "clearing a view never reuses request IDs");
    }

    #[test]
    fn invalid_requests_do_not_overwrite_pending_requests() {
        let snapshot = SnapshotId::new("fixture");
        let mut requests = RequestRegistry::default();
        assert!(requests
            .register(&get_window(&snapshot, 0, 0, false), 0, None)
            .is_err());
        assert!(requests.log().is_empty());
        let (id, _) = requests
            .register(&get_window(&snapshot, 0, 1, false), 0, None)
            .unwrap();
        assert_eq!(id, 1);
        assert!(requests
            .register(&get_window(&snapshot, 0, 1, true), 0, None)
            .is_err());
        assert_eq!(requests.len(), 1);
        assert_eq!(requests.pending_window(), Some(id));
        assert_eq!(requests.log().len(), 1);
    }
}
