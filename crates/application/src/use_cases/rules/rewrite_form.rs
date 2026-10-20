//! Форма диалога Rewrite: операции полями (вид + два поля + regex) вместо
//! enum'а, проверка и сборка правила.

use domain::entities::rule::{RewriteLocation, RewriteOp, RewriteRule};
use domain::value_objects::TrafficMatch;

use super::location_form::{LocationErrors, LocationForm};

/// Вид операции в выпадающем списке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    AddHeader,
    ReplaceHeader,
    RemoveHeader,
    BodyReplace,
    UrlReplace,
    SetStatus,
}

impl OpKind {
    pub const ALL: [OpKind; 6] = [
        OpKind::AddHeader,
        OpKind::ReplaceHeader,
        OpKind::RemoveHeader,
        OpKind::BodyReplace,
        OpKind::UrlReplace,
        OpKind::SetStatus,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OpKind::AddHeader => "Add header",
            OpKind::ReplaceHeader => "Set header",
            OpKind::RemoveHeader => "Remove header",
            OpKind::BodyReplace => "Replace in body",
            OpKind::UrlReplace => "Replace in URL",
            OpKind::SetStatus => "Set status",
        }
    }

    /// Подсказки полей: (первое, второе — `None`, если поля нет).
    pub fn placeholders(self) -> (&'static str, Option<&'static str>) {
        match self {
            OpKind::AddHeader | OpKind::ReplaceHeader => ("Header name", Some("Value")),
            OpKind::RemoveHeader => ("Header name", None),
            OpKind::BodyReplace | OpKind::UrlReplace => (
                "Find",
                Some("Replace with (regex: ${1} — group, $$ — literal $)"),
            ),
            OpKind::SetStatus => ("Status, e.g. 503", None),
        }
    }

    /// Есть ли у вида переключатель regex.
    pub fn has_regex(self) -> bool {
        matches!(self, OpKind::BodyReplace | OpKind::UrlReplace)
    }

    /// Только для фазы ответа.
    pub fn response_only(self) -> bool {
        self == OpKind::SetStatus
    }
}

/// Операция в форме.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpForm {
    pub kind: OpKind,
    pub first: String,
    pub second: String,
    pub regex: bool,
}

impl OpForm {
    pub fn new(kind: OpKind) -> Self {
        Self {
            kind,
            first: String::new(),
            second: String::new(),
            regex: false,
        }
    }

    pub fn from_op(op: &RewriteOp) -> Self {
        let form = |kind, first: &str, second: &str, regex| Self {
            kind,
            first: first.to_string(),
            second: second.to_string(),
            regex,
        };
        match op {
            RewriteOp::AddHeader { name, value } => form(OpKind::AddHeader, name, value, false),
            RewriteOp::ReplaceHeader { name, value } => {
                form(OpKind::ReplaceHeader, name, value, false)
            }
            RewriteOp::RemoveHeader { name } => form(OpKind::RemoveHeader, name, "", false),
            RewriteOp::BodyFindReplace {
                find,
                replace,
                regex,
            } => form(OpKind::BodyReplace, find, replace, *regex),
            RewriteOp::UrlRewrite {
                find,
                replace,
                regex,
            } => form(OpKind::UrlReplace, find, replace, *regex),
            RewriteOp::SetStatus { status } => {
                form(OpKind::SetStatus, &status.to_string(), "", false)
            }
        }
    }

    /// Операция или текст ошибки для поля.
    pub fn to_op(&self) -> Result<RewriteOp, String> {
        let first = self.first.trim();
        let required = |what: &str| -> Result<String, String> {
            if first.is_empty() {
                Err(format!("{what} is required"))
            } else {
                Ok(first.to_string())
            }
        };
        let check_regex = |find: &str| -> Result<(), String> {
            match self.regex && regex::Regex::new(find).is_err() {
                true => Err(format!("invalid regex: {find}")),
                false => Ok(()),
            }
        };
        Ok(match self.kind {
            OpKind::AddHeader => RewriteOp::AddHeader {
                name: required("Header name")?,
                value: self.second.clone(),
            },
            OpKind::ReplaceHeader => RewriteOp::ReplaceHeader {
                name: required("Header name")?,
                value: self.second.clone(),
            },
            OpKind::RemoveHeader => RewriteOp::RemoveHeader {
                name: required("Header name")?,
            },
            OpKind::BodyReplace => {
                // Find в теле — как введено (пробелы значимы).
                let find = if self.first.is_empty() {
                    required("Find")?
                } else {
                    self.first.clone()
                };
                check_regex(&find)?;
                RewriteOp::BodyFindReplace {
                    find,
                    replace: self.second.clone(),
                    regex: self.regex,
                }
            }
            OpKind::UrlReplace => {
                let find = required("Find")?;
                check_regex(&find)?;
                RewriteOp::UrlRewrite {
                    find,
                    replace: self.second.trim().to_string(),
                    regex: self.regex,
                }
            }
            // 1xx итоговым ответом движок не отдаёт (ломает HTTP/1).
            OpKind::SetStatus => match first.parse::<u16>() {
                Ok(status) if (200..=599).contains(&status) => RewriteOp::SetStatus { status },
                _ => return Err("status must be a number 200–599".into()),
            },
        })
    }
}

/// Форма правила целиком.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteForm {
    pub name: String,
    pub location: RewriteLocation,
    /// Пустые Host и Path — все запросы/ответы.
    pub matching: LocationForm,
    pub ops: Vec<OpForm>,
}

/// Ошибки формы: общая и по операциям (индекс → текст).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RewriteErrors {
    pub form: Option<String>,
    /// В коробке: ошибки полей условия крупные, а `Err` должен быть лёгким.
    pub location: Box<LocationErrors>,
    pub ops: Vec<(usize, String)>,
}

impl RewriteErrors {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn op(&self, index: usize) -> Option<&str> {
        self.ops
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, e)| e.as_str())
    }
}

/// Поля правила после проверки.
#[derive(Debug, Clone, PartialEq)]
pub struct RewriteParts {
    pub name: String,
    pub location: RewriteLocation,
    pub r#match: Option<TrafficMatch>,
    pub ops: Vec<RewriteOp>,
}

impl RewriteForm {
    pub fn from_rule(rule: &RewriteRule) -> Self {
        Self {
            name: rule.name.clone(),
            location: rule.location,
            matching: rule
                .r#match
                .as_ref()
                .map(LocationForm::from_match)
                .unwrap_or_default(),
            ops: rule.ops.iter().map(OpForm::from_op).collect(),
        }
    }

    pub fn validate(&self) -> Result<RewriteParts, RewriteErrors> {
        let mut errors = RewriteErrors::default();
        if self.name.trim().is_empty() {
            errors.form = Some("Name is required".into());
        } else if self.ops.is_empty() {
            errors.form = Some("Add at least one operation".into());
        }
        let r#match = match self.matching.to_optional_match() {
            Ok(m) => m,
            Err(e) => {
                errors.location = Box::new(e);
                None
            }
        };
        let mut ops = Vec::new();
        for (index, op) in self.ops.iter().enumerate() {
            if op.kind.response_only() && self.location == RewriteLocation::Request {
                errors
                    .ops
                    .push((index, "Set status applies to responses only".into()));
                continue;
            }
            match op.to_op() {
                Ok(op) => ops.push(op),
                Err(error) => errors.ops.push((index, error)),
            }
        }
        match errors.is_empty() {
            true => Ok(RewriteParts {
                name: self.name.trim().to_string(),
                location: self.location,
                r#match,
                ops,
            }),
            false => Err(errors),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(kind: OpKind, first: &str, second: &str, regex: bool) -> OpForm {
        OpForm {
            kind,
            first: first.into(),
            second: second.into(),
            regex,
        }
    }

    fn form(ops: Vec<OpForm>) -> RewriteForm {
        RewriteForm {
            name: "Fix".into(),
            location: RewriteLocation::Response,
            matching: LocationForm {
                host: "api.test".into(),
                path: "/v1/*".into(),
                method: None,
                ..LocationForm::default()
            },
            ops,
        }
    }

    #[test]
    fn valid_form_builds_parts_and_roundtrips() {
        let f = form(vec![
            op(OpKind::ReplaceHeader, "Cache-Control", "no-store", false),
            op(OpKind::BodyReplace, " prod", " test", false),
            op(OpKind::SetStatus, "503", "", false),
        ]);
        let parts = f.validate().expect("valid");
        assert_eq!(parts.ops.len(), 3);
        assert_eq!(
            parts.r#match.as_ref().and_then(|m| m.host_glob.as_deref()),
            Some("api.test")
        );
        assert_eq!(
            parts.ops[1],
            RewriteOp::BodyFindReplace {
                find: " prod".into(),
                replace: " test".into(),
                regex: false
            }
        );
        let rule = RewriteRule {
            id: domain::value_objects::RuleId::new(),
            name: parts.name,
            enabled: true,
            location: parts.location,
            r#match: parts.r#match,
            ops: parts.ops,
        };
        assert_eq!(RewriteForm::from_rule(&rule), f);
    }

    #[test]
    fn empty_location_means_everything_but_method_still_filters() {
        let mut f = form(vec![op(OpKind::RemoveHeader, "Server", "", false)]);
        f.matching = LocationForm::default();
        assert_eq!(f.validate().expect("valid").r#match, None);
        f.matching.method = Some("POST".into());
        let m = f
            .validate()
            .expect("valid")
            .r#match
            .expect("method condition");
        assert_eq!((m.method.as_deref(), m.host_glob), (Some("POST"), None));
    }

    #[test]
    fn errors_point_at_fields_and_ops() {
        let mut f = form(vec![
            op(OpKind::AddHeader, " ", "1", false),
            op(OpKind::UrlReplace, "(", "", true),
            op(OpKind::SetStatus, "abc", "", false),
        ]);
        f.name = String::new();
        let errors = f.validate().unwrap_err();
        assert!(errors.form.is_some());
        assert!(errors.op(0).is_some() && errors.op(1).is_some() && errors.op(2).is_some());

        let mut request = form(vec![op(OpKind::SetStatus, "500", "", false)]);
        request.location = RewriteLocation::Request;
        assert!(
            request
                .validate()
                .unwrap_err()
                .op(0)
                .is_some_and(|e| e.contains("responses only"))
        );
    }
}
