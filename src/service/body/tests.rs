use super::*;
use crate::service::activity::RequestStep;
use std::{collections::VecDeque, convert::Infallible};

type Polled = Poll<Option<Result<Frame<Bytes>, Infallible>>>;

/// Answers each poll with the next scripted result.
struct Scripted(VecDeque<Polled>);

impl Body for Scripted {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Polled {
        self.0.pop_front().unwrap_or(Poll::Ready(None))
    }
}

fn client_paced(progress: &watch::Receiver<ObjectActivity>) -> bool {
    matches!(progress.borrow().step(), RequestStep::ClientPaced)
}

/// A slow write between frames is the server's work, so only a wait on the client counts as
/// the client's silence, and every delivered frame still renews the limit.
#[test]
fn only_a_wait_on_the_client_counts_as_its_silence() {
    let frame = || Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"chunk")))));
    let (activity, mut progress) = watch::channel(ObjectActivity::default());
    let mut body = TransportBody {
        body: Scripted(VecDeque::from([frame(), frame(), Poll::Pending, frame()])),
        activity,
    };
    let mut cx = Context::from_waker(std::task::Waker::noop());
    for (poll, waits_on_client) in [(1, false), (2, false), (3, true), (4, false)] {
        let _ = Pin::new(&mut body).poll_frame(&mut cx);
        assert!(
            progress.has_changed().unwrap(),
            "poll {poll} restarts the watch's count"
        );
        progress.mark_unchanged();
        assert_eq!(client_paced(&progress), waits_on_client, "poll {poll}");
    }
}
