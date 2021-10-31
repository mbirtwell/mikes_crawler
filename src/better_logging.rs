use std::future::Future;
use std::io::Write;
use std::time::SystemTime;

use anyhow::anyhow;
use rand::RngCore;
use rocket::fairing::{Fairing, Info, Kind};
use rocket::request::FromRequest;
use rocket::{async_trait, request, Data, Request, Response};
use rocket_okapi::gen::OpenApiGenerator;
use rocket_okapi::request::{OpenApiFromRequest, RequestHeaderInput};
use slog::{o, Level, Logger, Record};
use slog_scope_futures::SlogScope;
use slog_term::{CountingWriter, RecordDecorator, ThreadSafeTimestampFn};
use std::io;

/// Rocket's logging actually leaves quite a lot to be desired out of the box
/// in particular there's no way of associating a log line with a particular
/// request. Including the log line which says the request is completed.
/// This module allows us to improve on that by giving each request a random id
/// and allowing us to arrange for that id to be included with most log lines
/// that are logged during the processing of the request.
/// Rocket are aware of the issue and there seems to be a pretty genuine push
/// to move over to tracing as the logging library, which would address all of
/// my complaints and more. See:
/// * https://github.com/SergioBenitez/Rocket/issues/21
/// * https://github.com/SergioBenitez/Rocket/pull/1579
pub struct BetterLogging {}

struct TimerStart(Option<SystemTime>);

struct RequestId(String);

impl RequestId {
    pub fn rand() -> Self {
        let mut request_id = [0u8; 3];
        rand::thread_rng().fill_bytes(&mut request_id);
        Self(base64::encode(request_id))
    }
}

#[async_trait]
impl Fairing for BetterLogging {
    fn info(&self) -> Info {
        Info {
            name: "BetterLogging",
            kind: Kind::Request | Kind::Response,
        }
    }

    async fn on_request(&self, req: &mut Request<'_>, _data: &mut Data<'_>) {
        req.local_cache(|| TimerStart(Some(SystemTime::now())));
        let request_id = RequestId::rand();
        let logger = ReqLogger::new(&request_id);
        slog::info!(logger.0, "Received request: {}", req.uri());
        req.local_cache(|| request_id);
        req.local_cache(|| logger);
    }

    async fn on_response<'r>(&self, req: &'r Request<'_>, res: &mut Response<'r>) {
        let logger = req.local_cache(ReqLogger::backup);
        let request_id = req.local_cache(|| RequestId("missing".to_string()));
        res.set_raw_header("X-REQ-ID", request_id.0.clone());
        let time_str = req
            .local_cache(|| TimerStart(None))
            .0
            .ok_or_else(|| anyhow!("Start time not set"))
            .and_then(|t| Ok(t.elapsed()?))
            .map(|d| format!("{}.{:03}s", d.as_secs(), d.subsec_millis()))
            .unwrap_or_else(|e| e.to_string());
        slog::info!(
            logger.0,
            "Completed request {} with status {} in {}",
            req.uri(),
            res.status(),
            time_str,
        )
    }
}

#[derive(Clone)]
pub struct ReqLogger(Logger);

impl ReqLogger {
    fn new(req_id: &RequestId) -> Self {
        Self(slog_scope::logger().new(o!("REQ" => req_id.0.clone())))
    }

    fn backup() -> Self {
        Self(slog_scope::logger().new(o!("REQ" => "missing".to_string())))
    }

    pub fn scope<R, F>(&self, f: F) -> impl Future<Output = R>
    where
        F: Future<Output = R>,
    {
        SlogScope::new(self.0.clone(), f)
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for ReqLogger {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, ()> {
        request::Outcome::Success(request.local_cache(ReqLogger::backup).clone())
    }
}

impl<'r> OpenApiFromRequest<'r> for ReqLogger {
    fn from_request_input(
        _gen: &mut OpenApiGenerator,
        _name: String,
        _required: bool,
    ) -> rocket_okapi::Result<RequestHeaderInput> {
        Ok(RequestHeaderInput::None)
    }
}

pub fn log_filter(rd: &Record) -> bool {
    if rd.module().starts_with("html5ever::tokenizer") && rd.level() <= Level::Debug {
        return false;
    }
    if rd.module() == "mio::poll" && rd.level() >= Level::Debug {
        return false;
    }
    if rd.module() == "want" && rd.level() >= Level::Debug {
        return false;
    }

    true
}

/// Main benefit of this version of the header printer is that it shows the
/// module which is what we plan to filter on.
pub fn print_msg_header(
    fn_timestamp: &dyn ThreadSafeTimestampFn<Output = io::Result<()>>,
    mut rd: &mut dyn RecordDecorator,
    record: &Record,
    _use_file_location: bool,
) -> io::Result<bool> {
    rd.start_timestamp()?;
    fn_timestamp(&mut rd)?;

    rd.start_whitespace()?;
    write!(rd, " ")?;

    rd.start_level()?;
    write!(rd, "{}", record.level().as_short_str())?;

    rd.start_location()?;
    write!(rd, "[{}]", record.module(),)?;

    rd.start_whitespace()?;
    write!(rd, " ")?;

    rd.start_msg()?;
    let mut count_rd = CountingWriter::new(&mut rd);
    write!(count_rd, "{}", record.msg())?;
    Ok(count_rd.count() != 0)
}
