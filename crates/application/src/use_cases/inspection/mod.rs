//! Инспектор обменов (экран деталей; UI-часть — Phase 5).
//!
//! Phase 5 дополнительно: текстовая грамматика фильтра таблицы
//! ([`parse_filter_query`]) и поиск по захваченному трафику
//! ([`SearchExchangesUseCase`]).

mod charset;
mod filter_parser;
mod get_exchange_detail;
mod search;

pub use charset::{decode_charset, sniff_charset, split_content_type};
pub use filter_parser::parse_filter_query;
pub use get_exchange_detail::GetExchangeDetailUseCase;
pub use search::{SearchExchangesUseCase, SearchHitDto, SearchQueryDto, SearchScope};
