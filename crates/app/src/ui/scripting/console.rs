//! Консоль скриптов: записи `ProxyEvent::ScriptLog` по правилам — панель
//! Console окна Script Editor. Пишет pump событий, читают окна редакторов.

use std::collections::{HashMap, VecDeque};

use application::dto::capture::ProxyEvent;
use application::dto::scripting::ScriptLogLevel;
use chrono::{DateTime, Local};
use domain::value_objects::RuleId;
use gpui_kit::{App, AppContext as _, Entity};

/// Записей на правило: старые вытесняются.
const LINES_PER_RULE: usize = 500;

/// Строка консоли.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    pub at: DateTime<Local>,
    pub level: ScriptLogLevel,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct ScriptConsoleStore {
    lines: HashMap<RuleId, VecDeque<ConsoleLine>>,
}

impl ScriptConsoleStore {
    /// Store консоли (один на приложение: пишет pump, читают окна).
    pub fn install(cx: &mut App) -> Entity<Self> {
        cx.new(|_| Self::default())
    }

    /// Применить пачку событий; `true` — консоль изменилась.
    pub fn apply_batch(&mut self, events: &[ProxyEvent]) -> bool {
        let mut changed = false;
        for event in events {
            if let ProxyEvent::ScriptLog {
                rule_id,
                level,
                message,
                ts,
                ..
            } = event
            {
                self.push(
                    *rule_id,
                    ConsoleLine {
                        at: ts.with_timezone(&Local),
                        level: *level,
                        message: message.clone(),
                    },
                );
                changed = true;
            }
        }
        changed
    }

    pub fn push(&mut self, rule: RuleId, line: ConsoleLine) {
        let lines = self.lines.entry(rule).or_default();
        if lines.len() >= LINES_PER_RULE {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn lines(&self, rule: &RuleId) -> impl Iterator<Item = &ConsoleLine> {
        self.lines.get(rule).into_iter().flatten()
    }

    pub fn clear(&mut self, rule: &RuleId) {
        self.lines.remove(rule);
    }

    /// Вся консоль правила текстом (для выделения и Copy all).
    pub fn text(&self, rule: &RuleId) -> String {
        console_text(self.lines(rule))
    }
}

/// Текст консоли: строка на запись — время, маркер уровня (✖ ошибка,
/// ⚠ предупреждение) и сообщение; многострочное сообщение — как есть.
pub fn console_text<'a>(lines: impl Iterator<Item = &'a ConsoleLine>) -> String {
    lines
        .map(|line| {
            let marker = match line.level {
                ScriptLogLevel::Error => "✖ ",
                ScriptLogLevel::Warn => "⚠ ",
                ScriptLogLevel::Debug | ScriptLogLevel::Info | ScriptLogLevel::Log => "",
            };
            format!("{}  {marker}{}", line.at.format("%H:%M:%S"), line.message)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn log(rule: RuleId, message: &str) -> ProxyEvent {
        ProxyEvent::ScriptLog {
            rule_id: rule,
            exchange_id: None,
            level: ScriptLogLevel::Log,
            message: message.into(),
            ts: Utc::now(),
        }
    }

    #[test]
    fn console_text_marks_levels_and_keeps_multiline_messages() {
        let at = chrono::Local::now();
        let line = |level, message: &str| ConsoleLine {
            at,
            level,
            message: message.into(),
        };
        let lines = [
            line(ScriptLogLevel::Log, "{\n  \"a\": 1\n}"),
            line(ScriptLogLevel::Warn, "careful"),
            line(ScriptLogLevel::Error, "boom"),
        ];
        let time = at.format("%H:%M:%S");
        assert_eq!(
            console_text(lines.iter()),
            format!("{time}  {{\n  \"a\": 1\n}}\n{time}  ⚠ careful\n{time}  ✖ boom")
        );
        assert_eq!(console_text(std::iter::empty()), "");
    }

    #[test]
    fn logs_are_kept_per_rule_and_capped() {
        let (a, b) = (RuleId::new(), RuleId::new());
        let mut store = ScriptConsoleStore::default();
        assert!(store.apply_batch(&[log(a, "1"), log(b, "x")]));
        assert!(!store.apply_batch(&[ProxyEvent::ConnectionClosed {
            id: domain::value_objects::ConnectionId::new(),
        }]));
        let only_a: Vec<_> = store.lines(&a).map(|l| l.message.as_str()).collect();
        assert_eq!(only_a, ["1"]);

        let flood: Vec<_> = (0..LINES_PER_RULE + 5)
            .map(|i| log(a, &i.to_string()))
            .collect();
        store.apply_batch(&flood);
        assert_eq!(store.lines(&a).count(), LINES_PER_RULE);
        assert_eq!(
            store.lines(&a).next().map(|l| l.message.as_str()),
            Some("5")
        );

        store.clear(&a);
        assert_eq!(store.lines(&a).count(), 0);
        assert_eq!(store.lines(&b).count(), 1);
    }
}
