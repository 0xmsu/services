use {
    crate::{app_data, database::Postgres, orderbook::Orderbook, quoter::QuoteHandler},
    anyhow::Result,
    observe::distributed_tracing::tracing_warp::make_span,
    serde::{Serialize, de::DeserializeOwned},
    shared::price_estimation::{PriceEstimationError, native::NativePriceEstimating},
    std::{
        convert::Infallible,
        fmt::Debug,
        sync::Arc,
        time::{Duration, Instant},
    },
    warp::{
        Filter,
        Rejection,
        Reply,
        filters::BoxedFilter,
        hyper::StatusCode,
        reply::{Json, WithStatus, json, with_status},
    },
};

mod cancel_order;
mod cancel_orders;
mod get_app_data;
mod get_auction;
mod get_native_price;
mod get_order_by_uid;
mod get_order_status;
mod get_orders_by_tx;
mod get_solver_competition;
mod get_solver_competition_v2;
mod get_token_metadata;
mod get_total_surplus;
mod get_trades;
mod get_user_orders;
mod post_order;
mod post_quote;
mod put_app_data;
mod version;

pub fn handle_all_routes(
    database: Postgres,
    orderbook: Arc<Orderbook>,
    quotes: Arc<QuoteHandler>,
    app_data: Arc<app_data::Registry>,
    native_price_estimator: Arc<dyn NativePriceEstimating>,
    quote_timeout: Duration,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    // Note that we add a string with endpoint's name to all responses.
    // This string will be used later to report metrics.
    // It is not used to form the actual server response.

    let routes = vec![
        (
            "v1/create_order",
            box_filter(post_order::post_order(orderbook.clone())),
        ),
        (
            "v1/get_order",
            box_filter(get_order_by_uid::get_order_by_uid(orderbook.clone())),
        ),
        (
            "v1/get_order_status",
            box_filter(get_order_status::get_status(orderbook.clone())),
        ),
        (
            "v1/get_trades",
            box_filter(get_trades::get_trades(database.clone())),
        ),
        (
            "v1/cancel_order",
            box_filter(cancel_order::cancel_order(orderbook.clone())),
        ),
        (
            "v1/cancel_orders",
            box_filter(cancel_orders::filter(orderbook.clone())),
        ),
        (
            "v1/get_user_orders",
            box_filter(get_user_orders::get_user_orders(orderbook.clone())),
        ),
        (
            "v1/get_orders_by_tx",
            box_filter(get_orders_by_tx::get_orders_by_tx(orderbook.clone())),
        ),
        ("v1/post_quote", box_filter(post_quote::post_quote(quotes))),
        (
            "v1/auction",
            box_filter(get_auction::get_auction(orderbook.clone())),
        ),
        (
            "v1/solver_competition",
            box_filter(get_solver_competition::get(Arc::new(database.clone()))),
        ),
        (
            "v2/solver_competition",
            box_filter(get_solver_competition_v2::get(database.clone())),
        ),
        (
            "v1/solver_competition/latest",
            box_filter(get_solver_competition::get_latest(Arc::new(
                database.clone(),
            ))),
        ),
        (
            "v2/solver_competition/latest",
            box_filter(get_solver_competition_v2::get_latest(database.clone())),
        ),
        ("v1/version", box_filter(version::version())),
        (
            "v1/get_native_price",
            box_filter(get_native_price::get_native_price(
                native_price_estimator,
                quote_timeout,
            )),
        ),
        (
            "v1/get_app_data",
            get_app_data::get(database.clone()).boxed(),
        ),
        (
            "v1/put_app_data",
            box_filter(put_app_data::filter(app_data)),
        ),
        (
            "v1/get_total_surplus",
            box_filter(get_total_surplus::get(database.clone())),
        ),
        (
            "v1/get_token_metadata",
            box_filter(get_token_metadata::get_token_metadata(database)),
        ),
    ];

    finalize_router(routes, "orderbook::api::request_summary")
}

pub type ApiReply = WithStatus<Json>;

// We turn Rejection into Reply to workaround warp not setting CORS headers on
// rejections.
async fn handle_rejection(err: Rejection) -> Result<impl Reply, Infallible> {
    let response = err.default_response();

    let metrics = ApiMetrics::instance(observe::metrics::get_storage_registry()).unwrap();
    metrics
        .requests_rejected
        .with_label_values(&[response.status().as_str()])
        .inc();

    Ok(response)
}

#[derive(prometheus_metric_storage::MetricStorage, Clone, Debug)]
#[metric(subsystem = "api")]
struct ApiMetrics {
    /// Number of completed API requests.
    #[metric(labels("method", "status_code"))]
    requests_complete: prometheus::IntCounterVec,

    /// Number of rejected API requests.
    #[metric(labels("status_code"))]
    requests_rejected: prometheus::IntCounterVec,

    /// Execution time for each API request.
    #[metric(labels("method"), buckets(0.1, 0.5, 1, 2, 4, 6, 8, 10))]
    requests_duration_seconds: prometheus::HistogramVec,
}

impl ApiMetrics {
    // Status codes we care about in our application. Populated with:
    // `rg -oIN 'StatusCode::[A-Z_]+' | sort | uniq`.
    const INITIAL_STATUSES: &'static [StatusCode] = &[
        StatusCode::OK,
        StatusCode::CREATED,
        StatusCode::BAD_REQUEST,
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
        StatusCode::NOT_FOUND,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::SERVICE_UNAVAILABLE,
    ];

    fn reset_requests_rejected(&self) {
        for status in Self::INITIAL_STATUSES {
            self.requests_rejected
                .with_label_values(&[status.as_str()])
                .reset();
        }
    }

    fn reset_requests_complete(&self, method: &str) {
        for status in Self::INITIAL_STATUSES {
            self.requests_complete
                .with_label_values(&[method, status.as_str()])
                .reset();
        }
    }

    fn on_request_completed(&self, method: &str, status: StatusCode, timer: Instant) {
        self.requests_complete
            .with_label_values(&[method, status.as_str()])
            .inc();
        self.requests_duration_seconds
            .with_label_values(&[method])
            .observe(timer.elapsed().as_secs_f64());
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Error<'a> {
    error_type: &'a str,
    description: &'a str,
    /// Additional arbitrary data that can be attached to an API error.
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

pub fn error(error_type: &str, description: impl AsRef<str>) -> Json {
    json(&Error {
        error_type,
        description: description.as_ref(),
        data: None,
    })
}

pub fn rich_error(error_type: &str, description: impl AsRef<str>, data: impl Serialize) -> Json {
    let data = match serde_json::to_value(&data) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::warn!(?err, "failed to serialize error data");
            None
        }
    };

    json(&Error {
        error_type,
        description: description.as_ref(),
        data,
    })
}

pub fn internal_error_reply() -> ApiReply {
    with_status(
        error("InternalServerError", ""),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
}

pub fn convert_json_response<T, E>(result: Result<T, E>) -> WithStatus<Json>
where
    T: Serialize,
    E: IntoWarpReply + Debug,
{
    match result {
        Ok(response) => with_status(warp::reply::json(&response), StatusCode::OK),
        Err(err) => err.into_warp_reply(),
    }
}

pub trait IntoWarpReply {
    fn into_warp_reply(self) -> ApiReply;
}

pub async fn response_body(response: warp::hyper::Response<warp::hyper::Body>) -> Vec<u8> {
    let mut body = response.into_body();
    let mut result = Vec::new();
    while let Some(bytes) = futures::StreamExt::next(&mut body).await {
        result.extend_from_slice(bytes.unwrap().as_ref());
    }
    result
}

const MAX_JSON_BODY_PAYLOAD: u64 = 1024 * 16;

pub fn extract_payload<T: DeserializeOwned + Send>()
-> impl Filter<Extract = (T,), Error = Rejection> + Clone {
    // (rejecting huge payloads)...
    extract_payload_with_max_size(MAX_JSON_BODY_PAYLOAD)
}

pub fn extract_payload_with_max_size<T: DeserializeOwned + Send>(
    max_size: u64,
) -> impl Filter<Extract = (T,), Error = Rejection> + Clone {
    warp::body::content_length_limit(max_size).and(warp::body::json())
}

pub type BoxedRoute = BoxedFilter<(Box<dyn Reply>,)>;

pub fn box_filter<Filter_, Reply_>(filter: Filter_) -> BoxedFilter<(Box<dyn Reply>,)>
where
    Filter_: Filter<Extract = (Reply_,), Error = Rejection> + Send + Sync + 'static,
    Reply_: Reply + Send + 'static,
{
    filter.map(|a| Box::new(a) as Box<dyn Reply>).boxed()
}

/// Sets up basic metrics, cors and proper log tracing for all routes.
///
/// # Panics
///
/// This method panics if `routes` is empty.
pub fn finalize_router(
    routes: Vec<(&'static str, BoxedRoute)>,
    log_prefix: &'static str,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    let metrics = ApiMetrics::instance(observe::metrics::get_storage_registry()).unwrap();
    metrics.reset_requests_rejected();
    for (method, _) in &routes {
        metrics.reset_requests_complete(method);
    }

    let router = routes
        .into_iter()
        .fold(
            Option::<BoxedFilter<(&'static str, Box<dyn Reply>)>>::None,
            |router, (method, route)| {
                let route = route.map(move |result| (method, result)).untuple_one();
                let next = match router {
                    Some(router) => router.or(route).unify().boxed(),
                    None => route.boxed(),
                };
                Some(next)
            },
        )
        .expect("routes cannot be empty");

    let instrumented =
        warp::any()
            .map(Instant::now)
            .and(router)
            .map(|timer, method, reply: Box<dyn Reply>| {
                let response = reply.into_response();
                metrics.on_request_completed(method, response.status(), timer);
                response
            });

    // Final setup
    let cors = warp::cors()
        .allow_any_origin()
        .allow_methods(vec!["GET", "POST", "DELETE", "OPTIONS", "PUT", "PATCH"])
        .allow_headers(vec!["Origin", "Content-Type", "X-Auth-Token", "X-AppId"]);

    warp::path!("api" / ..)
        .and(instrumented)
        .recover(handle_rejection)
        .with(cors)
        .with(warp::log::log(log_prefix))
        .with(warp::trace::trace(make_span))
}

impl IntoWarpReply for PriceEstimationError {
    fn into_warp_reply(self) -> WithStatus<Json> {
        match self {
            Self::UnsupportedToken { token, reason } => with_status(
                error(
                    "UnsupportedToken",
                    format!("Token {token:?} is unsupported: {reason:}"),
                ),
                StatusCode::BAD_REQUEST,
            ),
            Self::UnsupportedOrderType(order_type) => with_status(
                error(
                    "UnsupportedOrderType",
                    format!("{order_type} not supported"),
                ),
                StatusCode::BAD_REQUEST,
            ),
            Self::NoLiquidity | Self::RateLimited | Self::EstimatorInternal(_) => with_status(
                error("NoLiquidity", "no route found"),
                StatusCode::NOT_FOUND,
            ),
            Self::ProtocolInternal(err) => {
                tracing::error!(?err, "PriceEstimationError::Other");
                internal_error_reply()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, serde::ser, serde_json::json};

    #[test]
    fn rich_errors_skip_unset_data_field() {
        assert_eq!(
            serde_json::to_value(&Error {
                error_type: "foo",
                description: "bar",
                data: None,
            })
            .unwrap(),
            json!({
                "errorType": "foo",
                "description": "bar",
            }),
        );
        assert_eq!(
            serde_json::to_value(Error {
                error_type: "foo",
                description: "bar",
                data: Some(json!(42)),
            })
            .unwrap(),
            json!({
                "errorType": "foo",
                "description": "bar",
                "data": 42,
            }),
        );
    }

    #[tokio::test]
    async fn rich_errors_handle_serialization_errors() {
        struct AlwaysErrors;
        impl Serialize for AlwaysErrors {
            fn serialize<S>(&self, _: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                Err(ser::Error::custom("error"))
            }
        }

        let body = warp::hyper::body::to_bytes(
            rich_error("foo", "bar", AlwaysErrors)
                .into_response()
                .into_body(),
        )
        .await
        .unwrap();

        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            json!({
                "errorType": "foo",
                "description": "bar",
            })
        );
    }
}
