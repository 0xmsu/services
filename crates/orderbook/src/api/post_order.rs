use {
    crate::{
        api::{ApiReply, IntoWarpReply, error, extract_payload},
        orderbook::{AddOrderError, OrderReplacementError, Orderbook},
    },
    anyhow::Result,
    model::{
        order::{AppdataFromMismatch, OrderCreation, OrderUid},
        quote::QuoteId,
        signature,
    },
    shared::order_validation::{
        AppDataValidationError,
        OrderValidToError,
        PartialValidationError,
        ValidationError,
    },
    std::{convert::Infallible, sync::Arc},
    warp::{
        Filter,
        Rejection,
        hyper::StatusCode,
        reply::{self, with_status},
    },
};

pub fn create_order_request() -> impl Filter<Extract = (OrderCreation,), Error = Rejection> + Clone
{
    warp::path!("v1" / "orders")
        .and(warp::post())
        .and(extract_payload())
}

pub struct PartialValidationErrorWrapper(pub PartialValidationError);
impl IntoWarpReply for PartialValidationErrorWrapper {
    fn into_warp_reply(self) -> ApiReply {
        match self.0 {
            PartialValidationError::UnsupportedBuyTokenDestination(dest) => with_status(
                error("UnsupportedBuyTokenDestination", format!("Type {dest:?}")),
                StatusCode::BAD_REQUEST,
            ),
            PartialValidationError::UnsupportedSellTokenSource(src) => with_status(
                error("UnsupportedSellTokenSource", format!("Type {src:?}")),
                StatusCode::BAD_REQUEST,
            ),
            PartialValidationError::UnsupportedOrderType => with_status(
                error(
                    "UnsupportedOrderType",
                    "This order type is currently not supported",
                ),
                StatusCode::BAD_REQUEST,
            ),
            PartialValidationError::Forbidden => with_status(
                error("Forbidden", "Forbidden, your account is deny-listed"),
                StatusCode::FORBIDDEN,
            ),
            PartialValidationError::ValidTo(OrderValidToError::Insufficient) => with_status(
                error(
                    "InsufficientValidTo",
                    "validTo is not far enough in the future",
                ),
                StatusCode::BAD_REQUEST,
            ),
            PartialValidationError::ValidTo(OrderValidToError::Excessive) => with_status(
                error("ExcessiveValidTo", "validTo is too far into the future"),
                StatusCode::BAD_REQUEST,
            ),
            PartialValidationError::InvalidNativeSellToken => with_status(
                error(
                    "InvalidNativeSellToken",
                    "The chain's native token (Ether/xDai) cannot be used as the sell token",
                ),
                StatusCode::BAD_REQUEST,
            ),
            PartialValidationError::SameBuyAndSellToken => with_status(
                error(
                    "SameBuyAndSellToken",
                    "Buy token is the same as the sell token.",
                ),
                StatusCode::BAD_REQUEST,
            ),
            PartialValidationError::UnsupportedToken { token, reason } => with_status(
                error(
                    "UnsupportedToken",
                    format!("Token {token:?} is unsupported: {reason}"),
                ),
                StatusCode::BAD_REQUEST,
            ),
            PartialValidationError::Other(err) => {
                tracing::error!(?err, "PartialValidatonError");
                crate::api::internal_error_reply()
            }
        }
    }
}

pub struct AppDataValidationErrorWrapper(pub AppDataValidationError);
impl IntoWarpReply for AppDataValidationErrorWrapper {
    fn into_warp_reply(self) -> ApiReply {
        match self.0 {
            AppDataValidationError::Invalid(err) => with_status(
                error("InvalidAppData", format!("{err:?}")),
                StatusCode::BAD_REQUEST,
            ),
            AppDataValidationError::Mismatch { provided, actual } => with_status(
                error(
                    "AppDataHashMismatch",
                    format!(
                        "calculated app data hash {actual:?} doesn't match order app data field \
                         {provided:?}",
                    ),
                ),
                StatusCode::BAD_REQUEST,
            ),
        }
    }
}

pub struct ValidationErrorWrapper(ValidationError);
impl IntoWarpReply for ValidationErrorWrapper {
    fn into_warp_reply(self) -> ApiReply {
        match self.0 {
            ValidationError::Partial(pre) => PartialValidationErrorWrapper(pre).into_warp_reply(),
            ValidationError::AppData(err) => AppDataValidationErrorWrapper(err).into_warp_reply(),
            ValidationError::PriceForQuote(err) => err.into_warp_reply(),
            ValidationError::MissingFrom => with_status(
                error(
                    "MissingFrom",
                    "From address must be specified for on-chain signature",
                ),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::AppdataFromMismatch(AppdataFromMismatch {
                from,
                app_data_signer,
            }) => with_status(
                error(
                    "AppdataFromMismatch",
                    format!(
                        "from address {from:?} cannot be different from metadata.signer \
                         {app_data_signer:?} specified in the app data"
                    ),
                ),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::WrongOwner(signature::Recovered { message, signer }) => with_status(
                error(
                    "WrongOwner",
                    format!(
                        "recovered signer {signer:?} from signing hash {message:?} does not match \
                         from address"
                    ),
                ),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::InvalidEip1271Signature(hash) => with_status(
                error(
                    "InvalidEip1271Signature",
                    format!("signature for computed order hash {hash:?} is not valid"),
                ),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::InsufficientBalance => with_status(
                error(
                    "InsufficientBalance",
                    "order owner must have funds worth at least x in his account",
                ),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::InsufficientAllowance => with_status(
                error(
                    "InsufficientAllowance",
                    "order owner must give allowance to VaultRelayer",
                ),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::InvalidSignature => with_status(
                error("InvalidSignature", "invalid signature"),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::NonZeroFee => with_status(
                error("NonZeroFee", "Fee must be zero"),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::SellAmountOverflow => with_status(
                error(
                    "SellAmountOverflow",
                    "Sell amount + fee amount must fit in U256",
                ),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            ValidationError::TransferSimulationFailed => with_status(
                error(
                    "TransferSimulationFailed",
                    "sell token cannot be transferred",
                ),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::QuoteNotVerified => with_status(
                error(
                    "QuoteNotVerified",
                    "No quote for this trade could be verified to be accurate. Aborting the order \
                     creation since it will likely not be executed.",
                ),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::ZeroAmount => with_status(
                error("ZeroAmount", "Buy or sell amount is zero."),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::IncompatibleSigningScheme => with_status(
                error(
                    "IncompatibleSigningScheme",
                    "Signing scheme is not compatible with order placement method.",
                ),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::TooManyLimitOrders => with_status(
                error("TooManyLimitOrders", "Too many limit orders"),
                StatusCode::BAD_REQUEST,
            ),
            ValidationError::TooMuchGas => with_status(
                error("TooMuchGas", "Executing order requires too many gas units"),
                StatusCode::BAD_REQUEST,
            ),

            ValidationError::Other(err) => {
                tracing::error!(?err, "ValidationErrorWrapper");
                crate::api::internal_error_reply()
            }
        }
    }
}

impl IntoWarpReply for AddOrderError {
    fn into_warp_reply(self) -> ApiReply {
        match self {
            Self::OrderValidation(err) => ValidationErrorWrapper(err).into_warp_reply(),
            Self::DuplicatedOrder => with_status(
                error("DuplicatedOrder", "order already exists"),
                StatusCode::BAD_REQUEST,
            ),
            Self::Database(err) => {
                tracing::error!(?err, "AddOrderError");
                crate::api::internal_error_reply()
            }
            err @ AddOrderError::AppDataMismatch { .. } => {
                tracing::error!(
                    ?err,
                    "An order with full app data passed validation but then failed to be inserted \
                     because we already stored different full app data for the same contract app \
                     data. This should be impossible."
                );
                crate::api::internal_error_reply()
            }
            AddOrderError::OrderNotFound(err) => err.into_warp_reply(),
            AddOrderError::InvalidAppData(err) => reply::with_status(
                super::error("InvalidAppData", err.to_string()),
                StatusCode::BAD_REQUEST,
            ),
            AddOrderError::InvalidReplacement(err) => err.into_warp_reply(),
            AddOrderError::MetadataSerializationFailed(err) => reply::with_status(
                super::error("MetadataSerializationFailed", err.to_string()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        }
    }
}

impl IntoWarpReply for OrderReplacementError {
    fn into_warp_reply(self) -> super::ApiReply {
        match self {
            OrderReplacementError::InvalidSignature => with_status(
                super::error("InvalidSignature", "Malformed signature"),
                StatusCode::BAD_REQUEST,
            ),
            OrderReplacementError::WrongOwner => with_status(
                super::error("WrongOwner", "Old and new orders have different signers"),
                StatusCode::UNAUTHORIZED,
            ),
            OrderReplacementError::OldOrderActivelyBidOn => with_status(
                super::error(
                    "OldOrderActivelyBidOn",
                    "The old order is actively beign bid on in recent auctions",
                ),
                StatusCode::BAD_REQUEST,
            ),
            OrderReplacementError::Other(err) => {
                tracing::error!(?err, "replace_order");
                crate::api::internal_error_reply()
            }
        }
    }
}

pub fn create_order_response(
    result: Result<(OrderUid, Option<QuoteId>), AddOrderError>,
) -> ApiReply {
    match result {
        Ok((uid, _)) => with_status(warp::reply::json(&uid), StatusCode::CREATED),
        Err(err) => err.into_warp_reply(),
    }
}

pub fn post_order(
    orderbook: Arc<Orderbook>,
) -> impl Filter<Extract = (ApiReply,), Error = Rejection> + Clone {
    create_order_request().and_then(move |order: OrderCreation| {
        let orderbook = orderbook.clone();
        async move {
            let result = orderbook
                .add_order(order.clone())
                .await
                .map(|(order_uid, quote_metadata)| {
                    let quote_id = quote_metadata.as_ref().and_then(|q| q.id);
                    let quote_solver = quote_metadata.as_ref().map(|q| q.solver);
                    tracing::debug!(%order_uid, ?quote_id, ?quote_solver, "order created");
                    (order_uid, quote_metadata.and_then(|quote| quote.id))
                })
                .inspect_err(|err| {
                    tracing::debug!(?order, ?err, "error creating order");
                });

            Result::<_, Infallible>::Ok(create_order_response(result))
        }
    })
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::api::response_body,
        model::order::{OrderCreation, OrderUid},
        serde_json::json,
        warp::{Reply, test::request},
    };

    #[tokio::test]
    async fn create_order_request_ok() {
        let filter = create_order_request();
        let order_payload = OrderCreation::default();
        let request = request()
            .path("/v1/orders")
            .method("POST")
            .header("content-type", "application/json")
            .json(&order_payload);
        let result = request.filter(&filter).await.unwrap();
        assert_eq!(result, order_payload);
    }

    #[tokio::test]
    async fn create_order_response_created() {
        let uid = OrderUid([1u8; 56]);
        let response = create_order_response(Ok((uid, Some(42)))).into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response_body(response).await;
        let body: serde_json::Value = serde_json::from_slice(body.as_slice()).unwrap();
        let expected= json!(
            "0x0101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101"
        );
        assert_eq!(body, expected);
    }

    #[tokio::test]
    async fn create_order_response_duplicate() {
        let response = create_order_response(Err(AddOrderError::DuplicatedOrder)).into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_body(response).await;
        let body: serde_json::Value = serde_json::from_slice(body.as_slice()).unwrap();
        let expected_error =
            json!({"errorType": "DuplicatedOrder", "description": "order already exists"});
        assert_eq!(body, expected_error);
    }
}
