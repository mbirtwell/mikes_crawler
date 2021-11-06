use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::anyhow;
use rocket::async_trait;
use rocket::futures::{StreamExt, TryStreamExt};
use rocket::http::Status;
use rocket::serde::ser::SerializeMap;
use rocket::serde::{Serialize, Serializer};
use rocket::tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use rocket::tokio::task::spawn_blocking;
use schemars::JsonSchema;
use slog_scope::info;
use tokio_stream::wrappers::UnboundedReceiverStream;
use url::Url;

use crate::http_client::{HttpClient, HttpResponse};
use crate::link_extractor::{parse_page, PageInfo};

#[derive(Debug, PartialEq, Serialize, JsonSchema)]
enum PageResult {
    ServerFailure(
        #[serde(serialize_with = "serialize_status")]
        #[schemars(with = "u16")]
        Status,
        String,
    ),
    Error(String),
    #[allow(dead_code)]
    Redirect(
        #[serde(serialize_with = "serialize_status")]
        #[schemars(with = "u16")]
        Status,
        #[serde(serialize_with = "serialize_url")]
        #[schemars(with = "String")]
        Url,
    ),
    Crawled(PageInfo),
}

fn serialize_status<S>(data: &Status, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_u16(data.code)
}

fn serialize_url<S>(data: &Url, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(data.as_str())
}

#[derive(Default, PartialEq, Debug, Serialize, JsonSchema)]
pub struct CrawlResult {
    #[serde(serialize_with = "serialize_page_results")]
    #[schemars(with = "HashMap<String, PageResult>")]
    pages: HashMap<Url, PageResult>,
}

fn serialize_page_results<S>(
    data: &HashMap<Url, PageResult>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = serializer.serialize_map(Some(data.len()))?;
    for (url, page_info) in data {
        seq.serialize_entry(url.as_str(), page_info)?;
    }
    seq.end()
}

#[async_trait]
pub trait Crawler {
    async fn crawl(&self, seed: Url) -> anyhow::Result<CrawlResult>;
}

pub struct ProdCrawler {
    client: Box<dyn HttpClient>,
}

impl ProdCrawler {
    pub fn new(client: Box<dyn HttpClient>) -> Self {
        ProdCrawler { client }
    }
}

struct Crawl {
    seen: HashSet<Url>,
    todo: UnboundedSender<Url>,
    result: CrawlResult,
}

impl Crawl {
    fn new() -> (UnboundedReceiver<Url>, Arc<Mutex<Self>>) {
        let (todo, done) = unbounded_channel();
        let this = Crawl {
            seen: Default::default(),
            todo,
            result: Default::default(),
        };
        (done, Arc::new(Mutex::new(this)))
    }

    fn add_link(&mut self, found: &Url) -> anyhow::Result<()> {
        if !self.seen.contains(found) {
            self.seen.insert(found.clone());
            self.todo.send(found.clone())?;
        }
        Ok(())
    }
}

/// std::sync::Mutex is fine to use inside Tokio the docs even recommend it if
/// you don't want to hold locks across await. We actively don't want to hold
/// locks across await. So that's fine. But the PoisonError isn't Send so we
/// convert that in to a generic error early so that we don't have to worry a
/// about it.
fn lock_map_err<T>(lock: &Arc<Mutex<T>>) -> anyhow::Result<MutexGuard<'_, T>> {
    lock.lock().map_err(|e| anyhow!(e.to_string()))
}

async fn step(
    crawl: Arc<Mutex<Crawl>>,
    client: Box<dyn HttpClient>,
    url: Url,
) -> anyhow::Result<()> {
    match client.get(url.clone()).await {
        Ok(HttpResponse::Ok(body)) => {
            info!(
                "Got body to process from {} containing {} chars",
                url,
                body.len()
            );
            let url2 = url.clone();
            let page_info = spawn_blocking(move || parse_page(&url2, &body)).await??;
            let mut crawl = lock_map_err(&crawl)?;
            for found in page_info.internal_links.iter() {
                crawl.add_link(found)?
            }
            crawl
                .result
                .pages
                .insert(url, PageResult::Crawled(page_info));
        }
        Ok(HttpResponse::ServerFailure(status, msg)) => {
            info!(
                "Got response with status {}: Not processing the body",
                status
            );
            let mut crawl = lock_map_err(&crawl)?;
            crawl
                .result
                .pages
                .insert(url, PageResult::ServerFailure(status, msg));
        }
        Ok(HttpResponse::Redirect(status, target)) => {
            info!("Got redirect from {} to {}", url, target);
            let mut crawl = lock_map_err(&crawl)?;
            if target.domain() == url.domain() {
                crawl.add_link(&target)?
            }
            crawl
                .result
                .pages
                .insert(url, PageResult::Redirect(status, target.clone()));
        }
        Err(msg) => {
            info!("Error trying to make request or process response: {}", msg);
            let mut crawl = lock_map_err(&crawl)?;
            crawl
                .result
                .pages
                .insert(url, PageResult::Error(msg.to_string()));
        }
    }
    Ok(())
}

#[async_trait]
impl Crawler for ProdCrawler {
    async fn crawl(&self, seed: Url) -> anyhow::Result<CrawlResult> {
        let (rx, crawl) = Crawl::new();
        lock_map_err(&crawl)?.add_link(&seed)?;
        {
            let crawl2 = crawl.clone();
            let mut stream = UnboundedReceiverStream::new(rx)
                .map(|url| step(crawl2.clone(), self.client.clone(), url))
                .buffer_unordered(20);
            while let Some(()) = stream.try_next().await? {
                let crawl = lock_map_err(&crawl)?;
                if crawl.result.pages.len() == crawl.seen.len() {
                    break;
                }
            }
        }

        Ok(Arc::try_unwrap(crawl)
            .map_err(|_| anyhow!("Extra references to crawl still remain"))?
            .into_inner()?
            .result)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::Once;

    use anyhow::anyhow;
    use indoc::indoc;
    use rocket::async_trait;
    use rocket::http::Status;
    use rocket::tokio;
    use url::Url;

    use crate::crawler::{CrawlResult, Crawler, PageResult, ProdCrawler};
    use crate::http_client::{HttpClient, HttpResponse};
    use crate::link_extractor::PageInfo;
    use crate::setup_logging;
    use crate::test_util::PageInfoBuilder;

    static INIT: Once = Once::new();

    fn setup() {
        INIT.call_once(|| {
            Box::leak(Box::new(setup_logging()));
        })
    }

    fn crawl_result<const N: usize>(pages: [(&str, PageResult); N]) -> CrawlResult {
        CrawlResult {
            pages: HashMap::from(pages.map(|(url, result)| (Url::parse(url).unwrap(), result))),
        }
    }

    struct DummyResponse {
        respond: Box<dyn Fn() -> Result<HttpResponse, anyhow::Error> + Send + Sync>,
        hit_count: u32,
    }

    impl DummyResponse {
        fn new(
            respond: impl Fn() -> Result<HttpResponse, anyhow::Error> + Send + Sync + 'static,
        ) -> Self {
            DummyResponse {
                respond: Box::new(respond),
                hit_count: 0,
            }
        }
    }

    #[derive(Default, Clone)]
    struct DummyHttpClient {
        // anyhow::Error isn't Clone, so we can't just build the result and
        // store it in the map, instead we store a closure to build it when we
        // need it.
        responses: Arc<Mutex<HashMap<Url, DummyResponse>>>,
    }

    impl DummyHttpClient {
        fn insert(&mut self, url: &str, dummy_response: DummyResponse) {
            self.responses
                .lock()
                .unwrap()
                .insert(Url::parse(url).unwrap(), dummy_response);
        }
        pub fn add_server_failure(&mut self, url: &str, status: Status, msg: &'static str) {
            self.insert(
                url,
                DummyResponse::new(move || {
                    Ok(HttpResponse::ServerFailure(status, msg.to_string()))
                }),
            );
        }
        pub fn add_network_failure(&mut self, url: &str, msg: &'static str) {
            self.insert(url, DummyResponse::new(move || Err(anyhow!(msg))));
        }
        pub fn add_redirect(&mut self, url: &str, status: Status, target: &'static str) {
            self.insert(
                url,
                DummyResponse::new(move || {
                    Ok(HttpResponse::Redirect(status, Url::parse(target).unwrap()))
                }),
            );
        }

        pub fn add_page<S: Into<String>>(&mut self, url: &str, body: S) {
            let body: String = body.into();
            self.insert(
                url,
                DummyResponse::new(move || Ok(HttpResponse::Ok(body.clone()))),
            );
        }

        pub fn get_hit_counts(&self) -> HashMap<Url, u32> {
            self.responses
                .lock()
                .unwrap()
                .iter()
                .map(|(url, data)| (url.clone(), data.hit_count))
                .collect()
        }
    }

    #[async_trait]
    impl HttpClient for DummyHttpClient {
        async fn get(&self, url: Url) -> Result<HttpResponse, anyhow::Error> {
            let mut responses = self.responses.lock().unwrap();
            let response = responses
                .get_mut(&url)
                .unwrap_or_else(|| panic!("No response available for url {}", url));
            response.hit_count += 1;
            (response.respond)()
        }

        fn clone(&self) -> Box<dyn HttpClient> {
            Box::new(Clone::clone(self))
        }
    }

    async fn do_crawl(dummy_client: &DummyHttpClient, seed: &str) -> CrawlResult {
        let crawler = ProdCrawler::new(HttpClient::clone(dummy_client));

        crawler.crawl(Url::parse(seed).unwrap()).await.unwrap()
    }

    fn html_with_links<const N: usize>(links: [&str; N]) -> String {
        format!(
            indoc! {r##"
            <!DOCTYPE html>
            <html>
                <head></head>
                <body>
                    {}
                </body>
            </html>
        "##},
            links
                .iter()
                .map(|l| format!(r#"<a href="{}">Something</a>"#, l))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }

    fn crawled_internal<const N: usize>(links: [&str; N]) -> PageResult {
        PageResult::Crawled(PageInfoBuilder::new().internal_links(links).build())
    }

    #[tokio::test]
    async fn reports_single_server_error() {
        setup();
        let mut dummy_client = DummyHttpClient::default();
        let status = Status::InternalServerError;
        let msg = "Internal server error";
        let seed = "https://example.com/start";
        dummy_client.add_server_failure(seed, status, msg);

        let result = do_crawl(&dummy_client, seed).await;

        assert_eq!(
            result,
            crawl_result([(seed, PageResult::ServerFailure(status, msg.to_string())),])
        )
    }

    #[tokio::test]
    async fn reports_single_network_error() {
        setup();
        let mut dummy_client = DummyHttpClient::default();
        let msg = "Connection failed";
        let seed = "https://example.com/start";
        dummy_client.add_network_failure(seed, msg);

        let result = do_crawl(&dummy_client, seed).await;

        assert_eq!(
            result,
            crawl_result([(seed, PageResult::Error(msg.to_string()),)])
        )
    }

    #[tokio::test]
    async fn reports_single_page_with_external_links() {
        setup();
        let mut dummy_client = DummyHttpClient::default();
        let seed = "https://example.com/start";
        let external_link = "https://notexample.com/another";
        let html = html_with_links([external_link]);
        dummy_client.add_page(seed, html);

        let result = do_crawl(&dummy_client, seed).await;

        assert_eq!(
            result,
            crawl_result([(
                seed,
                PageResult::Crawled(
                    PageInfoBuilder::new()
                        .external_links([external_link])
                        .build()
                )
            )])
        )
    }

    #[tokio::test]
    async fn reports_redirect_and_target() {
        setup();
        let mut dummy_client = DummyHttpClient::default();
        let redirect = "https://example.com/redirect";
        let target = "https://example.com/target";
        let external_link = "https://notexample.com/another";
        let html = html_with_links([external_link]);
        dummy_client.add_redirect(redirect, Status::Found, target);
        dummy_client.add_page(target, html);

        let result = do_crawl(&dummy_client, redirect).await;

        assert_eq!(
            result,
            crawl_result([
                (
                    redirect,
                    PageResult::Redirect(Status::Found, Url::parse(target).unwrap())
                ),
                (
                    target,
                    PageResult::Crawled(
                        PageInfoBuilder::new()
                            .external_links([external_link])
                            .build()
                    )
                )
            ])
        )
    }

    #[tokio::test]
    async fn follows_multiple_internal_links() {
        setup();
        let mut dummy_client = DummyHttpClient::default();
        let seed = "https://example.com/start";
        let link1 = "https://example.com/link1";
        let link2 = "https://example.com/link2";
        let html = html_with_links([link1, link2]);
        dummy_client.add_page(seed, html);
        dummy_client.add_page(link1, "");
        dummy_client.add_page(link2, "");

        let result = do_crawl(&dummy_client, seed).await;

        assert_eq!(
            result,
            crawl_result([
                (seed, crawled_internal([link1, link2]),),
                (link1, PageResult::Crawled(PageInfo::default()),),
                (link2, PageResult::Crawled(PageInfo::default()),)
            ])
        )
    }

    #[tokio::test]
    async fn stop_after_loop_of_pages() {
        setup();
        let mut dummy_client = DummyHttpClient::default();
        let seed = "https://example.com/start";
        let link1 = "https://example.com/link1";
        let link2 = "https://example.com/link2";

        dummy_client.add_page(seed, html_with_links([link1]));
        dummy_client.add_page(link1, html_with_links([link2]));
        dummy_client.add_page(link2, html_with_links([seed]));

        let result = do_crawl(&dummy_client, seed).await;

        assert_eq!(
            result,
            crawl_result([
                (seed, crawled_internal([link1])),
                (link1, crawled_internal([link2])),
                (link2, crawled_internal([seed]))
            ])
        )
    }

    #[tokio::test]
    async fn stop_after_parallel_loop_of_pages() {
        setup();
        let mut dummy_client = DummyHttpClient::default();
        let seed = "https://example.com/start";
        let link1 = "https://example.com/link1";
        let link2 = "https://example.com/link2";

        dummy_client.add_page(seed, html_with_links([link1, link2]));
        dummy_client.add_page(link1, html_with_links([seed, link2]));
        dummy_client.add_page(link2, html_with_links([seed, link1]));

        let result = do_crawl(&dummy_client, seed).await;

        assert_eq!(
            result,
            crawl_result([
                (seed, crawled_internal([link1, link2])),
                (link1, crawled_internal([seed, link2])),
                (link2, crawled_internal([seed, link1]))
            ])
        );
        assert_eq!(
            dummy_client
                .get_hit_counts()
                .into_values()
                .collect::<Vec<_>>(),
            vec![1, 1, 1]
        )
    }

    #[tokio::test]
    async fn dont_follow_external_redirects() {
        setup();
        let mut dummy_client = DummyHttpClient::default();
        let redirect = "https://example.com/redirect";
        let target = "https://notexample.com/target";
        dummy_client.add_redirect(redirect, Status::Found, target);

        let result = do_crawl(&dummy_client, redirect).await;

        assert_eq!(
            result,
            crawl_result([(
                redirect,
                PageResult::Redirect(Status::Found, Url::parse(target).unwrap())
            ),])
        )
    }

    #[tokio::test]
    async fn dont_revisit_due_to_redirect() {
        setup();
        let mut dummy_client = DummyHttpClient::default();
        let seed = "https://example.com/start";
        let redirect = "https://example.com/redirect";
        dummy_client.add_page(seed, html_with_links([redirect]));
        dummy_client.add_redirect(redirect, Status::Found, seed);

        let result = do_crawl(&dummy_client, seed).await;

        assert_eq!(
            result,
            crawl_result([
                (seed, crawled_internal([redirect])),
                (
                    redirect,
                    PageResult::Redirect(Status::Found, Url::parse(seed).unwrap())
                ),
            ])
        );
        assert_eq!(
            dummy_client
                .get_hit_counts()
                .into_values()
                .collect::<Vec<_>>(),
            vec![1, 1]
        )
    }

    #[tokio::test]
    async fn dont_revisit_if_found_from_redirect() {
        setup();
        let mut dummy_client = DummyHttpClient::default();
        let redirect = "https://example.com/redirect";
        let target = "https://example.com/target";
        let back = "https://example.com/back";
        dummy_client.add_redirect(redirect, Status::Found, target);
        dummy_client.add_page(target, html_with_links([back]));
        dummy_client.add_page(back, html_with_links([target]));

        let result = do_crawl(&dummy_client, redirect).await;

        assert_eq!(
            result,
            crawl_result([
                (
                    redirect,
                    PageResult::Redirect(Status::Found, Url::parse(target).unwrap())
                ),
                (target, crawled_internal([back])),
                (back, crawled_internal([target]))
            ])
        );
        assert_eq!(
            dummy_client
                .get_hit_counts()
                .into_values()
                .collect::<Vec<_>>(),
            vec![1, 1, 1]
        )
    }
}
