//! Разбор WebSocket-фреймов (RFC 6455) из потока байт — для показа
//! сообщений; сами байты пересылаются как есть. Фрагментированные сообщения
//! собираются до FIN, управляющие фреймы (ping/pong/close) — сразу.
//! Содержимое хранится до потолка, размер — полный.

use domain::entities::websocket::{WS_PAYLOAD_CAP, WsOpcode};

/// Собранное сообщение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedMessage {
    pub opcode: WsOpcode,
    pub payload: Vec<u8>,
    pub size: u64,
}

/// Заголовок текущего фрейма, тело которого ещё читается.
#[derive(Debug)]
struct FrameState {
    opcode: u8,
    fin: bool,
    mask: Option<[u8; 4]>,
    remaining: u64,
    /// Позиция в маске (для фрагментов тела между чтениями).
    offset: u64,
}

/// Состояние парсера одного направления.
#[derive(Debug, Default)]
pub(crate) struct WsParser {
    /// Непрочитанный хвост заголовка фрейма.
    header: Vec<u8>,
    frame: Option<FrameState>,
    /// Собираемое data-сообщение (opcode, содержимое, размер).
    message: Option<(WsOpcode, Vec<u8>, u64)>,
    /// Тело управляющего фрейма.
    control: Vec<u8>,
    /// Поток не похож на WebSocket — дальше не разбираем.
    broken: bool,
}

impl WsParser {
    /// Скормить прочитанные байты; вернуть завершённые сообщения.
    pub(crate) fn feed(&mut self, mut bytes: &[u8]) -> Vec<ParsedMessage> {
        let mut out = Vec::new();
        while !bytes.is_empty() && !self.broken {
            match self.frame.take() {
                None => {
                    let take = bytes.len().min(14 - self.header.len().min(14));
                    self.header.extend_from_slice(&bytes[..take]);
                    match parse_header(&self.header) {
                        HeaderParse::Incomplete => bytes = &bytes[take..],
                        HeaderParse::Invalid => {
                            tracing::debug!(
                                "websocket stream is not RFC 6455 framing, messages not parsed further"
                            );
                            self.broken = true;
                        }
                        HeaderParse::Done(frame, used) => {
                            // Лишнее, что попало в буфер заголовка, — уже тело.
                            let extra = self.header.len() - used;
                            bytes = &bytes[take - extra..];
                            self.header.clear();
                            self.start_frame(&frame);
                            if frame.remaining == 0 {
                                out.extend(self.finish_frame(&frame));
                            } else {
                                self.frame = Some(frame);
                            }
                        }
                    }
                }
                Some(mut frame) => {
                    let take = (bytes.len() as u64).min(frame.remaining) as usize;
                    // Снимаем маску только с байт, которые сохраним.
                    let stored = take.min(self.room(&frame));
                    let chunk = unmask(&bytes[..stored], frame.mask, frame.offset);
                    self.append(&frame, take, &chunk);
                    frame.remaining -= take as u64;
                    frame.offset += take as u64;
                    bytes = &bytes[take..];
                    if frame.remaining == 0 {
                        out.extend(self.finish_frame(&frame));
                    } else {
                        self.frame = Some(frame);
                    }
                }
            }
        }
        out
    }

    fn start_frame(&mut self, frame: &FrameState) {
        match frame.opcode {
            0x1 | 0x2 => {
                let opcode = if frame.opcode == 0x1 {
                    WsOpcode::Text
                } else {
                    WsOpcode::Binary
                };
                self.message = Some((opcode, Vec::new(), 0));
            }
            0x8..=0xA => self.control.clear(),
            _ => {}
        }
    }

    /// Сколько ещё байт содержимого текущего сообщения поместится.
    fn room(&self, frame: &FrameState) -> usize {
        let used = match frame.opcode {
            0x0..=0x2 => self
                .message
                .as_ref()
                .map_or(WS_PAYLOAD_CAP, |(_, payload, _)| payload.len()),
            _ => self.control.len(),
        };
        WS_PAYLOAD_CAP.saturating_sub(used)
    }

    /// `seen` — сколько байт тела прошло, `chunk` — их сохраняемая часть.
    fn append(&mut self, frame: &FrameState, seen: usize, chunk: &[u8]) {
        let target = match frame.opcode {
            0x0..=0x2 => self.message.as_mut().map(|(_, payload, size)| {
                *size += seen as u64;
                payload
            }),
            _ => Some(&mut self.control),
        };
        if let Some(payload) = target {
            let room = WS_PAYLOAD_CAP.saturating_sub(payload.len());
            payload.extend_from_slice(&chunk[..chunk.len().min(room)]);
        }
    }

    fn finish_frame(&mut self, frame: &FrameState) -> Option<ParsedMessage> {
        match frame.opcode {
            0x0..=0x2 if frame.fin => {
                self.message
                    .take()
                    .map(|(opcode, payload, size)| ParsedMessage {
                        opcode,
                        payload,
                        size,
                    })
            }
            0x8..=0xA => {
                let opcode = match frame.opcode {
                    0x8 => WsOpcode::Close,
                    0x9 => WsOpcode::Ping,
                    _ => WsOpcode::Pong,
                };
                let payload = std::mem::take(&mut self.control);
                Some(ParsedMessage {
                    opcode,
                    size: payload.len() as u64,
                    payload,
                })
            }
            _ => None,
        }
    }
}

enum HeaderParse {
    Incomplete,
    Invalid,
    Done(FrameState, usize),
}

fn parse_header(buf: &[u8]) -> HeaderParse {
    if buf.len() < 2 {
        return HeaderParse::Incomplete;
    }
    let fin = buf[0] & 0x80 != 0;
    let opcode = buf[0] & 0x0F;
    if !matches!(opcode, 0x0..=0x2 | 0x8..=0xA) {
        return HeaderParse::Invalid;
    }
    let masked = buf[1] & 0x80 != 0;
    let (length, mut used) = match buf[1] & 0x7F {
        126 if buf.len() >= 4 => (u64::from(u16::from_be_bytes([buf[2], buf[3]])), 4),
        127 if buf.len() >= 10 => {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&buf[2..10]);
            // Старший бит 64-битной длины обязан быть нулём (RFC 6455).
            if raw[0] & 0x80 != 0 {
                return HeaderParse::Invalid;
            }
            (u64::from_be_bytes(raw), 10)
        }
        126 | 127 => return HeaderParse::Incomplete,
        short => (u64::from(short), 2),
    };
    let mask = if masked {
        if buf.len() < used + 4 {
            return HeaderParse::Incomplete;
        }
        let key = [buf[used], buf[used + 1], buf[used + 2], buf[used + 3]];
        used += 4;
        Some(key)
    } else {
        None
    };
    HeaderParse::Done(
        FrameState {
            opcode,
            fin,
            mask,
            remaining: length,
            offset: 0,
        },
        used,
    )
}

fn unmask(chunk: &[u8], mask: Option<[u8; 4]>, offset: u64) -> Vec<u8> {
    match mask {
        None => chunk.to_vec(),
        Some(key) => chunk
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ key[((offset + i as u64) % 4) as usize])
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Фрейм (опционально с маской) для тестов.
    fn frame(fin: bool, opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
        let mut out = vec![(if fin { 0x80 } else { 0 }) | opcode];
        let mask_bit = if mask.is_some() { 0x80 } else { 0 };
        match payload.len() {
            n if n < 126 => out.push(mask_bit | n as u8),
            n if n <= u16::MAX as usize => {
                out.push(mask_bit | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(mask_bit | 127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        if let Some(key) = mask {
            out.extend_from_slice(&key);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        } else {
            out.extend_from_slice(payload);
        }
        out
    }

    #[test]
    fn masked_text_split_across_reads_byte_by_byte() {
        let bytes = frame(true, 0x1, b"hello", Some([1, 2, 3, 4]));
        let mut parser = WsParser::default();
        let mut out = Vec::new();
        for b in &bytes {
            out.extend(parser.feed(std::slice::from_ref(b)));
        }
        assert_eq!(
            out,
            vec![ParsedMessage {
                opcode: WsOpcode::Text,
                payload: b"hello".to_vec(),
                size: 5
            }]
        );
    }

    #[test]
    fn fragments_with_interleaved_ping_and_several_frames_per_read() {
        let mut bytes = frame(false, 0x1, b"hel", None);
        bytes.extend(frame(true, 0x9, b"p", None));
        bytes.extend(frame(true, 0x0, b"lo", None));
        bytes.extend(frame(true, 0x2, &[0, 1, 2], None));
        let out = WsParser::default().feed(&bytes);
        let kinds: Vec<_> = out.iter().map(|m| (m.opcode, m.payload.clone())).collect();
        assert_eq!(
            kinds,
            vec![
                (WsOpcode::Ping, b"p".to_vec()),
                (WsOpcode::Text, b"hello".to_vec()),
                (WsOpcode::Binary, vec![0, 1, 2]),
            ]
        );
    }

    #[test]
    fn long_payload_is_capped_but_size_is_full() {
        let payload = vec![b'x'; WS_PAYLOAD_CAP + 10];
        let out = WsParser::default().feed(&frame(true, 0x2, &payload, Some([9, 9, 9, 9])));
        assert_eq!(out[0].payload.len(), WS_PAYLOAD_CAP);
        assert_eq!(out[0].size, payload.len() as u64);
    }

    #[test]
    fn length_with_msb_set_is_invalid() {
        let mut bytes = vec![0x82, 127];
        bytes.extend_from_slice(&u64::MAX.to_be_bytes());
        let mut parser = WsParser::default();
        assert!(parser.feed(&bytes).is_empty());
        assert!(
            parser.feed(&frame(true, 0x1, b"x", None)).is_empty(),
            "stays broken"
        );
    }

    #[test]
    fn garbage_stops_parsing() {
        let mut parser = WsParser::default();
        assert!(parser.feed(&[0x83, 0x00]).is_empty());
        assert!(parser.feed(&frame(true, 0x1, b"x", None)).is_empty());
    }
}
