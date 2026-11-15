//! PIPE_NOWAIT acceptance boundary, shared by Win32 and controlled API traces.
//! The OS adapter supplies codes; tests need no Win32 execution or copied codes.
pub(super) trait Api {
    const CONNECTED: u32;
    const LISTENING: u32;
    const NO_DATA: u32;
    fn connect(&self) -> (bool, u32);
    fn disconnect(&self);
}
pub(super) fn poll<A: Api>(api: &A) -> Result<bool, u32> {
    let (ready, error) = api.connect();
    // PIPE_NOWAIT nonzero only transitions a disconnected pipe to listening.
    // Its last-error value is undefined and cannot establish a connection.
    if ready {
        return Ok(false);
    }
    if error == A::CONNECTED {
        return Ok(true);
    }
    if error == A::LISTENING {
        Ok(false)
    } else if error == A::NO_DATA {
        api.disconnect();
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};
    #[derive(Clone, Copy)]
    enum State {
        ReadyToListen,
        Listening,
        Connected,
        ClientClosed,
        Failure,
    }
    struct Fake {
        transitions: RefCell<VecDeque<State>>,
        trace: Rc<RefCell<Vec<&'static str>>>,
    }
    impl Api for Fake {
        const CONNECTED: u32 = 11;
        const LISTENING: u32 = 12;
        const NO_DATA: u32 = 13;
        fn connect(&self) -> (bool, u32) {
            self.trace.borrow_mut().push("connect");
            match self.transitions.borrow_mut().pop_front().unwrap() {
                // GetLastError is undefined after nonzero; deliberately stale.
                State::ReadyToListen => (true, Self::CONNECTED),
                State::Listening => (false, Self::LISTENING),
                State::Connected => (false, Self::CONNECTED),
                State::ClientClosed => (false, Self::NO_DATA),
                State::Failure => (false, 99),
            }
        }
        fn disconnect(&self) {
            self.trace.borrow_mut().push("disconnect");
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.trace.borrow_mut().push("close-owned-handle");
        }
    }
    #[test]
    fn nowait_idle_ack_reconnect_and_owned_shutdown_trace() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let api = Fake {
            transitions: RefCell::new(
                [
                    State::ReadyToListen,
                    State::Listening,
                    State::Connected,
                    State::ReadyToListen,
                    State::Listening,
                    State::Listening,
                    State::Connected,
                    State::ClientClosed,
                    State::ReadyToListen,
                    State::Connected,
                    State::Failure,
                ]
                .into(),
            ),
            trace: trace.clone(),
        };
        assert!(!poll(&api).unwrap(), "nonzero means ready to listen, never an accepted stream");
        assert!(!poll(&api).unwrap());
        assert_eq!(&*trace.borrow(), &["connect", "connect"], "idle listener must not read/disconnect");
        for idle in [0, 3] {
            for _ in 0..idle {
                assert!(!poll(&api).unwrap());
            }
            assert!(poll(&api).unwrap());
            trace.borrow_mut().extend(["read-request", "write-reply", "client-ack"]);
            api.disconnect();
            assert!(!trace.borrow().contains(&"close-owned-handle"), "listener retains ownership across clients");
        }
        assert!(!poll(&api).unwrap(), "a client that closed without a stream requires reset");
        assert!(!poll(&api).unwrap());
        assert!(poll(&api).unwrap());
        trace.borrow_mut().extend(["read-request", "write-reply", "client-ack"]);
        api.disconnect();
        assert_eq!(poll(&api), Err(99));
        assert_eq!(trace.borrow().iter().filter(|e| **e == "disconnect").count(), 4);
        assert_eq!(trace.borrow().iter().filter(|e| **e == "read-request").count(), 3);
        drop(api);
        assert_eq!(trace.borrow().last(), Some(&"close-owned-handle"));
        let trace = trace.borrow();
        for (index, event) in trace.iter().enumerate() {
            if *event == "client-ack" {
                assert_eq!(trace[index + 1], "disconnect");
            }
        }
    }
}
