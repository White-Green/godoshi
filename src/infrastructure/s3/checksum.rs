use super::*;
use axum::http::HeaderMap;
use futures::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, BufReader};
use futures::{TryStreamExt, stream};
use s3s::checksum::ChecksumHasher;
use s3s::dto::{Checksum, ChecksumAlgorithm, ChecksumType};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub(super) enum Algorithm {
    Crc32 = 1,
    Crc64nvme = 2,
}

impl Serialize for Algorithm {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for Algorithm {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match i32::deserialize(deserializer)? {
            1 => Ok(Self::Crc32),
            2 => Ok(Self::Crc64nvme),
            _ => Err(serde::de::Error::custom("invalid checksum algorithm")),
        }
    }
}

impl Algorithm {
    pub fn parse(name: &str) -> S3Result<Self> {
        match name {
            "CRC32" => Ok(Self::Crc32),
            "CRC64NVME" => Ok(Self::Crc64nvme),
            _ => Err(S3Error::new(S3ErrorCode::NotImplemented)),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Crc32 => "CRC32",
            Self::Crc64nvme => "CRC64NVME",
        }
    }

    pub fn header(self) -> String {
        format!("x-amz-checksum-{}", self.name().to_ascii_lowercase())
    }

    fn hasher(self) -> ChecksumHasher {
        let mut hasher = ChecksumHasher::default();
        match self {
            Self::Crc32 => hasher.crc32 = Some(Default::default()),
            Self::Crc64nvme => hasher.crc64nvme = Some(Default::default()),
        }
        hasher
    }

    fn finish(self, hasher: ChecksumHasher) -> String {
        let result = hasher.finalize();
        match self {
            Self::Crc32 => result.checksum_crc32.unwrap(),
            Self::Crc64nvme => result.checksum_crc64nvme.unwrap(),
        }
    }

    fn bytes(self) -> usize {
        match self {
            Self::Crc32 => 4,
            Self::Crc64nvme => 8,
        }
    }
}

const ALGORITHMS: &[Algorithm] = &[Algorithm::Crc32, Algorithm::Crc64nvme];

impl Algorithm {
    pub fn multipart_type(self) -> Kind {
        match self {
            Self::Crc32 => Kind::Composite,
            Self::Crc64nvme => Kind::FullObject,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Composite,
    FullObject,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Composite => ChecksumType::COMPOSITE,
            Self::FullObject => ChecksumType::FULL_OBJECT,
        }
    }
}

impl From<Kind> for ChecksumType {
    fn from(kind: Kind) -> Self {
        Self::from_static(kind.name())
    }
}

pub(super) fn header<'a>(headers: &'a HeaderMap, name: &str) -> S3Result<Option<&'a str>> {
    headers
        .get(name)
        .map(|v| v.to_str().map_err(|_| S3Error::new(S3ErrorCode::InvalidRequest)))
        .transpose()
}

pub(super) fn digest(value: &str, length: usize) -> S3Result<Vec<u8>> {
    let bytes = STANDARD.decode(value).map_err(|_| S3Error::new(S3ErrorCode::InvalidDigest))?;
    if bytes.len() != length {
        return Err(S3Error::new(S3ErrorCode::InvalidDigest));
    }
    Ok(bytes)
}

#[derive(Clone)]
pub(super) struct Sum {
    pub algorithm: Algorithm,
    pub value: String,
    pub kind: Kind,
}

impl Sum {
    pub fn into_checksum(self) -> Checksum {
        let mut checksum = Checksum {
            checksum_type: Some(self.kind.into()),
            ..Default::default()
        };
        match self.algorithm {
            Algorithm::Crc32 => checksum.checksum_crc32 = Some(self.value),
            Algorithm::Crc64nvme => checksum.checksum_crc64nvme = Some(self.value),
        }
        checksum
    }
}

pub(super) fn request_sum(crc32: Option<String>, crc64nvme: Option<String>) -> S3Result<Option<(Algorithm, String)>> {
    match (crc32, crc64nvme) {
        (Some(_), Some(_)) => Err(S3Error::new(S3ErrorCode::InvalidRequest)),
        (Some(value), None) => Ok(Some((Algorithm::Crc32, value))),
        (None, Some(value)) => Ok(Some((Algorithm::Crc64nvme, value))),
        (None, None) => Ok(None),
    }
}

pub(super) struct UploadChecksums {
    pub algorithm: Option<ChecksumAlgorithm>,
    pub crc32: Option<String>,
    pub crc64nvme: Option<String>,
    pub md5: Option<String>,
}

pub(super) struct Upload {
    status: Arc<Mutex<Option<Result<Sum, S3ErrorCode>>>>,
    pub length: u64,
}

impl Upload {
    pub fn finish(&self) -> S3Result<Sum> {
        self.status
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Err(S3ErrorCode::IncompleteBody))
            .map_err(S3Error::new)
    }
    pub fn error(&self, error: worker::Error) -> S3Error {
        match self.status.lock().unwrap().as_ref() {
            Some(Err(code)) => S3Error::new(code.clone()),
            _ => r2_error(error),
        }
    }
}

struct Payload {
    reader: Box<dyn AsyncBufRead + Unpin>,
    chunked: bool,
    chunk_left: u64,
    chunk_crlf: bool,
    ended: bool,
    trailer: Option<String>,
    trailers: HeaderMap,
}

impl Payload {
    async fn line(&mut self, limit: usize) -> S3Result<String> {
        let mut bytes = Vec::new();
        let count = (&mut self.reader)
            .take(limit as u64 + 1)
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|_| S3Error::new(S3ErrorCode::IncompleteBody))?;
        if count > limit || !bytes.ends_with(b"\r\n") {
            return Err(S3Error::new(S3ErrorCode::IncompleteBody));
        }
        bytes.truncate(bytes.len() - 2);
        String::from_utf8(bytes).map_err(|_| S3Error::new(S3ErrorCode::InvalidRequest))
    }
    async fn next(&mut self) -> S3Result<Option<Vec<u8>>> {
        if self.ended {
            return Ok(None);
        }
        if self.chunked && self.chunk_left == 0 {
            if self.chunk_crlf && !self.line(2).await?.is_empty() {
                return Err(S3Error::new(S3ErrorCode::IncompleteBody));
            }
            let line = self.line(1024).await?;
            self.chunk_left = u64::from_str_radix(&line, 16).map_err(|_| S3Error::new(S3ErrorCode::InvalidRequest))?;
            if self.chunk_left == 0 {
                let mut total = 0;
                loop {
                    let line = self.line(16 * 1024 - total).await?;
                    total += line.len() + 2;
                    if line.is_empty() {
                        break;
                    }
                    if total >= 16 * 1024 {
                        return Err(S3Error::new(S3ErrorCode::InvalidRequest));
                    }
                    let (name, value) = line.split_once(':').ok_or_else(|| S3Error::new(S3ErrorCode::InvalidRequest))?;
                    let name: axum::http::HeaderName = name.parse().map_err(|_| S3Error::new(S3ErrorCode::InvalidRequest))?;
                    if self.trailer.as_deref() != Some(name.as_str()) || self.trailers.contains_key(&name) {
                        return Err(S3Error::new(S3ErrorCode::InvalidRequest));
                    }
                    self.trailers
                        .insert(name, value.trim().parse().map_err(|_| S3Error::new(S3ErrorCode::InvalidDigest))?);
                }
                self.ended = true;
                let mut extra = [0];
                if self
                    .reader
                    .read(&mut extra)
                    .await
                    .map_err(|_| S3Error::new(S3ErrorCode::IncompleteBody))?
                    != 0
                {
                    return Err(S3Error::new(S3ErrorCode::IncompleteBody));
                }
                return Ok(None);
            }
            self.chunk_crlf = true;
        }
        let size = if self.chunked {
            self.chunk_left.min(64 * 1024) as usize
        } else {
            64 * 1024
        };
        let mut bytes = vec![0; size];
        let count = self
            .reader
            .read(&mut bytes)
            .await
            .map_err(|_| S3Error::new(S3ErrorCode::IncompleteBody))?;
        if count == 0 {
            if self.chunked {
                return Err(S3Error::new(S3ErrorCode::IncompleteBody));
            }
            self.ended = true;
            return Ok(None);
        }
        if self.chunked {
            self.chunk_left -= count as u64;
        }
        bytes.truncate(count);
        Ok(Some(bytes))
    }
}

struct Verify {
    payload: Payload,
    left: u64,
    algorithm: Algorithm,
    expected: Option<String>,
    md5: Option<String>,
    hasher: ChecksumHasher,
    md5_hasher: ChecksumHasher,
    status: Arc<Mutex<Option<Result<Sum, S3ErrorCode>>>>,
    done: bool,
}

impl Verify {
    async fn next(&mut self) -> S3Result<Option<Vec<u8>>> {
        if self.done {
            return Ok(None);
        }
        let bytes = self.payload.next().await?.unwrap_or_default();
        self.left = self
            .left
            .checked_sub(bytes.len() as u64)
            .ok_or_else(|| S3Error::new(S3ErrorCode::IncompleteBody))?;
        self.hasher.update(&bytes);
        self.md5_hasher.update(&bytes);
        if self.left != 0 {
            if bytes.is_empty() {
                return Err(S3Error::new(S3ErrorCode::IncompleteBody));
            }
            return Ok(Some(bytes));
        }
        if self.payload.next().await?.is_some() {
            return Err(S3Error::new(S3ErrorCode::IncompleteBody));
        }
        let value = self.algorithm.finish(std::mem::take(&mut self.hasher));
        if let Some(trailer) = &self.payload.trailer {
            self.expected = Some(
                header(&self.payload.trailers, trailer)?
                    .ok_or_else(|| S3Error::new(S3ErrorCode::InvalidDigest))?
                    .to_owned(),
            );
        }
        if let Some(expected) = &self.expected {
            digest(expected, self.algorithm.bytes())?;
            if *expected != value {
                return Err(S3Error::new(S3ErrorCode::BadDigest));
            }
        }
        if let Some(expected) = &self.md5
            && std::mem::take(&mut self.md5_hasher).finalize().checksum_md5.as_ref() != Some(expected)
        {
            return Err(S3Error::new(S3ErrorCode::BadDigest));
        }
        *self.status.lock().unwrap() = Some(Ok(Sum {
            algorithm: self.algorithm,
            value,
            kind: Kind::FullObject,
        }));
        self.done = true;
        Ok((!bytes.is_empty()).then_some(bytes))
    }
}

pub(super) async fn upload(
    body: Option<StreamingBlob>,
    length: Option<i64>,
    content_encoding: Option<&str>,
    headers: &HeaderMap,
    checksums: UploadChecksums,
    required: Option<Algorithm>,
) -> S3Result<(Data, Upload)> {
    let chunked = content_encoding.is_some_and(|s| s.split(',').any(|v| v.trim().eq_ignore_ascii_case("aws-chunked")));
    let trailer = header(headers, "x-amz-trailer")?.map(str::to_owned);
    if chunked && (header(headers, "x-amz-content-sha256")? != Some("STREAMING-UNSIGNED-PAYLOAD-TRAILER") || trailer.is_none()) {
        return Err(S3Error::new(S3ErrorCode::NotImplemented));
    }
    if !chunked && trailer.is_some() {
        return Err(S3Error::new(S3ErrorCode::InvalidRequest));
    }
    let length = if chunked {
        header(headers, "x-amz-decoded-content-length")?.and_then(|s| s.parse::<u64>().ok())
    } else {
        length.and_then(|n| u64::try_from(n).ok())
    }
    .ok_or_else(|| S3Error::new(S3ErrorCode::MissingContentLength))?;
    if length > 5 * 1024 * 1024 * 1024 {
        return Err(S3Error::new(S3ErrorCode::EntityTooLarge));
    }
    let expected = request_sum(checksums.crc32, checksums.crc64nvme)?;
    let declared = checksums.algorithm.as_ref().map(|value| Algorithm::parse(value.as_str())).transpose()?;
    let from_trailer = trailer
        .as_ref()
        .map(|name| {
            ALGORITHMS
                .iter()
                .copied()
                .find(|a| a.header() == *name)
                .ok_or_else(|| S3Error::new(S3ErrorCode::InvalidRequest))
        })
        .transpose()?;
    let algorithm = expected
        .as_ref()
        .map(|(a, _)| *a)
        .or(from_trailer)
        .or(declared)
        .or(required)
        .unwrap_or(Algorithm::Crc64nvme);
    if [declared, required, from_trailer].into_iter().flatten().any(|a| a != algorithm) || (expected.is_some() && trailer.is_some()) {
        return Err(S3Error::new(S3ErrorCode::InvalidRequest));
    }
    if let Some((a, value)) = &expected {
        digest(value, a.bytes())?;
    }
    let md5 = checksums.md5;
    if let Some(value) = &md5 {
        digest(value, 16)?;
    }
    let has_md5 = md5.is_some();
    let body = body.unwrap_or_else(|| StreamingBlob::from_bytes(Default::default()));
    let reader = BufReader::new(body.map_err(std::io::Error::other).into_async_read());
    let status = Arc::new(Mutex::new(None));
    let mut verify = Verify {
        payload: Payload {
            reader: Box::new(reader),
            chunked,
            chunk_left: 0,
            chunk_crlf: false,
            ended: false,
            trailer,
            trailers: HeaderMap::new(),
        },
        left: length,
        algorithm,
        expected: expected.map(|(_, s)| s),
        md5,
        hasher: algorithm.hasher(),
        md5_hasher: ChecksumHasher {
            md5: has_md5.then(Default::default),
            ..Default::default()
        },
        status: status.clone(),
        done: false,
    };
    let result = Upload { status, length };
    if length == 0 {
        verify.next().await?;
        return Ok((Data::Empty, result));
    }
    let stream = stream::try_unfold(verify, |mut verify| async move {
        match verify.next().await {
            Ok(Some(bytes)) => Ok(Some((bytes, verify))),
            Ok(None) => Ok(None),
            Err(error) => {
                *verify.status.lock().unwrap() = Some(Err(error.code().clone()));
                Err(worker::Error::RustError("S3 upload validation failed".into()))
            }
        }
    });
    Ok((worker::FixedLengthStream::wrap(stream, length).into(), result))
}

pub(super) fn encoding(value: Option<String>) -> Option<String> {
    value
        .map(|s| {
            s.split(',')
                .filter(|v| !v.trim().eq_ignore_ascii_case("aws-chunked"))
                .collect::<Vec<_>>()
                .join(",")
        })
        .filter(|s| !s.is_empty())
}

pub(super) fn r2_checksum(object: &worker::Object) -> Checksum {
    let sums = object.checksum();
    let mut checksum = Checksum::default();
    if let Some(bytes) = sums.sha256 {
        checksum.checksum_sha256 = Some(STANDARD.encode(bytes));
    } else if let Some(bytes) = sums.sha1 {
        checksum.checksum_sha1 = Some(STANDARD.encode(bytes));
    } else {
        return checksum;
    }
    checksum.checksum_type = Some(ChecksumType::from_static(ChecksumType::FULL_OBJECT));
    checksum
}

#[derive(Deserialize)]
pub(super) struct Multipart {
    pub algorithm: Option<Algorithm>,
}

impl Multipart {
    pub fn new(algorithm: Option<ChecksumAlgorithm>, checksum_type: Option<ChecksumType>) -> S3Result<Self> {
        let upload = Self {
            algorithm: algorithm.as_ref().map(|value| Algorithm::parse(value.as_str())).transpose()?,
        };
        let kind = upload.algorithm().multipart_type();
        if checksum_type.is_some_and(|value| value.as_str() != kind.name()) {
            return Err(S3Error::new(S3ErrorCode::NotImplemented));
        }
        Ok(upload)
    }
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm.unwrap_or(Algorithm::Crc64nvme)
    }
}

impl Algorithm {
    pub fn combine(self, parts: &[Part]) -> S3Result<Sum> {
        let kind = self.multipart_type();
        let value = match kind {
            Kind::Composite => {
                let mut hasher = self.hasher();
                for part in parts {
                    hasher.update(&digest(&part.sum.value, self.bytes())?);
                }
                format!("{}-{}", self.finish(hasher), parts.len())
            }
            Kind::FullObject => {
                let mut crc = 0;
                for part in parts {
                    let bytes = digest(&part.sum.value, 8)?;
                    let next = u64::from_be_bytes(bytes.try_into().map_err(|_| S3Error::new(S3ErrorCode::InvalidDigest))?);
                    crc = crc_fast::checksum_combine(crc_fast::CrcAlgorithm::Crc64Nvme, crc, next, part.length);
                }
                STANDARD.encode(crc.to_be_bytes())
            }
        };
        Ok(Sum {
            algorithm: self,
            value,
            kind,
        })
    }
}

pub(super) struct Part {
    pub length: u64,
    pub sum: Sum,
}
