#![feature(iter_order_by)]

use std::io::stdout;

use rocket_okapi::openapi_get_routes;
use rocket_okapi::swagger_ui::{make_swagger_ui, SwaggerUIConfig};
use slog::{o, Drain};
use slog_scope::GlobalLoggerGuard;

use crate::api::*;
use crate::better_logging::{log_filter, print_msg_header, BetterLogging};
use crate::crawler::{CrawlerState, ProdCrawler};
use crate::http_client::ProdHttpClient;

mod api;
mod better_logging;
mod crawler;
mod http_client;
mod link_extractor;
mod serializers;
#[cfg(test)]
mod test_util;

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
        .mount("/", openapi_get_routes![crawl, list, status])
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
