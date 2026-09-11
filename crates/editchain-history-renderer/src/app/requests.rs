//! Request ownership and bounded diagnostics for the pure renderer reducer.

use std::collections::BTreeMap;

use editchain_protocol::{ErrorCode, RequestBody, ServiceError};
use serde_json::json;

use super::host::{LoggedRequest, Send};

// Numeric request IDs cross the JavaScript bridge without a string adapter.
const MAX_REQUEST_ID: u64 = 9_007_199_254_740_991;
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
    next_id: Option<u64>,
    in_flight: BTreeMap<u64, InFlight>,
    window: Option<u64>,
    log: Vec<LoggedRequest>,
}

impl Default for RequestRegistry {
    fn default() -> Self {
        Self {
            next_id: Some(1),
            in_flight: BTreeMap::new(),
            window: None,
            log: Vec::new(),
        }
    }
}

impl RequestRegistry {
    pub(super) fn register(
        &mut self,
        body: &RequestBody,
        generation: u64,
        search_epoch: Option<u64>,
    ) -> Result<(u64, Send), ServiceError> {
        body.validate()?;
        let is_window = matches!(body, RequestBody::GetWindow(_));
        if is_window && self.window.is_some() {
            return Err(invalid("A history window is already in flight."));
        }
        let id = self
            .next_id
            .ok_or_else(|| invalid("History request IDs are exhausted. Reopen the webview."))?;
        self.next_id = id.checked_add(1).filter(|next| *next <= MAX_REQUEST_ID);
        // At most one window and the most recent search matter to the reducer.
        // Retire the oldest remaining non-window envelope at the diagnostic cap.
        if self.in_flight.len() >= MAX_RETAINED_REQUESTS {
            if let Some(oldest) = self
                .in_flight
                .keys()
                .copied()
                .find(|old| Some(*old) != self.window)
            {
                drop(self.in_flight.remove(&oldest));
            }
        }
        drop(self.in_flight.insert(
            id,
            InFlight {
                body: body.clone(),
                gen_tag: generation,
                search_epoch,
            },
        ));
        if is_window {
            self.window = Some(id);
        }
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
        let request = self.in_flight.remove(&id)?;
        let was_window = self.window == Some(id);
        if was_window {
            self.window = None;
        }
        Some((request, was_window))
    }

    pub(super) fn clear(&mut self) {
        self.in_flight.clear();
        self.window = None;
    }

    pub(super) const fn pending_window(&self) -> Option<u64> {
        self.window
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
        self.in_flight.get(&id)
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
    fn invalid_requests_and_exhaustion_do_not_overwrite_pending_requests() {
        let snapshot = SnapshotId::new("fixture");
        let mut requests = RequestRegistry {
            next_id: Some(MAX_REQUEST_ID),
            ..RequestRegistry::default()
        };
        assert!(requests
            .register(&get_window(&snapshot, 0, 0, false), 0, None)
            .is_err());
        assert!(requests.log().is_empty());
        let (id, _) = requests
            .register(&get_window(&snapshot, 0, 1, false), 0, None)
            .unwrap();
        assert_eq!(id, MAX_REQUEST_ID);
        assert!(requests
            .register(&get_window(&snapshot, 0, 1, true), 0, None)
            .is_err());
        assert!(requests
            .register(&find_in_history(&snapshot, "x", 1), 0, Some(1))
            .is_err());
        assert_eq!(requests.len(), 1);
        assert_eq!(requests.pending_window(), Some(id));
        assert_eq!(requests.log().len(), 1);
    }
}
