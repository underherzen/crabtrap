//! Редактирование скриптов (Track S): черновик из окна Script Editor →
//! правило в RuleSet. Чистые функции, сохранение — UpdateRuleSetUseCase.

use domain::DomainError;
use domain::entities::rule::RuleSet;
use domain::entities::script_rule::ScriptRule;
use domain::value_objects::RuleId;

use super::edit::upsert;
use super::location_form::LocationForm;

/// Шаблон нового скрипта (как в Proxyman: обе функции с подсказками).
pub const DEFAULT_SCRIPT: &str = r#"/// Called before the request goes to the server (Request checkbox).
/// Change method, url, headers, query or body; return the request.
/// With "Run as Mock API", return { statusCode, headers, body } to answer
/// without hitting the server.
async function onRequest(context, url, request) {
  // console.log(request);

  // request.headers["X-New-Header"] = "crabtrap";
  // request.queries["page"] = "2";

  return request;
}

/// Called before the response goes to the client (Response checkbox).
/// Change statusCode, headers or body (object for JSON, string for text).
async function onResponse(context, url, request, response) {
  // console.log(response);

  // response.headers["Content-Type"] = "application/json";
  // response.statusCode = 500;
  // response.body = { "new-key": "crabtrap" };

  return response;
}
"#;

/// Черновик из окна редактора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptDraft {
    pub name: String,
    /// Host / Path / Method — как в остальных инструментах Tools.
    pub location: LocationForm,
    pub run_on_request: bool,
    pub run_on_response: bool,
    pub mock_api: bool,
    pub source: String,
}

impl ScriptDraft {
    /// Черновик нового скрипта (`location` — из трафика или пустой).
    pub fn new_for(location: LocationForm) -> Self {
        Self {
            name: String::from("Untitled Script"),
            location,
            run_on_request: true,
            run_on_response: true,
            mock_api: false,
            source: DEFAULT_SCRIPT.to_string(),
        }
    }

    /// Черновик существующего правила.
    pub fn from_rule(rule: &ScriptRule) -> Self {
        Self {
            name: rule.name.clone(),
            location: LocationForm::from_match(&rule.r#match),
            run_on_request: rule.run_on_request,
            run_on_response: rule.run_on_response,
            mock_api: rule.mock_api,
            source: rule.source.clone(),
        }
    }
}

/// Добавить (`id = None`) или заменить скрипт; `enabled` правки сохраняется.
/// Возвращает набор и id правила (новый — для окна, которое его открыло).
pub fn upsert_script(
    rules: RuleSet,
    id: Option<RuleId>,
    draft: ScriptDraft,
) -> Result<(RuleSet, RuleId), DomainError> {
    let location = draft.location.to_match().map_err(|errors| {
        DomainError::ValidationError(format!("script location: {}", errors.summary()))
    })?;
    let new_id = id.unwrap_or_default();
    let scripts = upsert(
        rules.scripts,
        id,
        |r| r.id,
        |old| ScriptRule {
            id: old.map_or(new_id, |o| o.id),
            name: draft.name.trim().to_string(),
            r#match: location,
            enabled: old.is_none_or(|o| o.enabled),
            run_on_request: draft.run_on_request,
            run_on_response: draft.run_on_response,
            mock_api: draft.mock_api,
            source: draft.source,
        },
    );
    // Неизвестный id вставляется как новый — с id из аргумента.
    Ok((RuleSet { scripts, ..rules }, new_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(name: &str) -> ScriptDraft {
        ScriptDraft {
            name: name.into(),
            location: LocationForm {
                host: "api.example.com".into(),
                path: "/v1/*".into(),
                method: Some("POST".into()),
                ..LocationForm::default()
            },
            run_on_request: true,
            run_on_response: false,
            mock_api: false,
            source: "async function onRequest(c,u,r){return r}".into(),
        }
    }

    #[test]
    fn new_script_is_appended_with_parsed_location() {
        let (rules, id) = upsert_script(RuleSet::default(), None, draft(" Auth ")).expect("upsert");
        let rule = &rules.scripts[0];
        assert_eq!(rule.id, id);
        assert_eq!(rule.name, "Auth");
        assert_eq!(rule.r#match.host_glob.as_deref(), Some("api.example.com"));
        assert_eq!(rule.r#match.path_glob.as_deref(), Some("/v1/*"));
        assert_eq!(rule.r#match.method.as_deref(), Some("POST"));
        assert!(rule.enabled && rule.run_on_request && !rule.run_on_response);
    }

    #[test]
    fn edit_replaces_in_place_and_keeps_enabled() {
        let (rules, id) = upsert_script(RuleSet::default(), None, draft("a")).expect("upsert");
        let (rules, _) = upsert_script(rules, None, draft("b")).expect("upsert");
        let rules = super::super::edit::with_rule_toggled(rules, id);
        let (rules, same) = upsert_script(rules, Some(id), draft("a2")).expect("upsert");
        assert_eq!(same, id);
        assert_eq!(rules.scripts.len(), 2);
        assert_eq!(rules.scripts[0].name, "a2");
        assert!(!rules.scripts[0].enabled, "toggle survives edit");
    }

    #[test]
    fn draft_roundtrips_through_rule() {
        let (rules, _) = upsert_script(RuleSet::default(), None, draft("r")).expect("upsert");
        let back = ScriptDraft::from_rule(&rules.scripts[0]);
        assert_eq!(back.location, draft("r").location);
        assert_eq!(back.source, draft("r").source);
    }

    #[test]
    fn bad_location_is_rejected() {
        let mut bad = draft("x");
        bad.location.host = "  ".into();
        assert!(upsert_script(RuleSet::default(), None, bad).is_err());
    }
}
