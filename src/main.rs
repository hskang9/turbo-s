use anyhow::*;
use http::header::CACHE_CONTROL;
use hyper::body::Bytes;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Request, Response, Server};
use tokio::sync::Mutex;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use ttl_cache::TtlCache;

// Current Hyper framework considers Body as a stream of bytes, which means, there is no way for rust to just copy it.
// Hence I made the stream into bytes and then add separate builders for each request and response.
async fn relay_request(req: Request<Body>) -> Result<(Request<Body>, Request<Body>), Error> {
    let uri = req.uri();
    let uri_string = match uri.query() {
        // Hard coded upstream server URL
        // TODO: Change this into server url that you want
        None => format!("https://www.standard.tech{}", uri.path()),
        Some(query) => format!("https://www.standard.tech{}?{}", uri.path(), query),
    };
    // turn local request path to the path to the upstream server
    let request_builder = Request::builder().uri(uri_string.clone());
    let second_request_builder = Request::builder().uri(uri_string.clone());
    let body_bytes = hyper::body::to_bytes(req.into_body()).await?;

    let req1 = request_builder.body(Body::from(body_bytes.clone()))?;
    let req2 = second_request_builder.body(Body::from(body_bytes.clone()))?;


    Ok((req1, req2))
}

// Current Hyper framework considers Body as a stream of bytes, which means, there is no way for rust to just copy it.
// Hence I made the stream into bytes and then add separate builders for each request and response.
async fn clone_response(resp: Response<Body>) -> Result<(Response<Body>, Response<Body>), Error> {
    // turn local request path to the path to the upstream server
    let body_bytes = hyper::body::to_bytes(resp.into_body()).await?;

    let resp1 = Response::builder().body(Body::from(body_bytes.clone()))?;
    let resp2 = Response::builder().body(Body::from(body_bytes.clone()))?;

    Ok((resp1, resp2))
}

#[derive(Debug)]
struct Stats {
    proxied: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let https = hyper_rustls::HttpsConnector::with_native_roots();
    let client: Client<_, hyper::Body> = Client::builder().build(https);
    let client: Arc<Client<_, hyper::Body>> = Arc::new(client);
    let stats: Arc<RwLock<Stats>> = Arc::new(RwLock::new(Stats { proxied: 0 }));
    let cache: Arc<RwLock<TtlCache<String, Bytes>>> = Arc::new(RwLock::new(TtlCache::new(1000)));
    let addr = SocketAddr::from(([0, 0, 0, 0], 7997));

    let make_svc = make_service_fn(move |_| {
        let client = Arc::clone(&client);
        let stats = Arc::clone(&stats);
        let cache = Arc::clone(&cache);
        async move {
            Ok::<_>(service_fn(move |req| {
                let client = Arc::clone(&client);
                let stats = Arc::clone(&stats);
                let cache = Arc::clone(&cache);
                async move {
                    if req.uri().path() == "/status" {
                        let stats: &Stats = &*stats.read().unwrap();
                        let body: Body = format!("{:?}", stats).into();
                        Ok(Response::new(body))
                    } else {
                        
                        let (req1, req2) = relay_request(req).await?;
                        let path = Mutex::new(req2.uri().path());
                        stats.write().unwrap().proxied += 1;
                        println!(
                            "Hitting {}, it should contain key: {}",
                            req1.uri().path(),
                            cache.read().unwrap().contains_key(req1.uri().path())
                        );
                        println!(
                            "Hitting {}, it should contain: {:?}",
                            req1.uri().path(),
                            cache.read().unwrap().get(req1.uri().path())
                        );
                        if cache.read().unwrap().contains_key(req1.uri().path()) {
                            println!("Cache hit for {}", req1.uri().path());
                            let cache = cache.read().unwrap();
                            let body = cache.get(req1.uri().path()).unwrap();
                            let body = Body::from(body.clone());
                            let mut resp = Response::new(body);
                            resp.headers_mut()
                                .insert(CACHE_CONTROL, "max-age=30".parse().unwrap());
                            return Ok(resp);
                        } else {
                            let resp = client
                                .request(req1)
                                .await
                                .context("requesting to upstream")?;
                            let (resp1, resp2) = clone_response(resp).await?;
                            let resp2_bytes = hyper::body::to_bytes(resp2.into_body()).await?;
                            let path = path.lock().await;
                            // Save cloned request and response to cache
                            cache.write().unwrap().insert(
                                path.to_string(),
                                resp2_bytes,
                                // Cache it for 30 seconds
                                Duration::from_secs(30),
                            );
                            println!(
                                "Cache now contains key at {}: {}",
                                path.to_string(),
                                cache.read().unwrap().contains_key(&path.to_string())
                            );
                            println!(
                                "Saving {}, it should contain: {:?}",
                                path.to_string(),
                                cache.read().unwrap().get(&path.to_string())
                            );
                            return Ok(resp1);
                        }
                    }
                }
            }))
        }
    });
    Server::bind(&addr)
        .serve(make_svc)
        .await
        .context("Running server")?;
    Ok::<()>(())
}
