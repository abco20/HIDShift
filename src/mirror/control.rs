use crate::interchip::{ControlStatus, MirrorControlRequest, MirrorControlResponse};

pub const MIRROR_CONTROL_TIMEOUT_MS: u64 = 250;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MirrorControlForwarderError {
    Busy,
    InvalidMessage,
    StaleResponse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingMirrorControl {
    pub request_id: u32,
    pub deadline_ms: u64,
}

/// Owns the single EP0 request that may be forwarded across the SPI link.
///
/// The USB adapter remains responsible for holding and completing EP0; this
/// state machine owns IDs, timeout policy, and rejection of stale responses.
pub struct MirrorControlForwarder {
    next_request_id: u32,
    pending: Option<PendingMirrorControl>,
}

impl MirrorControlForwarder {
    pub const fn new() -> Self {
        Self {
            next_request_id: 1,
            pending: None,
        }
    }

    pub const fn pending(&self) -> Option<PendingMirrorControl> {
        self.pending
    }

    pub fn begin(
        &mut self,
        setup_packet: [u8; 8],
        data: &[u8],
        now_ms: u64,
    ) -> Result<MirrorControlRequest, MirrorControlForwarderError> {
        if self.pending.is_some() {
            return Err(MirrorControlForwarderError::Busy);
        }
        let request_id = self.next_request_id;
        self.next_request_id = next_nonzero(self.next_request_id);
        let request = MirrorControlRequest::new(request_id, setup_packet, data)
            .map_err(|_| MirrorControlForwarderError::InvalidMessage)?;
        self.pending = Some(PendingMirrorControl {
            request_id,
            deadline_ms: now_ms.saturating_add(MIRROR_CONTROL_TIMEOUT_MS),
        });
        Ok(request)
    }

    pub fn complete(
        &mut self,
        response: MirrorControlResponse,
        now_ms: u64,
    ) -> Result<MirrorControlResponse, MirrorControlForwarderError> {
        let Some(pending) = self.pending else {
            return Err(MirrorControlForwarderError::StaleResponse);
        };
        if pending.request_id != response.request_id || now_ms >= pending.deadline_ms {
            return Err(MirrorControlForwarderError::StaleResponse);
        }
        self.pending = None;
        Ok(response)
    }

    pub fn expire(&mut self, now_ms: u64) -> Option<MirrorControlResponse> {
        let pending = self
            .pending
            .filter(|pending| now_ms >= pending.deadline_ms)?;
        self.pending = None;
        MirrorControlResponse::new(pending.request_id, ControlStatus::Timeout, &[]).ok()
    }

    pub fn cancel(&mut self) -> Option<u32> {
        self.pending.take().map(|pending| pending.request_id)
    }
}

impl Default for MirrorControlForwarder {
    fn default() -> Self {
        Self::new()
    }
}

const fn next_nonzero(value: u32) -> u32 {
    if value == u32::MAX || value == 0 {
        1
    } else {
        value + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_single_flight_and_matching_response_completes_it() {
        let mut forwarder = MirrorControlForwarder::new();
        let setup = [0x21, 9, 0x10, 3, 1, 0, 17, 0];
        let request = forwarder.begin(setup, &[0x10; 17], 100).unwrap();
        assert_eq!(request.setup_packet, setup);
        assert_eq!(request.data(), &[0x10; 17]);
        assert_eq!(
            forwarder.begin(setup, &[], 101),
            Err(MirrorControlForwarderError::Busy)
        );
        let response =
            MirrorControlResponse::new(request.request_id, ControlStatus::Success, &[]).unwrap();
        assert_eq!(forwarder.complete(response, 349), Ok(response));
        assert_eq!(forwarder.pending(), None);
    }

    #[test]
    fn timeout_and_cancel_clear_pending_and_stale_responses_are_rejected() {
        let mut forwarder = MirrorControlForwarder::new();
        let request = forwarder
            .begin([0xa1, 1, 0x10, 3, 1, 0, 17, 0], &[], 10)
            .unwrap();
        assert_eq!(forwarder.expire(259), None);
        let timeout = forwarder.expire(260).unwrap();
        assert_eq!(timeout.request_id, request.request_id);
        assert_eq!(timeout.status, ControlStatus::Timeout);
        let response =
            MirrorControlResponse::new(request.request_id, ControlStatus::Success, &[0x10; 17])
                .unwrap();
        assert_eq!(
            forwarder.complete(response, 260),
            Err(MirrorControlForwarderError::StaleResponse)
        );

        let next = forwarder.begin([0; 8], &[], 300).unwrap();
        assert_eq!(forwarder.cancel(), Some(next.request_id));
        assert_eq!(forwarder.pending(), None);
    }
}
