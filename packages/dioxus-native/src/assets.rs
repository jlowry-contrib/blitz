use blitz_shell::BlitzShellProxy;
use std::sync::Arc;

use blitz_traits::net::{NetHandler, NetProvider, Request};

/// Serves a `dioxus://` request path from memory, or returns `None` to fall back to the asset files.
pub type AssetHandler = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

#[derive(Default)]
pub struct DioxusNativeNetProvider {
    inner_net_provider: Option<Arc<dyn NetProvider + 'static>>,
    asset_handler: Option<AssetHandler>,
}

#[allow(unused)]
impl DioxusNativeNetProvider {
    pub fn shared(proxy: BlitzShellProxy) -> Arc<dyn NetProvider> {
        Arc::new(Self::new(proxy)) as Arc<dyn NetProvider>
    }

    pub fn new(proxy: BlitzShellProxy) -> Self {
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
        }
    }

    pub fn with_inner(proxy: BlitzShellProxy, inner: Arc<dyn NetProvider>) -> Self {
        Self {
            inner_net_provider: Some(inner),
            asset_handler: None,
        }
    }

    /// Serves `dioxus://` requests from `handler` first, before the bundled asset files.
    pub fn with_asset_handler(mut self, handler: AssetHandler) -> Self {
        self.asset_handler = Some(handler);
        self
    }

    pub fn inner(&self) -> Option<&Arc<dyn NetProvider>> {
        self.inner_net_provider.as_ref()
    }
}

impl NetProvider for DioxusNativeNetProvider {
    fn fetch(&self, doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        if request.url.scheme() == "dioxus" {
            if let Some(bytes) = self
                .asset_handler
                .as_ref()
                .and_then(|serve| serve(request.url.path()))
            {
                handler.bytes(request.url.to_string(), bytes.into());
                return;
            }
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
        } else if let Some(inner) = &self.inner_net_provider {
            inner.fetch(doc_id, request, handler);
        } else {
            #[cfg(feature = "tracing")]
            tracing::warn!("net feature not enabled, cannot fetch {request:#?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};

    use super::DioxusNativeNetProvider;

    struct Recorder(Arc<Mutex<Option<(String, Bytes)>>>);

    impl NetHandler for Recorder {
        fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
            *self.0.lock().unwrap() = Some((resolved_url, bytes));
        }
    }

    fn fetch(provider: &DioxusNativeNetProvider, url: &str) -> Option<(String, Bytes)> {
        let seen = Arc::new(Mutex::new(None));
        let request = Request::get(url.parse().unwrap());
        provider.fetch(0, request, Box::new(Recorder(Arc::clone(&seen))));
        seen.lock().unwrap().take()
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
}
