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
mod tests {}
