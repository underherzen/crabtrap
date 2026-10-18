//! Реализация порта [`BodyDecoderService`] (Phase 5): gzip/deflate через
//! `flate2` (rust_backend — чистый Rust, без C/zlib-ng), brotli через
//! чисто-рельсовый крейт `brotli`.
//!
//! Декомпрессия выполняется СИНХРОННО внутри async-метода порта:
//! тела ограничены capture-cap (≤50 MiB), декомпрессия in-memory —
//! микросекунды на килобайт; `spawn_blocking` тут не окупается, а вызов
//! из gpui всё равно идёт через D4-мост (tokio-хендл, async-channel).
//!
//! Защита от zip-bomb: жёсткий лимит выходных байтов
//! ([`MAX_DECOMPRESSED_BYTES`], 64 MiB) — сверх лимита `InvalidEntity`,
//! а не OOM. «deflate» в HTTP двусмыслен (RFC 7230: zlib-обёртка; многие
//! серверы шлют raw): пробуем zlib, при ошибке — raw fallback.

use std::io::Read;

use application::services::BodyDecoderService;
use async_trait::async_trait;
use domain::errors::DomainError;

/// Потолок декомпрессированного тела (больше capture-cap с запасом на
/// «обычные» раскрытия, но достаточно мал, чтобы держать память под контролем).
pub const MAX_DECOMPRESSED_BYTES: usize = 64 * 1024 * 1024;

/// Декодер тел на flate2 + brotli (stateless, дёшево создавать).
pub struct RustBodyDecoder {
    limit: usize,
}

impl RustBodyDecoder {
    pub fn new() -> Self {
        Self {
            limit: MAX_DECOMPRESSED_BYTES,
        }
    }

    /// Тестовый конструктор с урезанным лимитом (guard-тест без 64 MiB).
    #[cfg(test)]
    fn with_limit(limit: usize) -> Self {
        Self { limit }
    }
}

impl Default for RustBodyDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Нормализованное имя одной кодировки: trim + lowercase, «x-gzip» →
/// «gzip». `None` — неизвестная кодировка.
fn normalize(encoding: &str) -> Option<&'static str> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "" | "identity" => Some("identity"),
        "gzip" | "x-gzip" => Some("gzip"),
        "br" => Some("br"),
        "deflate" => Some("deflate"),
        "zstd" => Some("zstd"),
        _ => None,
    }
}

/// `Content-Encoding: gzip, br` — кодировки в порядке применения; `None`,
/// если хоть одна неизвестна (цепочку целиком не снять).
fn parse_chain(encoding: &str) -> Option<Vec<&'static str>> {
    encoding.split(',').map(normalize).collect()
}

fn decode_err(codec: &str, err: std::io::Error) -> DomainError {
    DomainError::InvalidEntity(format!("{codec} decode failed: {err}"))
}

/// Читает декодер до конца с лимитом выхода (zip-bomb guard).
fn drain_with_limit<R: Read>(reader: R, limit: usize, codec: &str) -> Result<Vec<u8>, DomainError> {
    let mut reader = reader;
    let mut out: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut chunk).map_err(|e| decode_err(codec, e))?;
        if n == 0 {
            return Ok(out);
        }
        if out.len() + n > limit {
            return Err(DomainError::InvalidEntity(format!(
                "{codec} output exceeds {limit} bytes (possible decompression bomb)"
            )));
        }
        out.extend_from_slice(&chunk[..n]);
    }
}

fn decode_gzip(data: &[u8], limit: usize) -> Result<Vec<u8>, DomainError> {
    // MultiGzDecoder переваривает и multi-member gzip (конкатенация потоков).
    drain_with_limit(flate2::read::MultiGzDecoder::new(data), limit, "gzip")
}

fn decode_deflate(data: &[u8], limit: usize) -> Result<Vec<u8>, DomainError> {
    // zlib-обёртка — буква RFC; при ошибке — raw deflate (реальные серверы).
    match drain_with_limit(flate2::read::ZlibDecoder::new(data), limit, "deflate") {
        Ok(out) => Ok(out),
        Err(_) => drain_with_limit(flate2::read::DeflateDecoder::new(data), limit, "deflate"),
    }
}

fn decode_brotli(data: &[u8], limit: usize) -> Result<Vec<u8>, DomainError> {
    drain_with_limit(brotli::Decompressor::new(data, 64 * 1024), limit, "brotli")
}

fn decode_zstd(data: &[u8], limit: usize) -> Result<Vec<u8>, DomainError> {
    let mut source = data;
    let decoder = ruzstd::decoding::StreamingDecoder::new(&mut source)
        .map_err(|e| DomainError::InvalidEntity(format!("zstd decode failed: {e}")))?;
    drain_with_limit(decoder, limit, "zstd")
}

fn decode_one(codec: &str, data: &[u8], limit: usize) -> Result<Vec<u8>, DomainError> {
    match codec {
        "gzip" => decode_gzip(data, limit),
        "deflate" => decode_deflate(data, limit),
        "br" => decode_brotli(data, limit),
        "zstd" => decode_zstd(data, limit),
        _ => Ok(data.to_vec()),
    }
}

#[async_trait]
impl BodyDecoderService for RustBodyDecoder {
    /// Снимает цепочку кодировок с конца (последняя применённая — первой).
    /// Неизвестная кодировка в цепочке — pass-through всего тела (контракт
    /// порта; узнать заранее — [`Self::supported`]).
    async fn decode(&self, data: &[u8], encoding: &str) -> Result<Vec<u8>, DomainError> {
        let Some(chain) = parse_chain(encoding) else {
            return Ok(data.to_vec());
        };
        let mut current = data.to_vec();
        for codec in chain.into_iter().rev() {
            current = decode_one(codec, &current, self.limit)?;
        }
        Ok(current)
    }

    /// Цепочка целиком известна И требует распаковки (identity/пусто — нет).
    async fn supported(&self, encoding: &str) -> bool {
        parse_chain(encoding).is_some_and(|chain| chain.iter().any(|c| *c != "identity"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
    use std::io::Write;

    fn decode(dec: &RustBodyDecoder, data: &[u8], enc: &str) -> Result<Vec<u8>, DomainError> {
        futures::executor::block_on(dec.decode(data, enc))
    }

    fn supported(dec: &RustBodyDecoder, enc: &str) -> bool {
        futures::executor::block_on(dec.supported(enc))
    }

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn raw_deflate(data: &[u8]) -> Vec<u8> {
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn br(data: &[u8]) -> Vec<u8> {
        let mut enc = brotli::CompressorWriter::new(Vec::new(), 4096, 5, 22);
        enc.write_all(data).unwrap();
        enc.into_inner()
    }

    #[test]
    fn gzip_roundtrip() {
        let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let dec = RustBodyDecoder::new();
        let out = decode(&dec, &gz(&payload), "gzip").expect("gzip decodes");
        assert_eq!(out, payload);
    }

    #[test]
    fn deflate_zlib_and_raw_fallback() {
        let payload = b"deflate me: hello world".repeat(64);
        let dec = RustBodyDecoder::new();
        // zlib-обёрнутая форма (по RFC).
        assert_eq!(decode(&dec, &zlib(&payload), "deflate").unwrap(), payload);
        // raw deflate (реальные серверы) — через fallback.
        assert_eq!(
            decode(&dec, &raw_deflate(&payload), "deflate").unwrap(),
            payload
        );
    }

    #[test]
    fn brotli_roundtrip() {
        let payload: Vec<u8> = "brotli brotli brotli ".repeat(512).into_bytes();
        let dec = RustBodyDecoder::new();
        let out = decode(&dec, &br(&payload), "br").expect("brotli decodes");
        assert_eq!(out, payload);
    }

    #[test]
    fn zstd_reference_frame_and_roundtrip() {
        let dec = RustBodyDecoder::new();
        // Эталонный кадр по RFC 8878: magic, FHD 0x20 (single segment,
        // FCS 1 байт = 5), блок raw last=1 size=5, «hello».
        let frame = [
            0x28, 0xB5, 0x2F, 0xFD, 0x20, 0x05, 0x29, 0x00, 0x00, b'h', b'e', b'l', b'l', b'o',
        ];
        assert_eq!(decode(&dec, &frame, "zstd").expect("zstd"), b"hello");

        let text = "привет, zstd! ".repeat(500);
        let packed = ruzstd::encoding::compress_to_vec(
            text.as_bytes(),
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        assert!(packed.len() < text.len());
        assert_eq!(
            decode(&dec, &packed, "ZSTD").expect("zstd"),
            text.as_bytes()
        );
    }

    #[test]
    fn stacked_encodings_are_decoded_in_reverse_order() {
        let dec = RustBodyDecoder::new();
        // Content-Encoding: gzip, br — сначала gzip, потом br; снимаем br → gzip.
        let packed = br(&gz(b"layered body"));
        assert_eq!(
            decode(&dec, &packed, "gzip, br").expect("stacked"),
            b"layered body"
        );
    }

    #[test]
    fn supported_knows_zstd_and_chains_but_not_unknown() {
        let dec = RustBodyDecoder::new();
        let supported = |enc: &str| futures::executor::block_on(dec.supported(enc));
        assert!(supported("zstd"));
        assert!(supported("gzip, br"));
        assert!(!supported("compress"));
        assert!(!supported("gzip, compress"));
    }

    #[test]
    fn encoding_names_case_insensitive_and_aliases() {
        let payload = b"alias probe";
        let dec = RustBodyDecoder::new();
        assert_eq!(decode(&dec, &gz(payload), "GZIP").unwrap(), payload);
        assert_eq!(decode(&dec, &gz(payload), " x-gzip ").unwrap(), payload);
        assert_eq!(decode(&dec, &br(payload), "BR").unwrap(), payload);
        assert!(supported(&dec, "GZip"));
        assert!(supported(&dec, "br"));
    }

    #[test]
    fn unknown_and_identity_pass_through() {
        let dec = RustBodyDecoder::new();
        let payload = b"plain bytes";
        assert_eq!(decode(&dec, payload, "").unwrap(), payload);
        assert_eq!(decode(&dec, payload, "identity").unwrap(), payload);
        assert_eq!(decode(&dec, payload, "compress").unwrap(), payload);
        assert!(!supported(&dec, "identity"));
        assert!(!supported(&dec, ""));
        assert!(!supported(&dec, "compress"));
    }

    #[test]
    fn corrupt_input_is_invalid_entity_not_panic() {
        let dec = RustBodyDecoder::new();
        let garbage = [0xff, 0x8b, 0x00, 0x11, 0x22];
        for enc in ["gzip", "deflate", "br"] {
            assert!(
                matches!(
                    decode(&dec, &garbage, enc),
                    Err(DomainError::InvalidEntity(_))
                ),
                "{enc} garbage must be InvalidEntity"
            );
        }
    }

    #[test]
    fn oversized_output_hits_guard() {
        // 4 MiB нулей раскрываются заведомо больше лимита 1 MiB.
        let dec = RustBodyDecoder::with_limit(1024 * 1024);
        let bomb = gz(&[0u8; 4 * 1024 * 1024]);
        let err = decode(&dec, &bomb, "gzip").unwrap_err();
        assert!(matches!(err, DomainError::InvalidEntity(_)));
        assert!(err.to_string().contains("exceeds"), "got: {err}");
    }

    #[test]
    fn empty_inputs_roundtrip() {
        let dec = RustBodyDecoder::new();
        for (enc, data) in [("gzip", gz(&[])), ("br", br(&[])), ("deflate", zlib(&[]))] {
            assert_eq!(decode(&dec, &data, enc).unwrap(), Vec::<u8>::new());
        }
    }
}
