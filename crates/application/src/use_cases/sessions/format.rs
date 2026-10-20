//! Файл сессии `.crabtrap`: gzip поверх NDJSON. Первая строка — заголовок
//! (формат, версия, число обменов), дальше по строке на обмен: сам обмен
//! (как в репозитории), его тела (base64, по id ссылки) и сообщения
//! WebSocket. Пишется и читается потоком — сессия не держится в памяти
//! целиком; строка больше [`MAX_RECORD_BYTES`] — ошибка (чужой файл не
//! должен съесть память).

use std::io::{BufRead, BufReader, Read, Write};

use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use domain::entities::exchange::HttpExchange;
use domain::entities::websocket::WebSocketMessage;
use domain::errors::DomainError;
use domain::value_objects::BodyId;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};

/// Метка формата в заголовке.
pub const FORMAT: &str = "crabtrap-session";
/// Версия формата, которую пишет эта сборка.
pub const VERSION: u32 = 1;
/// Потолок одной строки (обмен с телами).
pub const MAX_RECORD_BYTES: u64 = 512 * 1024 * 1024;
/// Сколько пустых строк подряд терпится: поток из одних `\n` (gzip-бомба)
/// не должен крутиться бесконечно.
const MAX_BLANK_LINES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHeader {
    pub format: String,
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub exchange_count: u64,
}

/// Тело обмена: id ссылки (`BodyRef.id`) и байты.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredBody {
    pub id: BodyId,
    #[serde(with = "base64_bytes")]
    pub data: Vec<u8>,
}

/// Строка файла: обмен с его телами и сообщениями.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub exchange: HttpExchange,
    #[serde(default)]
    pub bodies: Vec<StoredBody>,
    #[serde(default)]
    pub ws_messages: Vec<WebSocketMessage>,
}

/// Запись файла сессии.
pub struct SessionWriter<W: Write> {
    out: GzEncoder<W>,
}

impl<W: Write> SessionWriter<W> {
    pub fn new(out: W, header: &SessionHeader) -> Result<Self, DomainError> {
        let mut writer = Self {
            out: GzEncoder::new(out, Compression::default()),
        };
        writer.line(header)?;
        Ok(writer)
    }

    /// Дописать обмен. Строка, которую чтение не примет (больше
    /// [`MAX_RECORD_BYTES`]), — ошибка сразу, а не файл, который не
    /// откроется.
    pub fn push(&mut self, record: &SessionRecord) -> Result<(), DomainError> {
        let line = serde_json::to_vec(record)
            .map_err(|e| DomainError::InfrastructureError(format!("write session: {e}")))?;
        if line.len() as u64 > MAX_RECORD_BYTES {
            return Err(DomainError::ValidationError(format!(
                "the request to {} is larger than 512 MiB with its bodies and can't be saved",
                record.exchange.request.uri
            )));
        }
        self.out.write_all(&line).map_err(write_error)?;
        self.out.write_all(b"\n").map_err(write_error)
    }

    /// Дописать gzip-хвост и вернуть поток.
    pub fn finish(self) -> Result<W, DomainError> {
        self.out.finish().map_err(write_error)
    }

    fn line(&mut self, value: &impl Serialize) -> Result<(), DomainError> {
        serde_json::to_writer(&mut self.out, value)
            .map_err(|e| DomainError::InfrastructureError(format!("write session: {e}")))?;
        self.out.write_all(b"\n").map_err(write_error)
    }
}

/// Чтение файла сессии: заголовок проверяется сразу.
pub struct SessionReader<R: Read> {
    lines: BufReader<GzDecoder<R>>,
    pub header: SessionHeader,
}

impl<R: Read> SessionReader<R> {
    pub fn open(input: R) -> Result<Self, DomainError> {
        let mut lines = BufReader::new(GzDecoder::new(input));
        let header: SessionHeader = match next_line(&mut lines) {
            Ok(Some(line)) => serde_json::from_slice(&line).map_err(|_| not_a_session())?,
            Ok(None) | Err(LineError::Io(_) | LineError::Blank) => return Err(not_a_session()),
            Err(LineError::TooLong) => return Err(too_long()),
        };
        if header.format != FORMAT {
            return Err(not_a_session());
        }
        if header.version > VERSION {
            return Err(DomainError::ValidationError(format!(
                "the session was saved by a newer crabtrap (format {}); update crabtrap to open it",
                header.version
            )));
        }
        Ok(Self { lines, header })
    }

    /// Следующий обмен; `None` — файл кончился.
    pub fn next_record(&mut self) -> Result<Option<SessionRecord>, DomainError> {
        match next_line(&mut self.lines) {
            Ok(Some(line)) => serde_json::from_slice(&line)
                .map(Some)
                .map_err(|e| damaged(&e.to_string())),
            Ok(None) => Ok(None),
            Err(LineError::TooLong) => Err(too_long()),
            Err(LineError::Blank) => Err(damaged("too many empty lines")),
            // Обрезанный или испорченный gzip посреди записей.
            Err(LineError::Io(error)) => Err(damaged(&error.to_string())),
        }
    }
}

enum LineError {
    Io(std::io::Error),
    TooLong,
    Blank,
}

/// Строка без `\n` (байты — JSON разберёт и проверит UTF-8 сам); пустые
/// пропускаются, но не бесконечно. Содержимое длиннее потолка — ошибка.
fn next_line<R: BufRead>(input: &mut R) -> Result<Option<Vec<u8>>, LineError> {
    for _ in 0..=MAX_BLANK_LINES {
        let mut line = Vec::new();
        let read = input
            .by_ref()
            .take(MAX_RECORD_BYTES + 2)
            .read_until(b'\n', &mut line)
            .map_err(LineError::Io)?;
        if read == 0 {
            return Ok(None);
        }
        while line.last().is_some_and(|b| matches!(b, b'\n' | b'\r')) {
            line.pop();
        }
        if line.len() as u64 > MAX_RECORD_BYTES {
            return Err(LineError::TooLong);
        }
        if !line.is_empty() {
            return Ok(Some(line));
        }
    }
    Err(LineError::Blank)
}

fn not_a_session() -> DomainError {
    DomainError::ValidationError("this is not a crabtrap session file".into())
}

fn damaged(detail: &str) -> DomainError {
    DomainError::ValidationError(format!("the session file is damaged: {detail}"))
}

fn too_long() -> DomainError {
    DomainError::ValidationError("the session file has an exchange larger than 512 MiB".into())
}

fn write_error(error: std::io::Error) -> DomainError {
    DomainError::InfrastructureError(format!("write session: {error}"))
}

mod base64_bytes {
    use super::STANDARD;
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::entities::exchange::HttpRequest;
    use domain::value_objects::http_vo::{HttpVersionVo, Method, Protocol};
    use domain::value_objects::{ConnectionId, Host};

    fn exchange() -> HttpExchange {
        HttpExchange::new_pending(
            ConnectionId::new(),
            Host::new("api.test").expect("host"),
            HttpRequest {
                method: Method::new("GET"),
                uri: "https://api.test/x".into(),
                version: HttpVersionVo::Http11,
                headers: Vec::new(),
                body: None,
                raw_size: 0,
                origin: Default::default(),
            },
            Protocol::Http1,
            Utc::now(),
        )
    }

    fn header(count: u64) -> SessionHeader {
        SessionHeader {
            format: FORMAT.into(),
            version: VERSION,
            created_at: Utc::now(),
            exchange_count: count,
        }
    }

    #[test]
    fn records_round_trip_with_binary_bodies() {
        let record = SessionRecord {
            exchange: exchange(),
            bodies: vec![StoredBody {
                id: BodyId::new(),
                data: vec![0, 255, 10, 13],
            }],
            ws_messages: Vec::new(),
        };
        let mut writer = SessionWriter::new(Vec::new(), &header(1)).expect("writer");
        writer.push(&record).expect("push");
        let bytes = writer.finish().expect("finish");

        let mut reader = SessionReader::open(bytes.as_slice()).expect("reader");
        assert_eq!(reader.header.exchange_count, 1);
        assert_eq!(reader.next_record().expect("record"), Some(record));
        assert_eq!(reader.next_record().expect("end"), None);
    }

    #[test]
    fn foreign_and_newer_files_are_refused() {
        let plain = b"{\"log\":{}}".as_slice();
        assert!(SessionReader::open(plain).is_err(), "not gzip");
        let other = SessionHeader {
            format: "something-else".into(),
            ..header(0)
        };
        let bytes = SessionWriter::new(Vec::new(), &other)
            .expect("w")
            .finish()
            .expect("f");
        assert!(SessionReader::open(bytes.as_slice()).is_err());
        let newer = SessionHeader {
            version: VERSION + 1,
            ..header(0)
        };
        let bytes = SessionWriter::new(Vec::new(), &newer)
            .expect("w")
            .finish()
            .expect("f");
        let error = SessionReader::open(bytes.as_slice())
            .err()
            .expect("newer refused");
        assert!(error.to_string().contains("newer crabtrap"), "{error}");
    }

    #[test]
    fn a_truncated_file_keeps_the_records_before_the_cut() {
        let record = SessionRecord {
            exchange: exchange(),
            bodies: vec![StoredBody {
                id: BodyId::new(),
                data: (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect(),
            }],
            ws_messages: Vec::new(),
        };
        let mut writer = SessionWriter::new(Vec::new(), &header(2)).expect("w");
        writer.push(&record).expect("first");
        writer.push(&record).expect("second");
        let bytes = writer.finish().expect("f");
        let cut = &bytes[..bytes.len() - 40];

        let mut reader = SessionReader::open(cut).expect("header ok");
        assert!(reader.next_record().expect("first survives").is_some());
        let error = reader.next_record().expect_err("second is cut");
        assert!(error.to_string().contains("damaged"), "{error}");
    }

    #[test]
    fn endless_blank_lines_stop_with_an_error() {
        let mut writer = SessionWriter::new(Vec::new(), &header(0)).expect("w");
        writer
            .out
            .write_all(&vec![b'\n'; MAX_BLANK_LINES + 10])
            .expect("raw");
        let bytes = writer.finish().expect("f");
        let mut reader = SessionReader::open(bytes.as_slice()).expect("header ok");
        assert!(reader.next_record().is_err());
    }

    #[test]
    fn a_damaged_record_is_an_error_not_a_panic() {
        let mut writer = SessionWriter::new(Vec::new(), &header(1)).expect("w");
        writer.out.write_all(b"{not json}\n").expect("raw");
        let bytes = writer.finish().expect("f");
        let mut reader = SessionReader::open(bytes.as_slice()).expect("header ok");
        assert!(reader.next_record().is_err());
    }
}
