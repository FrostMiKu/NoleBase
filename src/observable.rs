use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub struct Observable<T, E> {
    pub output: BoxFuture<'static, T>,
    pub events: broadcast::Receiver<E>,
    pub cancel: CancellationToken,
}

impl<T, E: Clone> Observable<T, E> {
    #[allow(dead_code)]
    pub fn subscribe(&self) -> broadcast::Receiver<E> {
        self.events.resubscribe()
    }
}
