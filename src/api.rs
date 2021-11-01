use reqwest::Url;
use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{get, State};
use rocket_okapi::openapi;

use crate::better_logging::ReqLogger;
use crate::crawler::CrawlerState;
use crate::crawler::{CrawlResult, CrawlerStatus};

#[openapi]
#[get("/crawl/<seed>")]
/// Crawl a domain starting with <seed>
///
/// Returns information about all the links on every page reachable from seed
pub async fn crawl(
    logger: ReqLogger,
    crawler: &State<CrawlerState>,
    seed: String,
) -> Result<Json<CrawlResult>, (Status, String)> {
    logger
        .scope(async move {
            let seed = Url::parse(&seed).map_err(|e| (Status::BadRequest, e.to_string()))?;
            let result = crawler
                .crawl(seed)
                .await
                .map_err(|e| (Status::InternalServerError, e.to_string()))?;
            Ok(Json(result))
        })
        .await
}

#[openapi]
#[get("/status")]
/// Get a summary of all the crawl operations in progress on the server
pub async fn status(
    logger: ReqLogger,
    crawler: &State<CrawlerState>,
) -> Result<Json<CrawlerStatus>, (Status, String)> {
    logger
        .scope(async move {
            let result = crawler
                .status()
                .await
                .map_err(|e| (Status::InternalServerError, e.to_string()))?;
            Ok(Json(result))
        })
        .await
}

#[cfg(test)]
mod tests {
    use rocket::local::blocking::Client;
    use rocket::{async_trait, routes};
    use url::Url;

    use super::*;
    use crate::better_logging::BetterLogging;
    use crate::crawler::{CrawlResult, CrawlStatus, Crawler, CrawlerState, CrawlerStatus};
    use crate::test_util::{crawl_result, crawled_internal, leak_setup_logging};
    use rocket::serde::json::Value;

    #[derive(Default)]
    struct DummyCrawler {
        crawl: Option<Box<dyn Fn() -> anyhow::Result<CrawlResult> + Send + Sync>>,
        status: Option<Box<dyn Fn() -> anyhow::Result<CrawlerStatus> + Send + Sync>>,
    }

    impl DummyCrawler {
        fn with_crawl(
            crawl: impl Fn() -> anyhow::Result<CrawlResult> + Send + Sync + 'static,
        ) -> Self {
            DummyCrawler {
                crawl: Some(Box::new(crawl)),
                status: None,
            }
        }
        fn with_status(
            status: impl Fn() -> anyhow::Result<CrawlerStatus> + Send + Sync + 'static,
        ) -> Self {
            DummyCrawler {
                crawl: None,
                status: Some(Box::new(status)),
            }
        }
    }

    #[async_trait]
    impl Crawler for DummyCrawler {
        async fn crawl(&self, _seed: Url) -> anyhow::Result<CrawlResult> {
            (self.crawl.as_ref().unwrap())()
        }

        async fn status(&self) -> anyhow::Result<CrawlerStatus> {
            (self.status.as_ref().unwrap())()
        }
    }

    fn crawl_url(data: &str) -> String {
        format!("/crawl/{}", urlencoding::encode(data))
    }

    fn build_client(crawler: DummyCrawler) -> Client {
        leak_setup_logging();
        let rocket = rocket::build()
            .attach(BetterLogging {})
            .manage(CrawlerState::from(Box::new(crawler)))
            .mount("/", routes![crawl, status]);
        Client::tracked(rocket).unwrap()
    }

    #[test]
    fn crawl_return_bad_request_for_non_url() {
        let client = build_client(DummyCrawler::default());

        let url = "garbage";
        let response = client.get(crawl_url(url)).dispatch();

        assert_eq!(response.status(), Status::BadRequest);
        assert_eq!(
            response.into_string().unwrap(),
            Url::parse(url).unwrap_err().to_string()
        )
    }

    #[test]
    fn crawl_return_internal_error_from_crawler() {
        let error = "Something went wrong TEST";
        let client = build_client(DummyCrawler::with_crawl(move || anyhow::bail!(error)));

        let response = client.get(crawl_url("https://example.com")).dispatch();

        assert_eq!(response.status(), Status::InternalServerError);
        assert_eq!(response.into_string().unwrap(), error);
    }

    #[test]
    fn crawl_returns_result_from_crawler() {
        let url = "https://example.com/";
        let client = build_client(DummyCrawler::with_crawl(move || {
            Ok(crawl_result([(url, crawled_internal([url]))]))
        }));

        let response = client.get(crawl_url(url)).dispatch();

        assert_eq!(response.status(), Status::Ok);
        let json: Value = response.into_json().unwrap();
        println!("{:?}", json);
        assert_eq!(json["pages"][url]["Crawled"]["internal_links"][0], url);
    }

    #[test]
    fn status_return_internal_error_from_crawler() {
        let error = "Something went wrong TEST";
        let client = build_client(DummyCrawler::with_status(move || anyhow::bail!(error)));

        let response = client.get("/status").dispatch();

        assert_eq!(response.status(), Status::InternalServerError);
        assert_eq!(response.into_string().unwrap(), error);
    }

    #[test]
    fn status_returns_result_from_crawler() {
        let url = "https://example.com/";
        let client = build_client(DummyCrawler::with_status(move || {
            Ok(CrawlerStatus {
                crawls: vec![CrawlStatus {
                    seed: Url::parse(url).unwrap(),
                    done: 2,
                    todo: 10,
                }],
            })
        }));

        let response = client.get("/status").dispatch();

        assert_eq!(response.status(), Status::Ok);
        let json: Value = response.into_json().unwrap();
        println!("{:?}", json);
        assert_eq!(json["crawls"][0]["seed"], url);
    }
}
