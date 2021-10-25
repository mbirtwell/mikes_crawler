#![feature(iter_order_by)]

use crate::link_extractor::PageInfo;
use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{get, Build, Rocket};
use rocket_okapi::swagger_ui::{make_swagger_ui, SwaggerUIConfig};
use rocket_okapi::{openapi, openapi_get_routes};
use url::Url;

mod link_extractor;

#[openapi]
#[get("/crawl/<seed>")]
async fn crawl(seed: String) -> Result<Json<Vec<PageInfo>>, (Status, String)> {
    let url = Url::parse(&seed).map_err(|e| (Status::BadRequest, e.to_string()))?;

    Ok(Json(vec![]))
}

#[rocket::launch]
fn rocket() -> Rocket<Build> {
    rocket::build()
        .mount("/", openapi_get_routes![crawl])
        .mount(
            "/swagger",
            make_swagger_ui(&SwaggerUIConfig {
                url: "/openapi.json".to_string(),
                ..SwaggerUIConfig::default()
            }),
        )
}
