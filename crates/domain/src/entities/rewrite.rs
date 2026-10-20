//! Применение Rewrite-правил (как Rewrite в Charles): какие правила ловят
//! сообщение и что их операции делают с URL, статусом, заголовками и телом.
//! Чистые функции без IO — их зовёт пайплайн перехвата.
//!
//! Совпавшие включённые правила применяются ВСЕ, по порядку списка; внутри
//! правила — операции по порядку.

use super::rule::{RewriteLocation, RewriteOp, RewriteRule, RuleSet};
use super::rule_routing::RequestFacts;
use crate::value_objects::MatchTarget;
use crate::value_objects::http_vo::HeaderEntry;
use crate::value_objects::regex_cache::compiled;

/// Что правило может изменить. `url` — только у запроса, `status` — только
/// у ответа; `body` — `None`, если тело не загружено (операции над телом
/// пропускаются).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteTarget {
    pub url: Option<String>,
    pub status: Option<u16>,
    pub headers: Vec<HeaderEntry>,
    pub body: Option<Vec<u8>>,
}

/// Ответ, по которому матчатся правила фазы ответа.
#[derive(Debug, Clone, Copy)]
pub struct ResponseFacts<'a> {
    pub status: u16,
    pub content_type: Option<&'a str>,
}

impl RewriteRule {
    /// Есть ли операции над телом (пайплайну нужно загрузить тело).
    pub fn touches_body(&self) -> bool {
        self.ops
            .iter()
            .any(|op| matches!(op, RewriteOp::BodyFindReplace { .. }))
    }

    /// Правило ловит сообщение. Без условия — всё; у ответа учитываются
    /// статус и Content-Type, `body_regex` не поддерживается (как у
    /// маршрутизации).
    fn catches(&self, request: &RequestFacts<'_>, response: Option<ResponseFacts<'_>>) -> bool {
        let Some(m) = &self.r#match else {
            return true;
        };
        let (status, content_type) = match response {
            Some(r) => (Some(r.status), r.content_type),
            None => (None, None),
        };
        m.body_regex.is_none()
            && m.matches(&MatchTarget {
                status,
                content_type,
                ..request.target()
            })
    }

    /// Применить операции правила.
    pub fn apply(&self, target: RewriteTarget) -> RewriteTarget {
        self.ops.iter().fold(target, apply_op)
    }
}

impl RuleSet {
    /// Правила фазы, совпавшие с сообщением (`response = None` — запрос).
    pub fn rewrites_for<'a>(
        &'a self,
        request: &RequestFacts<'_>,
        response: Option<ResponseFacts<'_>>,
    ) -> Vec<&'a RewriteRule> {
        if !self.rewrite_enabled {
            return Vec::new();
        }
        let location = match response {
            Some(_) => RewriteLocation::Response,
            None => RewriteLocation::Request,
        };
        self.rewrite
            .iter()
            .filter(|rule| rule.enabled && rule.location == location)
            .filter(|rule| rule.catches(request, response))
            .collect()
    }
}

fn apply_op(target: RewriteTarget, op: &RewriteOp) -> RewriteTarget {
    match op {
        RewriteOp::AddHeader { name, value } => RewriteTarget {
            headers: target
                .headers
                .into_iter()
                .chain(std::iter::once(header(name, value)))
                .collect(),
            ..target
        },
        RewriteOp::ReplaceHeader { name, value } => RewriteTarget {
            headers: replace_header(target.headers, name, value),
            ..target
        },
        RewriteOp::RemoveHeader { name } => RewriteTarget {
            headers: target
                .headers
                .into_iter()
                .filter(|h| !h.name.eq_ignore_ascii_case(name.trim()))
                .collect(),
            ..target
        },
        RewriteOp::BodyFindReplace {
            find,
            replace,
            regex,
        } => RewriteTarget {
            body: target
                .body
                .map(|body| replace_body(body, find, replace, *regex)),
            ..target
        },
        RewriteOp::UrlRewrite {
            find,
            replace,
            regex,
        } => RewriteTarget {
            url: target
                .url
                .map(|url| replace_text(&url, find, replace, *regex)),
            ..target
        },
        RewriteOp::SetStatus { status } => RewriteTarget {
            status: target.status.map(|_| *status),
            ..target
        },
    }
}

fn header(name: &str, value: &str) -> HeaderEntry {
    HeaderEntry {
        name: name.trim().to_string(),
        value: value.to_string(),
    }
}

/// Все заголовки с этим именем → одно значение на месте первого;
/// отсутствующий — добавить в конец.
fn replace_header(headers: Vec<HeaderEntry>, name: &str, value: &str) -> Vec<HeaderEntry> {
    let name = name.trim();
    let mut replaced = false;
    let mut out: Vec<HeaderEntry> = headers
        .into_iter()
        .filter_map(|h| {
            if !h.name.eq_ignore_ascii_case(name) {
                return Some(h);
            }
            if replaced {
                return None;
            }
            replaced = true;
            Some(HeaderEntry {
                value: value.to_string(),
                ..h
            })
        })
        .collect();
    if !replaced {
        out.push(header(name, value));
    }
    out
}

/// Замена в теле: только текстовое (UTF-8) тело, двоичное не трогается.
fn replace_body(body: Vec<u8>, find: &str, replace: &str, regex: bool) -> Vec<u8> {
    match String::from_utf8(body) {
        Ok(text) => replace_text(&text, find, replace, regex).into_bytes(),
        Err(error) => error.into_bytes(),
    }
}

/// Подстрока или regex (`${1}` / `$$` в замене); битый regex — без
/// изменений (валидация — в UpdateRuleSetUseCase).
fn replace_text(text: &str, find: &str, replace: &str, regex: bool) -> String {
    if !regex {
        return text.replace(find, replace);
    }
    match compiled(find) {
        Some(re) => re.replace_all(text, replace).into_owned(),
        None => text.to_string(),
    }
}

#[cfg(test)]
#[path = "rewrite_tests.rs"]
mod tests;
