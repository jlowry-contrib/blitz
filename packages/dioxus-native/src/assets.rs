use blitz_shell::BlitzShellProxy;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use blitz_traits::net::{NetHandler, NetProvider, NetWaker, Request};

/// Serves a `dioxus://` request path from memory, or returns `None` to fall back to the asset files.
pub type AssetHandler = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// Requests the asset handler serves at once; it bounds the memory of concurrent image decodes.
const ASSET_WORKERS: usize = 4;

#[derive(Default)]
pub struct DioxusNativeNetProvider {
    inner_net_provider: Option<Arc<dyn NetProvider + 'static>>,
    asset_handler: Option<AssetHandler>,
    waker: Option<Arc<dyn NetWaker>>,
    jobs: Option<Sender<Job>>,
}

type Job = Box<dyn FnOnce() + Send>;

fn workers() -> Sender<Job> {
    let (jobs, queue) = channel::<Job>();
    let queue = Arc::new(Mutex::new(queue));
    for _ in 0..ASSET_WORKERS {
        let queue = Arc::clone(&queue);
        std::thread::Builder::new()
            .name("dioxus-asset-handler".into())
            .spawn(move || {
                loop {
                    let Ok(job) = queue.lock().unwrap().recv() else {
                        break;
                    };
                    job();
                }
            })
            .expect("spawning an asset handler thread");
    }
    jobs
}

#[allow(unused)]
impl DioxusNativeNetProvider {
    pub fn shared(proxy: BlitzShellProxy) -> Arc<dyn NetProvider> {
        Arc::new(Self::new(proxy)) as Arc<dyn NetProvider>
    }

    pub fn new(proxy: BlitzShellProxy) -> Self {
        let waker = Some(Arc::new(proxy.clone()) as Arc<dyn NetWaker>);
        #[cfg(any(feature = "data-uri", feature = "net"))]
        let net_waker = Some(Arc::new(proxy) as _);

        #[cfg(feature = "net")]
        let inner_net_provider = Some(blitz_net::Provider::shared(net_waker.clone()));
        #[cfg(all(feature = "data-uri", not(feature = "net")))]
        let inner_net_provider = Some(blitz_shell::DataUriNetProvider::shared(net_waker.clone()));
        #[cfg(all(not(feature = "data-uri"), not(feature = "net")))]
        let inner_net_provider = None;

        Self {
            inner_net_provider,
            asset_handler: None,
            jobs: None,
            waker,
        }
    }

    pub fn with_inner(proxy: BlitzShellProxy, inner: Arc<dyn NetProvider>) -> Self {
        Self {
            inner_net_provider: Some(inner),
            asset_handler: None,
            jobs: None,
            waker: Some(Arc::new(proxy) as Arc<dyn NetWaker>),
        }
    }

    /// Serves `dioxus://` requests from `handler` first, before the bundled asset files.
    ///
    /// Requests then run on a few worker threads, so a slow handler never blocks the
    /// document, which is woken when each response arrives.
    pub fn with_asset_handler(mut self, handler: AssetHandler) -> Self {
        self.asset_handler = Some(handler);
        self.jobs = Some(workers());
        self
    }

    pub fn inner(&self) -> Option<&Arc<dyn NetProvider>> {
        self.inner_net_provider.as_ref()
    }
}

impl NetProvider for DioxusNativeNetProvider {
    fn fetch(&self, doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        if request.url.scheme() == "dioxus" {
            let (Some(serve), Some(jobs)) = (self.asset_handler.clone(), &self.jobs) else {
                serve_file(request, handler);
                return;
            };
            let waker = self.waker.clone();
            let _ = jobs.send(Box::new(move || {
                match serve(request.url.path()) {
                    Some(bytes) => handler.bytes(request.url.to_string(), bytes.into()),
                    None => serve_file(request, handler),
                }
                if let Some(waker) = waker {
                    waker.wake(doc_id);
                }
            }));
        } else if let Some(inner) = &self.inner_net_provider {
            inner.fetch(doc_id, request, handler);
        } else {
            #[cfg(feature = "tracing")]
            tracing::warn!("net feature not enabled, cannot fetch {request:#?}");
        }
    }
}

fn serve_file(request: Request, handler: Box<dyn NetHandler>) {
    #[allow(clippy::single_match)] // cfg'd code
    match dioxus_asset_resolver::native::serve_asset(request.url.path()) {
        Ok(res) => {
            #[cfg(feature = "tracing")]
            tracing::trace!("fetching asset from file system success {request:#?}");
            handler.bytes(request.url.to_string(), res.into_body().into())
        }
        Err(_) => {
            #[cfg(feature = "tracing")]
            tracing::warn!("fetching asset from file system error {request:#?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Sender, channel};
    use std::sync::{Arc, Mutex};
    use std::thread::ThreadId;
    use std::time::Duration;

    use blitz_traits::net::{Bytes, NetHandler, NetProvider, NetWaker, Request};

    use super::DioxusNativeNetProvider;

    struct Recorder(Mutex<Sender<(String, Bytes)>>);

    impl NetHandler for Recorder {
        fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
            let _ = self.0.lock().unwrap().send((resolved_url, bytes));
        }
    }

    fn fetch(provider: &DioxusNativeNetProvider, url: &str) -> Option<(String, Bytes)> {
        let (sender, receiver) = channel();
        let request = Request::get(url.parse().unwrap());
        provider.fetch(0, request, Box::new(Recorder(Mutex::new(sender))));
        receiver.recv_timeout(Duration::from_secs(5)).ok()
    }

    #[test]
    fn the_asset_handler_serves_dioxus_urls_from_memory() {
        let provider =
            DioxusNativeNetProvider::default().with_asset_handler(Arc::new(|path: &str| {
                (path == "/symbol/heart.png").then(|| b"png bytes".to_vec())
            }));
        let (url, bytes) =
            fetch(&provider, "dioxus://index.html/symbol/heart.png").expect("served");
        assert_eq!(url, "dioxus://index.html/symbol/heart.png");
        assert_eq!(&bytes[..], b"png bytes");
    }

    #[test]
    fn paths_the_handler_declines_fall_back_to_the_filesystem() {
        let provider =
            DioxusNativeNetProvider::default().with_asset_handler(Arc::new(|_: &str| None));
        let (_, bytes) = fetch(&provider, "dioxus://index.html/no/such/asset.png").expect("served");
        assert_eq!(&bytes[..], b"Not Found");
    }

    #[test]
    fn at_most_a_few_requests_run_the_handler_at_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (woken, wakes) = channel();
        let woken = Mutex::new(woken);
        let waker: Arc<dyn NetWaker> =
            Arc::new(move |doc_id: usize| woken.lock().unwrap().send(doc_id).unwrap());
        let running = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let (now, peak) = (Arc::clone(&running), Arc::clone(&most));
        let provider = DioxusNativeNetProvider {
            waker: Some(waker),
            ..DioxusNativeNetProvider::default()
        }
        .with_asset_handler(Arc::new(move |_: &str| {
            peak.fetch_max(now.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            now.fetch_sub(1, Ordering::SeqCst);
            Some(Vec::new())
        }));
        let requests = 3 * super::ASSET_WORKERS;
        for _ in 0..requests {
            let (sender, _received) = channel();
            let request = Request::get("dioxus://index.html/a.png".parse().unwrap());
            provider.fetch(0, request, Box::new(Recorder(Mutex::new(sender))));
        }
        for _ in 0..requests {
            wakes
                .recv_timeout(Duration::from_secs(5))
                .expect("every request wakes");
        }
        let most = most.load(Ordering::SeqCst);
        assert!((2..=super::ASSET_WORKERS).contains(&most), "{most} at once");
    }

    #[test]
    fn the_handler_runs_off_the_fetching_thread_and_then_wakes_the_document() {
        let (woken, wakes) = channel();
        let woken = Mutex::new(woken);
        let waker: Arc<dyn NetWaker> =
            Arc::new(move |doc_id: usize| woken.lock().unwrap().send(doc_id).unwrap());
        let ran_on = Arc::new(Mutex::new(None::<ThreadId>));
        let seen = Arc::clone(&ran_on);
        let provider = DioxusNativeNetProvider {
            waker: Some(waker),
            ..DioxusNativeNetProvider::default()
        }
        .with_asset_handler(Arc::new(move |_: &str| {
            *seen.lock().unwrap() = Some(std::thread::current().id());
            Some(Vec::new())
        }));
        let (sender, _received) = channel();
        let request = Request::get("dioxus://index.html/a.png".parse().unwrap());
        provider.fetch(7, request, Box::new(Recorder(Mutex::new(sender))));
        assert_eq!(wakes.recv_timeout(Duration::from_secs(5)), Ok(7));
        assert_ne!(*ran_on.lock().unwrap(), Some(std::thread::current().id()));
    }
}
