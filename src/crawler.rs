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

use crate::http_client::{HttpClient, HttpResponse, USER_AGENT};
use crate::link_extractor::{parse_page, PageInfo};

#[derive(Debug, PartialEq, Serialize, JsonSchema)]
enum PageResult {
    #[schemars(title = "Server failure")]
    /// The remote server returned something other than success
    ///
    /// Seen as we are just crawling urls that the user and server have provided
    /// us we report all 400/500 codes here
    ServerFailure {
        #[serde(serialize_with = "serialize_status")]
        #[schemars(with = "u16")]
        /// The http status returned by the server
        status: Status,
        /// The body of the server response
        body: String,
    },
    #[schemars(title = "Error")]
    /// There was another error trying to reach this page
    ///
    /// This could be network errors, some error whilst processing the http
    /// response that was returned.
    Error(String),
    #[schemars(title = "Redirect")]
    /// The server returned a redirect.
    ///
    /// If the redirect was to the same domain we'll follow it.
    Redirect {
        #[serde(serialize_with = "serialize_status")]
        #[schemars(with = "u16")]
        status: Status,
        #[serde(serialize_with = "serialize_url")]
        #[schemars(with = "String")]
        location: Url,
    },
    #[schemars(title = "Crawled")]
    /// The link information for a page
    ///
    /// This is the interesting one. This provides information about all the
    /// links found on the page. The crawler will have continued with all the
    /// internal_links reported.
    Crawled(PageInfo),
    #[schemars(title = "Other content")]
    /// A good response from the server, but not html
    ///
    /// Includes the mimetype of the response
    OtherContent(String),
    #[schemars(title = "Excluded by robots.txt")]
    /// The server had a robots.txt file that asked robots not to visit this
    /// url. So we didn't.
    ExcludedByRobotsTxt,
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

fn crawl_result_example() -> CrawlResult {
    CrawlResult {
        pages: HashMap::from([
            (
                Url::parse("https://example.com/").unwrap(),
                PageResult::Crawled(PageInfo {
                    internal_links: vec![
                        Url::parse("https://example.com/redirect").unwrap(),
                        Url::parse("https://example.com/failure").unwrap(),
                    ],
                    external_links: vec![
                        Url::parse("https://www.iana.org/domains/example").unwrap()
                    ],
                }),
            ),
            (
                Url::parse("https://example.com/redirect").unwrap(),
                PageResult::Redirect {
                    status: Status::Found,
                    location: Url::parse("https://example.com/pdf").unwrap(),
                },
            ),
            (
                Url::parse("https://example.com/pdf").unwrap(),
                PageResult::OtherContent("x-application/pdf".to_string()),
            ),
            (
                Url::parse("https://example.com/failure").unwrap(),
                PageResult::ServerFailure {
                    status: Status::InternalServerError,
                    body: "Internal server error".to_string(),
                },
            ),
            (
                Url::parse("https://example.com/excluded").unwrap(),
                PageResult::ExcludedByRobotsTxt,
            ),
            (
                Url::parse("https://example.com/error").unwrap(),
                PageResult::Error("Connection failure".to_string()),
            ),
        ]),
    }
}

#[derive(Default, PartialEq, Debug, Serialize, JsonSchema)]
#[schemars(example = "crawl_result_example")]
/// The detailed result of crawling a domain
pub struct CrawlResult {
    #[serde(serialize_with = "serialize_page_results")]
    #[schemars(with = "HashMap<String, PageResult>")]
    /// One entry for each page that was reachable.
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

fn url_example() -> String {
    "http://example.com".to_string()
}

#[derive(PartialEq, Debug, Serialize, JsonSchema)]
/// Summary of a single crawl operation progress
pub struct CrawlStatus {
    #[serde(serialize_with = "serialize_url")]
    #[schemars(with = "String", example = "url_example")]
    /// The url passed to the request
    seed: Url,
    /// The number of urls processed
    done: usize,
    /// The number of urls seen but not processed, including both those in the
    /// queue and in being processed. This might go up before we done as we find
    /// new urls.
    todo: usize,
}

#[derive(PartialEq, Debug, Serialize, JsonSchema)]
/// Summary of all crawls in progress on the server
pub struct CrawlerStatus {
    crawls: Vec<CrawlStatus>,
}

#[async_trait]
pub trait Crawler {
    async fn crawl(&self, seed: Url) -> anyhow::Result<CrawlResult>;
    async fn status(&self) -> anyhow::Result<CrawlerStatus>;
}

pub struct ProdCrawler {
    client: Box<dyn HttpClient>,
    crawls: Mutex<HashMap<Url, Arc<Mutex<Crawl>>>>,
}

impl ProdCrawler {
    pub fn new(client: Box<dyn HttpClient>) -> Self {
        ProdCrawler {
            client,
            crawls: Mutex::new(HashMap::new()),
        }
    }
}

struct Crawl {
    seen: HashSet<Url>,
    todo: UnboundedSender<Url>,
    result: CrawlResult,
    robots: Option<String>,
}

impl Crawl {
    fn new(robots: Option<String>) -> (UnboundedReceiver<Url>, Arc<Mutex<Self>>) {
        let (todo, done) = unbounded_channel();
        let this = Crawl {
            seen: Default::default(),
            todo,
            result: Default::default(),
            robots,
        };
        (done, Arc::new(Mutex::new(this)))
    }

    fn allowed_by_robots(&self, url: &Url) -> bool {
        self.robots
            .as_ref()
            .map(|robots| {
                let mut matcher = robotstxt::DefaultMatcher::default();
                matcher.one_agent_allowed_by_robots(&*robots, USER_AGENT, url.as_str())
            })
            .unwrap_or(true)
    }

    fn add_link(&mut self, found: &Url) -> anyhow::Result<()> {
        if !self.seen.contains(found) {
            self.seen.insert(found.clone());

            if self.allowed_by_robots(found) {
                self.todo.send(found.clone())?;
            } else {
                self.result
                    .pages
                    .insert(found.clone(), PageResult::ExcludedByRobotsTxt);
            }
        }
        Ok(())
    }
}

/// std::sync::Mutex is fine to use inside Tokio the docs even recommend it if
/// you don't want to hold locks across await. We actively don't want to hold
/// locks across await. So that's fine. But the PoisonError isn't Send so we
/// convert that in to a generic error early so that we don't have to worry a
/// about it.
fn lock_map_err<T>(lock: &Mutex<T>) -> anyhow::Result<MutexGuard<'_, T>> {
    lock.lock().map_err(|e| anyhow!(e.to_string()))
}

async fn step(
    crawl: Arc<Mutex<Crawl>>,
    client: Box<dyn HttpClient>,
    url: Url,
) -> anyhow::Result<()> {
    match client.get(url.clone()).await {
        Ok(HttpResponse::Html(body)) => {
            info!(
                "Got body to process from {} containing {} chars",
                url,
                body.len()
            );
            let url2 = url.clone();
            let page_info = spawn_blocking(move || parse_page(&url2, &body)).await??;
            let mut crawl = lock_map_err(&crawl)?;
            for found in page_info.internal_links.iter() {
                let mut found = found.clone();
                found.set_fragment(None);
                crawl.add_link(&found)?
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
                .insert(url, PageResult::ServerFailure { status, body: msg });
        }
        Ok(HttpResponse::Redirect(status, target)) => {
            info!("Got redirect from {} to {}", url, target);
            let mut crawl = lock_map_err(&crawl)?;
            if target.domain() == url.domain() {
                crawl.add_link(&target)?
            }
            crawl.result.pages.insert(
                url,
                PageResult::Redirect {
                    status,
                    location: target,
                },
            );
        }
        Ok(HttpResponse::OtherContent(content_type)) => {
            info!("Got non html response containing: {}", content_type);
            let mut crawl = lock_map_err(&crawl)?;
            crawl
                .result
                .pages
                .insert(url, PageResult::OtherContent(content_type));
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

impl ProdCrawler {
    async fn crawl_inner(
        &self,
        rx: UnboundedReceiver<Url>,
        crawl: Arc<Mutex<Crawl>>,
    ) -> anyhow::Result<()> {
        let mut stream = UnboundedReceiverStream::new(rx)
            .map(|url| step(crawl.clone(), self.client.clone(), url))
            .buffer_unordered(20);
        while let Some(()) = stream.try_next().await? {
            let crawl = lock_map_err(&crawl)?;
            if crawl.result.pages.len() == crawl.seen.len() {
                break;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Crawler for ProdCrawler {
    async fn crawl(&self, seed: Url) -> anyhow::Result<CrawlResult> {
        let robots = self.client.get_robots(seed.join("/robots.txt")?).await?;
        let (rx, crawl) = Crawl::new(robots);
        lock_map_err(&crawl)?.add_link(&seed)?;
        lock_map_err(&self.crawls)?.insert(seed.clone(), crawl.clone());
        let result = self.crawl_inner(rx, crawl.clone()).await;
        lock_map_err(&self.crawls)?.remove(&seed);
        // Propagate errors from crawl_inner after removing the crawl from the
        // list of active crawls
        result?;

        Ok(Arc::try_unwrap(crawl)
            .map_err(|_| anyhow!("Extra references to crawl still remain"))?
            .into_inner()?
            .result)
    }

    async fn status(&self) -> anyhow::Result<CrawlerStatus> {
        let crawls = lock_map_err(&self.crawls)?
            .iter()
            .map(|(url, crawl)| {
                let crawl = lock_map_err(crawl)?;
                Ok(CrawlStatus {
                    seed: url.clone(),
                    done: crawl.result.pages.len(),
                    todo: crawl.seen.len() - crawl.result.pages.len(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(CrawlerStatus { crawls })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::Mutex;

    use anyhow::{anyhow, Error};
    use indoc::indoc;
    use rocket::async_trait;
    use rocket::http::Status;
    use rocket::tokio;
    use rocket::tokio::sync::{Barrier, Semaphore};
    use url::Url;

    use crate::crawler::{
        CrawlResult, CrawlStatus, Crawler, CrawlerStatus, PageResult, ProdCrawler,
    };
    use crate::http_client::{HttpClient, HttpResponse};
    use crate::link_extractor::PageInfo;
    use crate::test_util;
    use crate::test_util::PageInfoBuilder;

    fn crawl_result<const N: usize>(pages: [(&str, PageResult); N]) -> CrawlResult {
        CrawlResult {
            pages: HashMap::from(pages.map(|(url, result)| (Url::parse(url).unwrap(), result))),
        }
    }

    struct DummyResponse {
        respond: Box<dyn Fn() -> Result<HttpResponse, anyhow::Error> + Send + Sync>,
        hit_count: u32,
        stop: Option<Arc<Barrier>>,
        restart: Option<Arc<Semaphore>>,
    }

    impl DummyResponse {
        fn new(
            respond: impl Fn() -> Result<HttpResponse, anyhow::Error> + Send + Sync + 'static,
        ) -> Self {
            DummyResponse {
                respond: Box::new(respond),
                hit_count: 0,
                stop: None,
                restart: None,
            }
        }

        fn with_restart(
            respond: impl Fn() -> Result<HttpResponse, anyhow::Error> + Send + Sync + 'static,
            stop: Arc<Barrier>,
            restart: Arc<Semaphore>,
        ) -> Self {
            DummyResponse {
                respond: Box::new(respond),
                hit_count: 0,
                stop: Some(stop),
                restart: Some(restart),
            }
        }
    }

    #[derive(Default, Clone)]
    struct DummyHttpClient {
        // anyhow::Error isn't Clone, so we can't just build the result and
        // store it in the map, instead we store a closure to build it when we
        // need it.
        responses: Arc<Mutex<HashMap<Url, DummyResponse>>>,
        robots: Option<String>,
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
                DummyResponse::new(move || Ok(HttpResponse::Html(body.clone()))),
            );
        }

        pub fn add_other_content<S: Into<String>>(&mut self, url: &str, content_type: S) {
            let content_type: String = content_type.into();
            self.insert(
                url,
                DummyResponse::new(move || Ok(HttpResponse::OtherContent(content_type.clone()))),
            );
        }

        pub fn get_hit_counts(&self) -> HashMap<String, u32> {
            self.responses
                .lock()
                .unwrap()
                .iter()
                .map(|(url, data)| (url.clone().to_string(), data.hit_count))
                .collect()
        }
    }

    #[async_trait]
    impl HttpClient for DummyHttpClient {
        async fn get(&self, url: Url) -> Result<HttpResponse, anyhow::Error> {
            let (response, stop, restart) = {
                let mut responses = self.responses.lock().unwrap();
                let response = responses
                    .get_mut(&url)
                    .unwrap_or_else(|| panic!("No response available for url {}", url));
                response.hit_count += 1;
                (
                    (response.respond)(),
                    response.stop.clone(),
                    response.restart.clone(),
                )
            };
            if let Some(stop) = stop {
                stop.wait().await;
            }
            if let Some(restart) = restart {
                restart.acquire().await.unwrap().forget();
            }
            response
        }

        async fn get_robots(&self, _url: Url) -> Result<Option<String>, Error> {
            Ok(self.robots.clone())
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
        test_util::leak_setup_logging();
        let mut dummy_client = DummyHttpClient::default();
        let status = Status::InternalServerError;
        let msg = "Internal server error";
        let seed = "https://example.com/start";
        dummy_client.add_server_failure(seed, status, msg);

        let result = do_crawl(&dummy_client, seed).await;

        assert_eq!(
            result,
            crawl_result([(
                seed,
                PageResult::ServerFailure {
                    status,
                    body: msg.to_string()
                }
            ),])
        )
    }

    #[tokio::test]
    async fn reports_single_network_error() {
        test_util::leak_setup_logging();
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
        test_util::leak_setup_logging();
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
        test_util::leak_setup_logging();
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
                    PageResult::Redirect {
                        status: Status::Found,
                        location: Url::parse(target).unwrap()
                    }
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
        test_util::leak_setup_logging();
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
        test_util::leak_setup_logging();
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
        test_util::leak_setup_logging();
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
        test_util::leak_setup_logging();
        let mut dummy_client = DummyHttpClient::default();
        let redirect = "https://example.com/redirect";
        let target = "https://notexample.com/target";
        dummy_client.add_redirect(redirect, Status::Found, target);

        let result = do_crawl(&dummy_client, redirect).await;

        assert_eq!(
            result,
            crawl_result([(
                redirect,
                PageResult::Redirect {
                    status: Status::Found,
                    location: Url::parse(target).unwrap()
                }
            ),])
        )
    }

    #[tokio::test]
    async fn dont_revisit_due_to_redirect() {
        test_util::leak_setup_logging();
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
                    PageResult::Redirect {
                        status: Status::Found,
                        location: Url::parse(seed).unwrap()
                    }
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
        test_util::leak_setup_logging();
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
                    PageResult::Redirect {
                        status: Status::Found,
                        location: Url::parse(target).unwrap()
                    }
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

    #[tokio::test]
    async fn dont_visit_fragments_separately() {
        test_util::leak_setup_logging();
        let mut dummy_client = DummyHttpClient::default();
        let page = "https://example.com/page";
        let link1 = format!("{}#link1", page);
        let link2 = format!("{}#link2", page);
        dummy_client.add_page(page, html_with_links([&link1, &link2]));

        let result = do_crawl(&dummy_client, page).await;

        assert_eq!(
            result,
            crawl_result([(page, crawled_internal([&link1, &link2])),])
        );
        assert_eq!(
            dummy_client
                .get_hit_counts()
                .into_values()
                .collect::<Vec<_>>(),
            vec![1]
        )
    }

    #[tokio::test]
    async fn ignores_non_html() {
        test_util::leak_setup_logging();
        let mut dummy_client = DummyHttpClient::default();
        let pdf = "https://example.com/thing.pdf";
        let content_type = "x-application/pdf";
        dummy_client.add_other_content(pdf, content_type);

        let result = do_crawl(&dummy_client, pdf).await;

        assert_eq!(
            result,
            crawl_result([(pdf, PageResult::OtherContent(content_type.to_string())),])
        );
    }

    #[tokio::test]
    async fn ignores_link_to_page_excluded_by_robots_txt() {
        test_util::leak_setup_logging();
        let mut dummy_client = DummyHttpClient::default();
        let page = "https://example.com/page";
        let excluded = "https://example.com/excluded";
        dummy_client.robots = Some(
            indoc! { r#"
                User-agent: *
                Disallow: /excluded
            "#}
            .to_string(),
        );
        dummy_client.add_page(page, html_with_links([excluded]));
        dummy_client.add_page(excluded, "");

        let result = do_crawl(&dummy_client, page).await;

        assert_eq!(
            result,
            crawl_result([
                (page, crawled_internal([excluded])),
                (excluded, PageResult::ExcludedByRobotsTxt)
            ])
        );
        assert_eq!(dummy_client.get_hit_counts()[excluded], 0);
    }

    #[tokio::test]
    async fn get_some_status() {
        test_util::leak_setup_logging();
        let mut dummy_client = DummyHttpClient::default();
        let start = "https://example.com/start";
        let page1 = "https://example.com/page1";
        let page2 = "https://example.com/page2";

        let stop = Arc::new(Barrier::new(3));
        let restart = Arc::new(Semaphore::new(0));

        dummy_client.add_page(start, html_with_links([page1, page2]));
        dummy_client.insert(
            page1,
            DummyResponse::with_restart(
                move || Ok(HttpResponse::Html("".to_string())),
                stop.clone(),
                restart.clone(),
            ),
        );
        dummy_client.insert(
            page2,
            DummyResponse::with_restart(
                move || Ok(HttpResponse::Html("".to_string())),
                stop.clone(),
                restart.clone(),
            ),
        );

        let crawler = Arc::new(ProdCrawler::new(HttpClient::clone(&dummy_client)));
        let crawler2 = crawler.clone();
        let crawl = tokio::spawn(async move { crawler.crawl(Url::parse(start).unwrap()).await });

        stop.wait().await;

        let status = crawler2.status().await.unwrap();

        restart.add_permits(2);

        assert_eq!(
            status,
            CrawlerStatus {
                crawls: vec![CrawlStatus {
                    seed: Url::parse(start).unwrap(),
                    done: 1,
                    todo: 2
                }]
            }
        );

        crawl.await.unwrap().unwrap();

        let status = crawler2.status().await.unwrap();

        assert_eq!(status, CrawlerStatus { crawls: vec![] });
    }

    #[tokio::test]
    async fn crawl_tracking_is_removed_if_theres_an_error() {
        test_util::leak_setup_logging();
        let mut dummy_client = DummyHttpClient::default();
        let start = "https://example.com/start";
        let page1 = "https://example.com/page1";
        let page2 = "https://example.com/page2";

        let stop = Arc::new(Barrier::new(3));
        let restart = Arc::new(Semaphore::new(0));

        dummy_client.add_page(start, html_with_links([page1, page2]));
        dummy_client.insert(
            page1,
            DummyResponse::with_restart(
                move || Ok(HttpResponse::Html("".to_string())),
                stop.clone(),
                restart.clone(),
            ),
        );
        dummy_client.insert(
            page2,
            DummyResponse::with_restart(
                move || Ok(HttpResponse::Html("".to_string())),
                stop.clone(),
                restart.clone(),
            ),
        );

        let crawler = Arc::new(ProdCrawler::new(HttpClient::clone(&dummy_client)));
        let crawler2 = crawler.clone();
        let crawler3 = crawler.clone();
        let crawl = tokio::spawn(async move { crawler.crawl(Url::parse(start).unwrap()).await });

        stop.wait().await;

        std::thread::spawn(move || {
            let crawl = crawler3
                .crawls
                .lock()
                .unwrap()
                .get(&Url::parse(start).unwrap())
                .unwrap()
                .clone();
            let _lock = crawl.lock().unwrap();
            panic!("Test panic to poison mutex");
        })
        .join()
        .expect_err("Expect panic thread to end in err");

        restart.add_permits(2);

        crawl
            .await
            .unwrap()
            .expect_err("Expected crawl to end in err");

        let status = crawler2.status().await.unwrap();

        assert_eq!(status, CrawlerStatus { crawls: vec![] });
    }
}

pub type CrawlerState = Box<dyn Crawler + Send + Sync>;
