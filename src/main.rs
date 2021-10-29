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
use crate::crawler::{CrawlResult, Crawler, ProdCrawler};
use crate::http_client::ProdHttpClient;

mod better_logging;
mod crawler;
mod http_client;
mod link_extractor;
#[cfg(test)]
mod test_util;

type CrawlerState = Box<dyn Crawler + Send + Sync>;

#[openapi]
#[get("/crawl/<seed>")]
async fn crawl(
    logger: ReqLogger,
    crawler: &State<CrawlerState>,
    seed: String,
) -> Result<Json<CrawlResult>, (Status, String)> {
    logger
        .scope(async move {
            let seed = Url::parse(&seed).map_err(|e| (Status::BadRequest, e.to_string()))?;
            let result = crawler.crawl(seed).await;
            Ok(Json(result))
        })
        .await
}

#[rocket::main]
async fn main() {
    let plain = slog_term::PlainSyncDecorator::new(stdout());
    let logger = slog::Logger::root(
        slog_term::FullFormat::new(plain)
            .use_custom_header_print(print_msg_header)
            .build()
            .filter(log_filter)
            .fuse(),
        o!(),
    );
    let _slog_guard = slog_scope::set_global_logger(logger);
    let _log_guard = slog_stdlog::init().unwrap();

    rocket::build()
        .attach(BetterLogging {})
        .manage(CrawlerState::from(Box::new(ProdCrawler::new(Box::new(
            ProdHttpClient::new(),
        )))))
        .mount("/", openapi_get_routes![crawl])
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
