//! Вкладки инструментов (SSL Proxying, Block List, Map Local, Map Remote):
//! одна раскладка — «Enable …», пояснение, таблица, кнопки — с данными
//! конкретного инструмента. Диалоги добавления/правки — [`super::dialog`].

use std::rc::Rc;

use application::use_cases::rules::edit::{
    MapLocalResponse, Tool, format_location, tool_enabled, with_rule_duplicated, with_rule_toggled,
    with_tool_enabled, without_rule,
};
use application::use_cases::rules::location_form::{LocationForm, RemoteTargetForm};
use application::use_cases::rules::response_form::ResponseForm;
use domain::entities::rule::{
    BlockAction, BreakPhase, DEFAULT_MAP_LOCAL_STATUS, MapLocalRule, RewriteLocation, RewriteRule,
    RuleSet,
};
use domain::entities::script_rule::ScriptRule;
use domain::value_objects::RuleId;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement, Styled, Window, div, px,
};

use super::dialog::{DialogInit, DialogKind, open_location_dialog};
use super::rewrite_dialog::open_rewrite_dialog;
use super::store::RulesStore;
use super::table::{
    RuleRow, RuleWindowHandler, TableHandlers, enable_checkbox, rule_actions, rule_table,
};
use super::{Prefill, RulesScreen, RulesTab};
use crate::ui::scripting::window as scripting_window;

/// Чем добавляется и правится правило вкладки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleEditor {
    Dialog(DialogKind),
    /// Диалог Rewrite (список операций).
    Rewrite,
    /// Отдельное окно Script Editor.
    Script,
}

/// Всё, чем вкладки инструментов отличаются друг от друга.
struct ToolSpec {
    tool: Tool,
    editor: RuleEditor,
    id: &'static str,
    enable_label: &'static str,
    description: &'static str,
    columns: (&'static str, &'static str),
    empty: &'static str,
}

fn spec(tab: RulesTab) -> ToolSpec {
    match tab {
        RulesTab::SslProxying => ToolSpec {
            tool: Tool::SslProxying,
            editor: RuleEditor::Dialog(DialogKind::SslHost),
            id: "ssl",
            enable_label: "Enable SSL Proxying",
            description: "HTTPS to these hosts is decrypted so you can see it (the crabtrap CA must be trusted). Other hosts pass through as tunnels.",
            columns: ("Host", "Mode"),
            empty: "No hosts yet. Add one, e.g. api.example.com, *.example.com or * for all.",
        },
        RulesTab::AllowList => ToolSpec {
            tool: Tool::AllowList,
            editor: RuleEditor::Dialog(DialogKind::AllowHost),
            id: "allow",
            enable_label: "Enable Allow List",
            description: "Record only these hosts. Everything else passes through crabtrap unrecorded and untouched by rules — no decryption, no throttling.",
            columns: ("Host", "Recording"),
            empty: "No hosts yet. Add one, e.g. api.example.com or *.example.com — or right-click a request → Allow Only.",
        },
        RulesTab::BlockList => ToolSpec {
            tool: Tool::BlockList,
            editor: RuleEditor::Dialog(DialogKind::Block),
            id: "block",
            enable_label: "Enable Block List",
            description: "Requests to these locations never reach the server.",
            columns: ("Location", "Action"),
            empty: "Nothing is blocked.",
        },
        RulesTab::MapLocal => ToolSpec {
            tool: Tool::MapLocal,
            editor: RuleEditor::Dialog(DialogKind::MapLocal),
            id: "local",
            enable_label: "Enable Map Local",
            description: "Responses for these locations come from a local file (or a folder + request path).",
            columns: ("Location", "Local file"),
            empty: "No local mappings.",
        },
        RulesTab::MapRemote | RulesTab::Throttle => ToolSpec {
            tool: Tool::MapRemote,
            editor: RuleEditor::Dialog(DialogKind::MapRemote),
            id: "remote",
            enable_label: "Enable Map Remote",
            description: "Requests for these locations are sent to another server; path and query are kept unless the target sets them.",
            columns: ("Location", "Map to"),
            empty: "No remote mappings.",
        },
        RulesTab::Rewrite => ToolSpec {
            tool: Tool::Rewrite,
            editor: RuleEditor::Rewrite,
            id: "rewrite",
            enable_label: "Enable Rewrite",
            description: "Change matching requests or responses on the fly: headers, text in the body or URL (plain or regex), response status. All matching rules apply, top to bottom.",
            columns: ("Location", "Rewrite"),
            empty: "No rewrite rules.",
        },
        RulesTab::Breakpoints => ToolSpec {
            tool: Tool::Breakpoints,
            editor: RuleEditor::Dialog(DialogKind::Breakpoint),
            id: "breakpoint",
            enable_label: "Enable Breakpoints",
            description: "Matching requests or responses pause until you Execute (optionally edited) or Abort them in the breakpoint window. Unanswered for 5 minutes — they continue unchanged.",
            columns: ("Location", "Stop on"),
            empty: "No breakpoints.",
        },
        RulesTab::Scripting => ToolSpec {
            tool: Tool::Scripting,
            editor: RuleEditor::Script,
            id: "script",
            enable_label: "Enable Scripting",
            description: "JavaScript runs on matching requests and responses: change the URL, headers, status or body, or answer as a mock API. Double-click a script to open it in the editor.",
            columns: ("Location", "Script"),
            empty: "No scripts. Add one to open the Script Editor.",
        },
    }
}

/// Открыть правку правила: диалог или окно Script Editor.
fn open_editor(
    editor: RuleEditor,
    store: &Entity<RulesStore>,
    id: RuleId,
    window: &mut Window,
    cx: &mut App,
) {
    match editor {
        RuleEditor::Dialog(kind) => {
            let rules = store.read(cx).rules.clone();
            if let Some(init) = init_for_edit(&rules, kind, id) {
                open_location_dialog(kind, store.clone(), init, window, cx);
            }
        }
        RuleEditor::Rewrite => {
            let rule = store
                .read(cx)
                .rules
                .rewrite
                .iter()
                .find(|r| r.id == id)
                .cloned();
            if let Some(rule) = rule {
                open_rewrite_dialog(
                    store.clone(),
                    Some(&rule),
                    LocationForm::default(),
                    window,
                    cx,
                );
            }
        }
        RuleEditor::Script => scripting_window::open_rule(id, cx),
    }
}

/// Добавить правило: пустой диалог или новый скрипт.
fn open_new(
    editor: RuleEditor,
    store: &Entity<RulesStore>,
    location: LocationForm,
    window: &mut Window,
    cx: &mut App,
) {
    match editor {
        RuleEditor::Dialog(kind) => {
            let init = DialogInit {
                location,
                ..DialogInit::default()
            };
            open_location_dialog(kind, store.clone(), init, window, cx);
        }
        RuleEditor::Rewrite => open_rewrite_dialog(store.clone(), None, location, window, cx),
        RuleEditor::Script => scripting_window::open_new(location, cx),
    }
}

/// Строки таблицы инструмента.
fn rows(rules: &RuleSet, tool: Tool) -> Vec<RuleRow> {
    match tool {
        Tool::SslProxying => rules
            .ssl_proxy
            .iter()
            .map(|e| RuleRow {
                id: e.id,
                enabled: e.enabled,
                location: e.host_pattern.clone(),
                target: if e.exclude {
                    "Don't decrypt"
                } else {
                    "Decrypt"
                }
                .into(),
            })
            .collect(),
        Tool::AllowList => rules
            .allow_list
            .iter()
            .map(|e| RuleRow {
                id: e.id,
                enabled: e.enabled,
                location: e.host_pattern.clone(),
                target: "Recorded".into(),
            })
            .collect(),
        Tool::BlockList => rules
            .blocks
            .iter()
            .map(|r| RuleRow {
                id: r.id,
                enabled: r.enabled,
                location: format_location(&r.r#match),
                target: match r.action {
                    BlockAction::Status(code) => format!("Respond {code}"),
                    BlockAction::Close => "Drop connection".into(),
                },
            })
            .collect(),
        Tool::MapLocal => rules
            .map_local
            .iter()
            .map(|r| RuleRow {
                id: r.id,
                enabled: r.enabled,
                location: format_location(&r.r#match),
                target: map_local_target(r),
            })
            .collect(),
        Tool::MapRemote | Tool::Throttle => rules
            .map_remote
            .iter()
            .map(|r| RuleRow {
                id: r.id,
                enabled: r.enabled,
                location: format_location(&r.r#match),
                target: if r.preserve_host {
                    format!("{}  (keep Host)", r.remote)
                } else {
                    r.remote.clone()
                },
            })
            .collect(),
        Tool::Rewrite => rules
            .rewrite
            .iter()
            .map(|r| RuleRow {
                id: r.id,
                enabled: r.enabled,
                location: r
                    .r#match
                    .as_ref()
                    .map_or_else(|| "All".to_string(), format_location),
                target: rewrite_target(r),
            })
            .collect(),
        Tool::Breakpoints => rules
            .breakpoints
            .iter()
            .map(|r| RuleRow {
                id: r.id,
                enabled: r.enabled,
                location: format_location(&r.r#match),
                target: phase_label(r.phase).into(),
            })
            .collect(),
        Tool::Scripting => rules
            .scripts
            .iter()
            .map(|r| RuleRow {
                id: r.id,
                enabled: r.enabled,
                location: format_location(&r.r#match),
                target: script_target(r),
            })
            .collect(),
    }
}

fn phase_label(phase: BreakPhase) -> &'static str {
    match phase {
        BreakPhase::Request => "Request",
        BreakPhase::Response => "Response",
        BreakPhase::Both => "Request and response",
    }
}

/// «Имя — Response · 2 operations».
fn rewrite_target(rule: &RewriteRule) -> String {
    let phase = match rule.location {
        RewriteLocation::Request => "Request",
        RewriteLocation::Response => "Response",
    };
    let ops = match rule.ops.len() {
        1 => "1 operation".to_string(),
        n => format!("{n} operations"),
    };
    format!("{} — {phase} · {ops}", rule.name)
}

/// «Имя — Request · Response» / «Имя — Mock API».
fn script_target(rule: &ScriptRule) -> String {
    let phases: Vec<&str> = [
        (rule.mock_api, "Mock API"),
        (rule.run_on_request && !rule.mock_api, "Request"),
        (rule.run_on_response, "Response"),
    ]
    .into_iter()
    .filter_map(|(on, label)| on.then_some(label))
    .collect();
    format!("{} — {}", rule.name, phases.join(" · "))
}

/// Колонка «Local file»: путь, а нестандартный ответ — в скобках.
fn map_local_target(rule: &MapLocalRule) -> String {
    let path = rule.local_path.display();
    match (rule.status, rule.content_type.as_deref()) {
        (DEFAULT_MAP_LOCAL_STATUS, None) => path.to_string(),
        (status, None) => format!("{path}  ({status})"),
        (status, Some(ct)) => format!("{path}  ({status}, {ct})"),
    }
}

/// Значения диалога правки существующего правила.
fn init_for_edit(rules: &RuleSet, kind: DialogKind, id: RuleId) -> Option<DialogInit> {
    let base = |location: LocationForm| DialogInit {
        editing: Some(id),
        location,
        ..DialogInit::default()
    };
    match kind {
        DialogKind::SslHost => rules
            .ssl_proxy
            .iter()
            .find(|e| e.id == id)
            .map(|e| DialogInit {
                ssl_exclude: e.exclude,
                ..base(LocationForm {
                    host: e.host_pattern.clone(),
                    ..LocationForm::default()
                })
            }),
        DialogKind::AllowHost => rules.allow_list.iter().find(|e| e.id == id).map(|e| {
            base(LocationForm {
                host: e.host_pattern.clone(),
                ..LocationForm::default()
            })
        }),
        DialogKind::Block => rules
            .blocks
            .iter()
            .find(|r| r.id == id)
            .map(|r| DialogInit {
                block_close: r.action == BlockAction::Close,
                ..base(LocationForm::from_match(&r.r#match))
            }),
        DialogKind::MapLocal => rules
            .map_local
            .iter()
            .find(|r| r.id == id)
            .map(|r| DialogInit {
                local_path: r.local_path.display().to_string(),
                response: ResponseForm::from_response(&MapLocalResponse {
                    status: r.status,
                    content_type: r.content_type.clone(),
                    headers: r.headers.clone(),
                }),
                ..base(LocationForm::from_match(&r.r#match))
            }),
        DialogKind::MapRemote => rules
            .map_remote
            .iter()
            .find(|r| r.id == id)
            .map(|r| DialogInit {
                remote: Some(RemoteTargetForm::from_url(&r.remote)),
                keep_host: r.preserve_host,
                ..base(LocationForm::from_match(&r.r#match))
            }),
        DialogKind::Breakpoint => {
            rules
                .breakpoints
                .iter()
                .find(|r| r.id == id)
                .map(|r| DialogInit {
                    break_phase: r.phase,
                    ..base(LocationForm::from_match(&r.r#match))
                })
        }
        DialogKind::ThrottleScope => None,
    }
}

impl RulesScreen {
    pub(super) fn tool_tab(&mut self, tab: RulesTab, cx: &mut Context<Self>) -> AnyElement {
        let spec = spec(tab);
        let rules = self.store.read(cx).rules.clone();
        let rows = rows(&rules, spec.tool);
        let store = self.store.clone();
        let weak = cx.entity().downgrade();

        let tool = spec.tool;
        let enable = enable_checkbox(spec.id, spec.enable_label, tool_enabled(&rules, tool), {
            let store = store.clone();
            move |checked, cx| {
                RulesStore::apply(&store, cx, move |rules| {
                    Ok(with_tool_enabled(rules, tool, checked))
                });
            }
        });

        let editor = spec.editor;
        let open_edit: RuleWindowHandler = Rc::new({
            let store = store.clone();
            move |id, window, cx| open_editor(editor, &store, id, window, cx)
        });
        let handlers = TableHandlers {
            select: Rc::new({
                let weak = weak.clone();
                move |id, cx| {
                    let _ = weak.update(cx, |s, cx| {
                        s.selected = Some(id);
                        cx.notify();
                    });
                }
            }),
            toggle: Rc::new({
                let store = store.clone();
                move |id, cx| {
                    RulesStore::apply(&store, cx, move |rules| Ok(with_rule_toggled(rules, id)));
                }
            }),
            open: open_edit.clone(),
        };
        let selected = self.selected.filter(|id| rows.iter().any(|r| r.id == *id));
        let table = rule_table(
            spec.id,
            spec.columns,
            &rows,
            selected,
            spec.empty,
            &handlers,
            cx,
        );

        let actions = rule_actions(
            spec.id,
            selected.is_some(),
            {
                let store = store.clone();
                move |window, cx| open_new(editor, &store, LocationForm::default(), window, cx)
            },
            move |window, cx| {
                if let Some(id) = selected {
                    open_edit(id, window, cx);
                }
            },
            {
                let store = store.clone();
                move |cx| {
                    if let Some(id) = selected {
                        RulesStore::apply(&store, cx, move |rules| {
                            Ok(with_rule_duplicated(rules, id))
                        });
                    }
                }
            },
            {
                let (store, weak) = (store.clone(), weak.clone());
                move |cx| {
                    if let Some(id) = selected {
                        RulesStore::apply(&store, cx, move |rules| Ok(without_rule(rules, id)));
                        let _ = weak.update(cx, |s, _| s.selected = None);
                    }
                }
            },
        );

        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(enable)
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(cx.theme().muted_foreground)
                    .child(spec.description),
            )
            .child(table)
            .child(actions)
            .into_any_element()
    }

    /// Диалог добавления с полями из трафика (правый клик → Map Local… и т.п.).
    pub(super) fn open_prefilled_dialog(
        &mut self,
        prefill: Prefill,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (self.tab, prefill.mock) {
            (RulesTab::Throttle, _) => {}
            (RulesTab::MapLocal, Some(mock)) => {
                let init = DialogInit {
                    location: prefill.location,
                    local_path: mock.local_path.display().to_string(),
                    response: ResponseForm::from_response(&mock.response),
                    ..DialogInit::default()
                };
                open_location_dialog(DialogKind::MapLocal, self.store.clone(), init, window, cx);
            }
            (tab, _) => open_new(spec(tab).editor, &self.store, prefill.location, window, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use application::use_cases::rules::edit::{
        parse_location, upsert_block, upsert_map_local, upsert_map_remote,
    };
    use domain::value_objects::http_vo::HeaderEntry;

    #[test]
    fn map_local_edit_restores_response_and_row_shows_it() {
        let loc = |s: &str| parse_location(s).expect("ok");
        let teapot = MapLocalResponse {
            status: 418,
            content_type: Some("text/plain".into()),
            headers: vec![HeaderEntry::new("X-Mock", "1")],
        };
        let rules = upsert_map_local(
            RuleSet::default(),
            None,
            loc("a.com"),
            "/m/a.json".into(),
            teapot,
        );
        let rules = upsert_map_local(
            rules,
            None,
            loc("b.com"),
            "/m/b.json".into(),
            MapLocalResponse::default(),
        );
        let rules = upsert_map_local(
            rules,
            None,
            loc("c.com"),
            "/m/c.json".into(),
            MapLocalResponse {
                status: 404,
                content_type: None,
                headers: Vec::new(),
            },
        );

        let init =
            init_for_edit(&rules, DialogKind::MapLocal, rules.map_local[0].id).expect("local");
        assert_eq!(
            (
                init.response.status.as_str(),
                init.response.content_type.as_str()
            ),
            ("418", "text/plain")
        );
        assert_eq!(init.response.headers, "X-Mock: 1\n");
        let targets: Vec<String> = rows(&rules, Tool::MapLocal)
            .into_iter()
            .map(|r| r.target)
            .collect();
        assert_eq!(
            targets,
            [
                "/m/a.json  (418, text/plain)",
                "/m/b.json",
                "/m/c.json  (404)"
            ]
        );
    }

    #[test]
    fn edit_init_restores_fields_of_the_rule() {
        let loc = || parse_location("api.com/v1/*").expect("ok");
        let rules = upsert_block(RuleSet::default(), None, loc(), BlockAction::Close);
        let rules = upsert_map_remote(
            rules,
            None,
            loc(),
            "https://s.example.com:8443/m".into(),
            true,
        );

        let block = init_for_edit(&rules, DialogKind::Block, rules.blocks[0].id).expect("block");
        assert_eq!(block.location.host, "api.com");
        assert_eq!(block.location.path, "/v1/*");
        assert!(block.block_close);

        let remote =
            init_for_edit(&rules, DialogKind::MapRemote, rules.map_remote[0].id).expect("remote");
        let target = remote.remote.expect("target");
        assert!(target.https && remote.keep_host);
        assert_eq!(
            (target.host.as_str(), target.port.as_str()),
            ("s.example.com", "8443")
        );
        assert!(init_for_edit(&rules, DialogKind::Block, RuleId::new()).is_none());
    }

    #[test]
    fn rows_describe_targets() {
        let rules = upsert_block(
            RuleSet::default(),
            None,
            parse_location("a.com").expect("ok"),
            BlockAction::Status(403),
        );
        let r = rows(&rules, Tool::BlockList);
        assert_eq!(
            (r[0].location.as_str(), r[0].target.as_str()),
            ("a.com/*", "Respond 403")
        );
    }
}
