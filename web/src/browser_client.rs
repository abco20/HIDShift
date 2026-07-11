use std::cell::RefCell;
use std::rc::Rc;

use futures_channel::oneshot;
use futures_util::future::{Either, select};
use gloo_timers::future::TimeoutFuture;
use hidshift::{ManagementCommand, ManagementResponse};
use hidshift_client::ManagementClient;

use crate::transport::BrowserTransport;

const REQUEST_TIMEOUT_MS: u32 = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserClientError {
    Busy,
    Disconnected,
    Transport(String),
    Protocol(String),
    Timeout,
}

pub struct BrowserClient {
    protocol: RefCell<ManagementClient>,
    transport: RefCell<Option<Rc<BrowserTransport>>>,
    response_sender:
        RefCell<Option<oneshot::Sender<Result<ManagementResponse, BrowserClientError>>>>,
}

impl BrowserClient {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            protocol: RefCell::new(ManagementClient::new(0)),
            transport: RefCell::new(None),
            response_sender: RefCell::new(None),
        })
    }

    pub fn attach(self: &Rc<Self>, transport: Rc<BrowserTransport>) {
        *self.transport.borrow_mut() = Some(transport);
    }

    pub fn detach(&self) {
        self.protocol.borrow_mut().cancel();
        self.response_sender.borrow_mut().take();
        self.transport.borrow_mut().take();
    }

    pub fn receive(&self, bytes: &[u8]) {
        let accepted = self.protocol.borrow_mut().accept(bytes);
        let response = match accepted {
            Ok(response) => Ok(response),
            Err(error) => {
                if !self.protocol.borrow().is_pending() {
                    return;
                }
                self.protocol.borrow_mut().cancel();
                Err(BrowserClientError::Protocol(format!(
                    "firmware response rejected: {error:?}; bytes={bytes:02x?}"
                )))
            }
        };
        if let Some(sender) = self.response_sender.borrow_mut().take() {
            let _ = sender.send(response);
        }
    }

    pub async fn request(
        &self,
        command: ManagementCommand,
    ) -> Result<ManagementResponse, BrowserClientError> {
        if self.response_sender.borrow().is_some() {
            return Err(BrowserClientError::Busy);
        }
        let transport = self
            .transport
            .borrow()
            .clone()
            .ok_or(BrowserClientError::Disconnected)?;
        let pending = self
            .protocol
            .borrow_mut()
            .begin(command)
            .map_err(|error| BrowserClientError::Protocol(format!("{error:?}")))?;
        let (sender, receiver) = oneshot::channel();
        *self.response_sender.borrow_mut() = Some(sender);

        if let Err(error) = transport.write(pending).await {
            self.protocol.borrow_mut().cancel();
            self.response_sender.borrow_mut().take();
            return Err(BrowserClientError::Transport(error));
        }

        let receiver = Box::pin(receiver);
        let timeout = Box::pin(TimeoutFuture::new(REQUEST_TIMEOUT_MS));
        match select(receiver, timeout).await {
            Either::Left((Ok(response), _)) => response,
            Either::Left((Err(_), _)) => Err(BrowserClientError::Disconnected),
            Either::Right(((), _)) => {
                self.protocol.borrow_mut().cancel();
                self.response_sender.borrow_mut().take();
                Err(BrowserClientError::Timeout)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt;
    use hidshift::ManagementCommand;

    use super::*;

    #[test]
    fn malformed_response_fails_pending_request_instead_of_timing_out() {
        let client = BrowserClient::new();
        client
            .protocol
            .borrow_mut()
            .begin(ManagementCommand::GetStatus)
            .unwrap();
        let (sender, receiver) = oneshot::channel();
        *client.response_sender.borrow_mut() = Some(sender);

        client.receive(&[0xff; 20]);

        assert!(matches!(
            receiver.now_or_never(),
            Some(Ok(Err(BrowserClientError::Protocol(message))))
                if message.contains("firmware response rejected")
                    && message.contains("bytes=")
        ));
        assert!(!client.protocol.borrow().is_pending());
    }

    #[test]
    fn unsolicited_invalid_notification_is_ignored() {
        let client = BrowserClient::new();

        client.receive(&[0xff; 20]);

        assert!(client.response_sender.borrow().is_none());
        assert!(!client.protocol.borrow().is_pending());
    }
}
