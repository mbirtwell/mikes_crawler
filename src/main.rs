#![feature(iter_order_by)]

use crate::crawler::{CrawlResult, Crawler, ProdCrawler};
use crate::http_client::ProdHttpClient;
use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{get, Build, Rocket, State};
use rocket_okapi::swagger_ui::{make_swagger_ui, SwaggerUIConfig};
use rocket_okapi::{openapi, openapi_get_routes};
use url::Url;

mod crawler;
mod http_client;
mod link_extractor;
#[cfg(test)]
mod test_util;

type CrawlerState = Box<dyn Crawler + Send + Sync>;

#[openapi]
#[get("/crawl/<seed>")]
async fn crawl(
    crawler: &State<CrawlerState>,
    seed: String,
) -> Result<Json<CrawlResult>, (Status, String)> {
    let seed = Url::parse(&seed).map_err(|e| (Status::BadRequest, e.to_string()))?;
    let result = crawler.crawl(seed).await;
    Ok(Json(result))
}

#[rocket::launch]
fn rocket() -> Rocket<Build> {
    rocket::build()
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
}
