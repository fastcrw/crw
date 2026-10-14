use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use crw_core::error::CrwError;
use crw_core::types::ApiResponse;

/// The `errorCode` an error envelope carries, attached to the response's
/// extensions (never sent on the wire) so the request log can name the reason
/// for a failure without parsing the body.
#[derive(Debug, Clone, Copy)]
pub struct ErrorCode(pub &'static str);

/// Wrapper to implement IntoResponse for CrwError in the server crate.
pub struct AppError(pub CrwError);

impl From<CrwError> for AppError {
    fn from(e: CrwError) -> Self {
        Self(e)
    }
}

impl From<JsonRejection> for AppError {
    fn from(rejection: JsonRejection) -> Self {
        let msg = match &rejection {
            JsonRejection::JsonDataError(_) => {
                let raw = rejection.body_text();
                // Strip internal Rust type paths, keep the user-readable part.
                if let Some(pos) = raw.find(": ") {
                    format!("Invalid request body: {}", &raw[pos + 2..])
                } else {
                    format!("Invalid request body: {raw}")
                }
            }
            JsonRejection::JsonSyntaxError(_) => "Invalid JSON syntax in request body".to_string(),
            JsonRejection::MissingJsonContentType(_) => {
                "Missing Content-Type: application/json header".to_string()
            }
            _ => "Invalid request body".to_string(),
        };
        Self(CrwError::InvalidRequest(msg))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            CrwError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            CrwError::NotFound(_) => StatusCode::NOT_FOUND,
            CrwError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            CrwError::HttpError(_) => StatusCode::BAD_GATEWAY,
            CrwError::TargetUnreachable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            CrwError::ExtractionError(_) => StatusCode::UNPROCESSABLE_ENTITY,
            CrwError::UnsupportedContentType(_) => StatusCode::UNPROCESSABLE_ENTITY,
            CrwError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            CrwError::SearchDisabled(_) => StatusCode::SERVICE_UNAVAILABLE,
            CrwError::SearchDegraded(_) => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };

        let error_code = self.0.error_code();
        let body = ApiResponse::<()>::err_with_code(self.0.to_string(), error_code.to_string());
        let mut res = (status, Json(body)).into_response();
        res.extensions_mut().insert(ErrorCode(error_code));
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    fn status_for(err: CrwError) -> StatusCode {
        let app_err = AppError(err);
        let response = app_err.into_response();
        response.status()
    }

    #[test]
    fn app_error_invalid_request_400() {
        assert_eq!(
            status_for(CrwError::InvalidRequest("bad".into())),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn app_error_unsupported_content_type_422_not_500() {
        // Must be an explicit arm: the `_` fallback would report a .docx as a
        // 500, blaming the server for the origin's media type.
        assert_eq!(
            status_for(CrwError::UnsupportedContentType("application/zip".into())),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn app_error_not_found_404() {
        assert_eq!(
            status_for(CrwError::NotFound("missing".into())),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn app_error_timeout_504() {
        assert_eq!(
            status_for(CrwError::Timeout(5000)),
            StatusCode::GATEWAY_TIMEOUT
        );
    }

    #[test]
    fn app_error_http_error_502() {
        assert_eq!(
            status_for(CrwError::HttpError("fail".into())),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn app_error_extraction_422() {
        assert_eq!(
            status_for(CrwError::ExtractionError("parse fail".into())),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn app_error_internal_500() {
        assert_eq!(
            status_for(CrwError::Internal("oops".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn app_error_renderer_500() {
        assert_eq!(
            status_for(CrwError::RendererError("cdp fail".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn app_error_body_is_api_response() {
        let app_err = AppError(CrwError::InvalidRequest("test error".into()));
        let response = app_err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], false);
        assert!(json["error"].as_str().unwrap().contains("test error"));
    }
}
