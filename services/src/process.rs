use std::{fmt, future::Future, pin::Pin};

use tokio::sync::{mpsc, oneshot};

type Operation<S> = Box<dyn FnOnce(&mut S) + Send>;
type AsyncOperation<S> = Box<dyn FnOnce(S) -> Pin<Box<dyn Future<Output = S> + Send>> + Send>;

enum Request<S> {
    Apply(Operation<S>),
    ApplyAsync(AsyncOperation<S>),
}

/// 内部服务的数据只由接收请求的任务持有。
pub(crate) struct Process<S>(mpsc::UnboundedSender<Request<S>>);

pub(crate) struct WeakProcess<S>(mpsc::WeakUnboundedSender<Request<S>>);

impl<S> WeakProcess<S> {
    pub(crate) fn upgrade(&self) -> Option<Process<S>> {
        self.0.upgrade().map(Process)
    }
}

impl<S> Clone for Process<S> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<S> fmt::Debug for Process<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Process").finish_non_exhaustive()
    }
}

impl<S: Send + 'static> Process<S> {
    pub(crate) fn downgrade(&self) -> WeakProcess<S> {
        WeakProcess(self.0.downgrade())
    }

    pub(crate) fn start(state: S) -> Self {
        let (process, run) = Self::channel(state);
        tokio::spawn(run);
        process
    }

    pub(crate) fn channel(mut state: S) -> (Self, impl Future<Output = ()> + Send + 'static) {
        let (sender, mut requests) = mpsc::unbounded_channel::<Request<S>>();
        let run = async move {
            while let Some(request) = requests.recv().await {
                match request {
                    Request::Apply(operation) => operation(&mut state),
                    Request::ApplyAsync(operation) => state = operation(state).await,
                }
            }
        };
        (Self(sender), run)
    }

    pub(crate) fn post(&self, operation: impl FnOnce(&mut S) + Send + 'static) {
        let _ = self.0.send(Request::Apply(Box::new(operation)));
    }

    pub(crate) async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut S) -> T + Send + 'static,
    ) -> T {
        let (reply, response) = oneshot::channel();
        self.post(move |state| {
            let _ = reply.send(operation(state));
        });
        response.await.expect("内部服务未返回响应")
    }

    pub(crate) fn post_blocking(&self, operation: impl FnOnce(&mut S) + Send + 'static) {
        let request = Request::ApplyAsync(Box::new(move |mut state| {
            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    operation(&mut state);
                    state
                })
                .await
                .expect("内部服务磁盘任务失败")
            })
        }));
        let _ = self.0.send(request);
    }

    /// 在同一请求队列中完成磁盘操作，避免占用异步执行线程。
    pub(crate) async fn blocking<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut S) -> T + Send + 'static,
    ) -> T {
        self.call_async(move |mut state| async move {
            tokio::task::spawn_blocking(move || {
                let result = operation(&mut state);
                (state, result)
            })
            .await
            .expect("内部服务磁盘任务失败")
        })
        .await
    }

    pub(crate) async fn with<T: Send + 'static>(
        &self,
        operation: impl for<'a> FnOnce(&'a mut S) -> Pin<Box<dyn Future<Output = T> + Send + 'a>>
        + Send
        + 'static,
    ) -> T {
        self.call_async(|mut state| async move {
            let result = operation(&mut state).await;
            (state, result)
        })
        .await
    }

    pub(crate) async fn call_async<
        T: Send + 'static,
        F: Future<Output = (S, T)> + Send + 'static,
    >(
        &self,
        operation: impl FnOnce(S) -> F + Send + 'static,
    ) -> T {
        let (reply, response) = oneshot::channel();
        let request = Request::ApplyAsync(Box::new(move |state| {
            Box::pin(async move {
                let (state, result) = operation(state).await;
                let _ = reply.send(result);
                state
            })
        }));
        assert!(self.0.send(request).is_ok(), "内部服务已退出");
        response.await.expect("内部服务未返回响应")
    }
}

impl Process<()> {
    pub(crate) async fn run<T: Send + 'static>(
        &self,
        operation: impl Future<Output = T> + Send + 'static,
    ) -> T {
        self.call_async(|()| async move { ((), operation.await) })
            .await
    }
}
