use crate::core::bucket::Data;
use axum::body::Body;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

pub struct FileResponse {
    pub key: String,
    pub data: Data,
    pub attachment: bool,
}

impl IntoResponse for FileResponse {
    fn into_response(self) -> Response {
        let mut response = Response::new(Body::from_stream(self.data.stream));
        let filename = self.key.rsplit('/').next().unwrap();
        let disposition = if self.attachment { "attachment" } else { "inline" };
        response.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            format!(
                "{disposition}; filename=\"download\"; filename*=UTF-8''{}",
                utf8_percent_encode(filename, NON_ALPHANUMERIC)
            )
            .parse()
            .unwrap(),
        );
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, self.data.size.to_string().parse().unwrap());
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, preview_type(filename).parse().unwrap());
        response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
        response
            .headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'".parse().unwrap());
        response
    }
}

fn preview_type(filename: &str) -> &'static str {
    match filename.rsplit('.').next().unwrap_or_default().to_ascii_lowercase().as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "txt" | "log" | "csv" | "json" | "md" | "html" | "svg" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
