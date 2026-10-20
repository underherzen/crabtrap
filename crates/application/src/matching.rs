//! Факты запроса для матча правил: разбор URI перехваченного сообщения
//! (полный URL, хост, path + query) и имя GraphQL-операции из query или
//! JSON-тела. Общий для всех пайплайнов и Test скрипта.
//!
//! Имя операции ищется только по запросу (`with_graphql`) — тело
//! разбирается, лишь если какое-то правило условие на операцию задаёт.

use std::sync::OnceLock;

use domain::entities::RequestFacts;
use serde::Deserialize;

use crate::services::InterceptableMessage;

/// Тело больше этого не разбирается в поисках GraphQL-операции.
const MAX_GRAPHQL_BODY_BYTES: usize = 1024 * 1024;

/// Владеемые факты запроса (`facts()` — заимствованный вид для домена).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTarget {
    url: url::Url,
    host: String,
    path: String,
    method: String,
    graphql_operation: Option<String>,
}

impl RequestTarget {
    /// Факты перехваченного запроса (`None` — URI не абсолютный URL);
    /// `graphql` — искать имя операции (в query и загруженном теле).
    pub fn of_message(message: &InterceptableMessage, graphql: bool) -> Option<Self> {
        let target = Self::parse(
            message.uri.as_deref()?,
            message.method.as_deref().unwrap_or_default(),
        )?;
        Some(match graphql {
            true => target.with_graphql(message.body.as_deref()),
            false => target,
        })
    }

    /// Факты по URI и методу (без GraphQL-операции).
    pub fn parse(uri: &str, method: &str) -> Option<Self> {
        let url = url::Url::parse(uri).ok()?;
        let path = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_string(),
        };
        Some(Self {
            host: url.host_str().unwrap_or_default().to_string(),
            graphql_operation: None,
            url,
            path,
            method: method.to_string(),
        })
    }

    /// С именем GraphQL-операции из query или `body`.
    pub fn with_graphql(self, body: Option<&[u8]>) -> Self {
        Self {
            graphql_operation: graphql_operation(&self.url, body),
            ..self
        }
    }

    /// URL в том виде, в каком с ним сверяется `url_regex` (нормализованный:
    /// хост в нижнем регистре, без порта по умолчанию, `/` у пустого пути).
    pub fn url(&self) -> &url::Url {
        &self.url
    }

    pub fn facts(&self) -> RequestFacts<'_> {
        RequestFacts {
            url: self.url.as_str(),
            host: &self.host,
            path: &self.path,
            method: &self.method,
            graphql_operation: self.graphql_operation.as_deref(),
        }
    }
}

/// Имя GraphQL-операции: `operationName` (query или JSON-тело), иначе имя
/// из документа `query` (`query GetUser { … }`). Пакет (JSON-массив) — по
/// первой операции. `None` — не GraphQL или операция анонимная.
pub fn graphql_operation(url: &url::Url, body: Option<&[u8]>) -> Option<String> {
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    param("operationName")
        .filter(|name| !name.is_empty())
        .or_else(|| param("query").and_then(|doc| document_operation(&doc)))
        .or_else(|| body.and_then(body_operation))
}

/// Поля GraphQL-запроса, нужные для имени операции (остальное — `variables`
/// и т.п. — serde пропускает, не строя дерево JSON).
#[derive(Deserialize)]
struct GraphqlRequest {
    #[serde(rename = "operationName")]
    operation_name: Option<String>,
    query: Option<String>,
}

/// Один запрос или пакет (первый определяет операцию).
#[derive(Deserialize)]
#[serde(untagged)]
enum GraphqlBody {
    Single(GraphqlRequest),
    Batch(Vec<GraphqlRequest>),
}

fn body_operation(body: &[u8]) -> Option<String> {
    let first = body.iter().find(|b| !b.is_ascii_whitespace())?;
    if body.len() > MAX_GRAPHQL_BODY_BYTES || !matches!(first, b'{' | b'[') {
        return None;
    }
    let request = match serde_json::from_slice(body).ok()? {
        GraphqlBody::Single(single) => single,
        GraphqlBody::Batch(batch) => batch.into_iter().next()?,
    };
    request
        .operation_name
        .filter(|name| !name.is_empty())
        .or_else(|| request.query.as_deref().and_then(document_operation))
}

/// Имя первой именованной операции документа (комментарии `#` пропускаются).
fn document_operation(document: &str) -> Option<String> {
    static OPERATION: OnceLock<regex::Regex> = OnceLock::new();
    let re = OPERATION.get_or_init(|| {
        regex::Regex::new(
            r"^(?:\s|#[^\n]*\n)*(?:query|mutation|subscription)\s+([_A-Za-z][_0-9A-Za-z]*)",
        )
        .expect("static regex")
    });
    re.captures(document).map(|c| c[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(uri: &str, body: Option<&str>) -> Option<String> {
        let url = url::Url::parse(uri).expect("url");
        graphql_operation(&url, body.map(str::as_bytes))
    }

    #[test]
    fn operation_from_json_body() {
        let body = r#"{"operationName":"GetUser","query":"query GetUser { me { id } }"}"#;
        assert_eq!(
            op("https://a.test/graphql", Some(body)).as_deref(),
            Some("GetUser")
        );
        let unnamed = r#"{"query":"  # list\n mutation DeleteUser($id: ID!) { x }"}"#;
        assert_eq!(
            op("https://a.test/graphql", Some(unnamed)).as_deref(),
            Some("DeleteUser")
        );
        let batch = r#"[{"operationName":"First"},{"operationName":"Second"}]"#;
        assert_eq!(
            op("https://a.test/graphql", Some(batch)).as_deref(),
            Some("First")
        );
    }

    #[test]
    fn operation_from_query_string() {
        assert_eq!(
            op("https://a.test/graphql?operationName=Feed&query=x", None).as_deref(),
            Some("Feed")
        );
        assert_eq!(
            op(
                "https://a.test/graphql?query=query%20Search%20%7B%20x%20%7D",
                None
            )
            .as_deref(),
            Some("Search")
        );
    }

    #[test]
    fn not_graphql_or_anonymous_is_none() {
        assert_eq!(op("https://a.test/graphql", Some("{ me { id } }")), None);
        assert_eq!(
            op("https://a.test/graphql", Some(r#"{"query":"{ me }"}"#)),
            None
        );
        assert_eq!(op("https://a.test/api", Some("plain text")), None);
        assert_eq!(op("https://a.test/api", Some(r#"{"a":1}"#)), None);
        assert_eq!(op("https://a.test/api", None), None);
    }

    #[test]
    fn target_facts_carry_url_path_and_operation() {
        let target = RequestTarget::parse("https://API.test:8443/v1/x?y=1", "POST")
            .expect("target")
            .with_graphql(Some(br#"{"operationName":"Op","variables":{"id":[1,2]}}"#));
        let facts = target.facts();
        assert_eq!(facts.url, "https://api.test:8443/v1/x?y=1");
        assert_eq!(facts.host, "api.test");
        assert_eq!(facts.path, "/v1/x?y=1");
        assert_eq!(facts.method, "POST");
        assert_eq!(facts.graphql_operation, Some("Op"));
        assert_eq!(RequestTarget::parse("/relative", "GET"), None);
        let plain =
            RequestTarget::parse("https://a.test/graphql?operationName=Op", "GET").expect("target");
        assert_eq!(
            plain.facts().graphql_operation,
            None,
            "the operation is looked up only on request"
        );
    }
}
