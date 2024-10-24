use std::{
    fmt::Debug,
    future::{Future, IntoFuture},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use alloy::{
    node_bindings::Anvil,
    providers::{Provider, ProviderBuilder},
    rpc::{
        client::ClientBuilder,
        json_rpc::{RequestPacket, Response, ResponsePacket, ResponsePayload, RpcError},
    },
    transports::TransportError,
};
use eyre::Result;
use revm::{
    db::{in_memory_db::CacheDB, EmptyDB},
    interpreter::primitives::Env,
    Evm,
};
use serde_json::value::RawValue;
use tower::{Layer, Service};

struct RevmLayer {
    db: Arc<Mutex<CacheDB<EmptyDB>>>,
}

impl RevmLayer {
    fn new(db: CacheDB<EmptyDB>) -> Self {
        RevmLayer {
            db: Arc::new(Mutex::new(db)),
        }
    }
}

impl<S> Layer<S> for RevmLayer {
    type Service = RevmService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RevmService {
            inner,
            db: self.db.clone(),
        }
    }
}

// A logging service that wraps an inner service.
#[derive(Debug, Clone)]
struct RevmService<S> {
    inner: S,
    db: Arc<Mutex<CacheDB<EmptyDB>>>,
}

// Implement tower::Service for LoggingService.
impl<S> Service<RequestPacket> for RevmService<S>
where
    S: Service<RequestPacket, Response = ResponsePacket, Error = TransportError> + Send,
    S::Future: Send + 'static,
    S::Response: Send + 'static + Debug,
    S::Error: Send + 'static + Debug,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let db = self.db.clone();
        let inner_future = self.inner.call(req.clone());

        Box::pin(async move {
            let mut db_guard = db.lock().unwrap();
            let mut env = Env::default();
            let mut evm = Evm::builder()
                .with_db(&mut *db_guard)
                .with_env(Box::new(env))
                .build();

            match req {
                RequestPacket::Single(req) => {
                    match req.clone().meta().method.clone().into_owned().as_str() {
                        // build out the rest of the relevant json rpc methods from here
                        "eth_blockNumber" => {
                            let block_number = evm.block().number.to_string();
                            let res = Response {
                                id: req.meta().id.clone(),
                                payload: ResponsePayload::Success(
                                    RawValue::from_string(block_number).unwrap(),
                                ),
                            };
                            Ok(ResponsePacket::Single(res))
                        }
                        _ => Err(RpcError::UnsupportedFeature("method not found").into()),
                    }
                }
                RequestPacket::Batch(_reqs) => {
                    Err(RpcError::UnsupportedFeature("batch requests not supported").into())
                }
            }
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Spin up a local Anvil node.
    // Ensure `anvil` is available in $PATH.
    let anvil = Anvil::new().spawn();

    // Create a new client with the logging layer.
    let rpc_url = anvil.endpoint_url();
    let client = ClientBuilder::default()
        .layer(RevmLayer::new(CacheDB::new(EmptyDB::new())))
        .http(rpc_url);

    // Create a new provider with the client.
    let provider = ProviderBuilder::new().on_client(client);

    for _ in 0..10 {
        println!("{:?}", provider.get_block_number().into_future().await?);
    }

    Ok(())
}
