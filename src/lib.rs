#![feature(iter_order_by)]

use std::io::stdout;

use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{get, State};
use rocket_okapi::swagger_ui::{make_swagger_ui, SwaggerUIConfig};
use rocket_okapi::{openapi, openapi_get_routes};
use slog::{o, Drain};
use url::Url;

use crate::better_logging::{log_filter, print_msg_header, BetterLogging, ReqLogger};
use crate::crawler::{CrawlResult, Crawler, CrawlerStatus, ProdCrawler};
use crate::http_client::ProdHttpClient;
use slog_scope::GlobalLoggerGuard;

mod better_logging;
mod crawler;
mod http_client;
mod link_extractor;
#[cfg(test)]
mod test_util;

type CrawlerState = Box<dyn Crawler + Send + Sync>;

#[openapi]
#[get("/crawl/<seed>")]
/// Crawl a domain starting with <seed>
///
/// Returns information about all the links on every page reachable from seed
async fn crawl(
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
async fn status(
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

pub fn setup_logging() -> GlobalLoggerGuard {
    let plain = slog_term::PlainSyncDecorator::new(stdout());
    let logger = slog::Logger::root(
        slog_term::FullFormat::new(plain)
            .use_custom_header_print(print_msg_header)
            .build()
            .filter(log_filter)
            .fuse(),
        o!(),
    );
    let slog_guard = slog_scope::set_global_logger(logger);
    slog_stdlog::init().unwrap();
    slog_guard
}

pub async fn run_server() {
    rocket::build()
        .attach(BetterLogging {})
        .manage(CrawlerState::from(Box::new(ProdCrawler::new(Box::new(
            ProdHttpClient::new(),
        )))))
        .mount("/", openapi_get_routes![crawl, status])
        .mount(
            "/swagger",
            make_swagger_ui(&SwaggerUIConfig {
                url: "/openapi.json".to_string(),
                ..SwaggerUIConfig::default()
            }),
        )
        .launch()
        .await
        .unwrap();
}
