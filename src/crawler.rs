use std::collections::HashMap;

use rocket::async_trait;
use rocket::http::Status;
use rocket::serde::ser::SerializeMap;
use rocket::serde::{Serialize, Serializer};
use schemars::JsonSchema;
use slog_scope::info;
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
    async fn crawl(&self, seed: Url) -> CrawlResult;
}

pub struct ProdCrawler {
    client: Box<dyn HttpClient>,
}

impl ProdCrawler {
    pub fn new(client: Box<dyn HttpClient>) -> Self {
        ProdCrawler { client }
    }
}

#[async_trait]
impl Crawler for ProdCrawler {
    async fn crawl(&self, seed: Url) -> CrawlResult {
        let mut result = CrawlResult::default();
        match self.client.get(seed.clone()).await {
            Ok(HttpResponse::Ok(body)) => {
                info!(
                    "Got body to process from {} containing {} chars",
                    seed,
                    body.len()
                );
                let page_info = parse_page(&seed, &body);
                result.pages.insert(seed, PageResult::Crawled(page_info));
            }
            Ok(HttpResponse::ServerFailure(status, msg)) => {
                info!(
                    "Got response with status {}: Not processing the body",
                    status
                );
                result
                    .pages
                    .insert(seed, PageResult::ServerFailure(status, msg));
            }
            Ok(HttpResponse::Redirect(_status, _url)) => {
                todo!()
            }
            Err(msg) => {
                info!("Error trying to make request or process response: {}", msg);
                result
                    .pages
                    .insert(seed, PageResult::Error(msg.to_string()));
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use anyhow::anyhow;
    use indoc::indoc;
    use rocket::async_trait;
    use rocket::http::Status;
    use rocket::tokio;
    use url::Url;

    use crate::crawler::{CrawlResult, Crawler, PageResult, ProdCrawler};
    use crate::http_client::{HttpClient, HttpResponse};
    use crate::test_util::PageInfoBuilder;

    fn crawl_result<const N: usize>(pages: [(&str, PageResult); N]) -> CrawlResult {
        CrawlResult {
            pages: HashMap::from(pages.map(|(url, result)| (Url::parse(url).unwrap(), result))),
        }
    }

    #[derive(Default)]
    struct DummyHttpClient {
        // anyhow::Error isn't Clone, so we can't just build the result and
        // store it in the map, instead we store a closure to build it when we
        // need it.
        responses: HashMap<Url, Box<dyn Fn() -> Result<HttpResponse, anyhow::Error> + Send + Sync>>,
    }

    impl DummyHttpClient {
        pub fn add_server_failure(&mut self, url: &str, status: Status, msg: &'static str) {
            self.responses.insert(
                Url::parse(url).unwrap(),
                Box::new(move || Ok(HttpResponse::ServerFailure(status, msg.to_string()))),
            );
        }
        pub fn add_network_failure(&mut self, url: &str, msg: &'static str) {
            self.responses.insert(
                Url::parse(url).unwrap(),
                Box::new(move || Err(anyhow!(msg))),
            );
        }
        pub fn add_page(&mut self, url: &str, body: &'static str) {
            self.responses.insert(
                Url::parse(url).unwrap(),
                Box::new(|| Ok(HttpResponse::Ok(body.to_string()))),
            );
        }
    }

    #[async_trait]
    impl HttpClient for DummyHttpClient {
        async fn get(&self, url: Url) -> Result<HttpResponse, anyhow::Error> {
            (self
                .responses
                .get(&url)
                .unwrap_or_else(|| panic!("No response available for url {}", url)))()
        }
    }

    async fn do_crawl(dummy_client: DummyHttpClient, seed: &str) -> CrawlResult {
        let crawler = ProdCrawler::new(Box::new(dummy_client));

        crawler.crawl(Url::parse(seed).unwrap()).await
    }

    #[tokio::test]
    async fn reports_single_server_error() {
        let mut dummy_client = DummyHttpClient::default();
        let status = Status::InternalServerError;
        let msg = "Internal server error";
        let seed = "https://example.com/start";
        dummy_client.add_server_failure(seed, status, msg);

        let result = do_crawl(dummy_client, seed).await;

        assert_eq!(
            result,
            crawl_result([(seed, PageResult::ServerFailure(status, msg.to_string())),])
        )
    }

    #[tokio::test]
    async fn reports_single_network_error() {
        let mut dummy_client = DummyHttpClient::default();
        let msg = "Connection failed";
        let seed = "https://example.com/start";
        dummy_client.add_network_failure(seed, msg);

        let result = do_crawl(dummy_client, seed).await;

        assert_eq!(
            result,
            crawl_result([(seed, PageResult::Error(msg.to_string()),)])
        )
    }

    #[tokio::test]
    async fn reports_single_page_with_external_links() {
        let mut dummy_client = DummyHttpClient::default();
        let seed = "https://example.com/start";
        let html = indoc! {r##"
            <!DOCTYPE html>
            <html>
                <head></head>
                <body>
                    <a href="https://notexample.com/another">Interesting</a>
                </body>
            </html
        "##};
        dummy_client.add_page(seed, html);

        let result = do_crawl(dummy_client, seed).await;

        assert_eq!(
            result,
            crawl_result([(
                seed,
                PageResult::Crawled(
                    PageInfoBuilder::new()
                        .external_links(["https://notexample.com/another"])
                        .build()
                )
            )])
        )
    }
}
