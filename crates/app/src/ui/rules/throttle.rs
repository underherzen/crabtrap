//! Вкладка Throttle: «Enable Throttling», пресеты, свои значения (kbps / мс)
//! и область действия (весь трафик или location через диалог).

use application::use_cases::rules::edit::{
    CUSTOM_THROTTLE, THROTTLE_PRESETS, Tool, format_location, tool_enabled, with_throttle_custom,
    with_throttle_preset, with_throttle_scope, with_tool_enabled,
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, App, Context, Entity, IntoElement, ParentElement, Styled, div, px};

use super::RulesScreen;
use super::dialog::{DialogInit, DialogKind, open_location_dialog};
use super::store::RulesStore;
use super::table::enable_checkbox;

/// Потолок своих значений: 10 Гбит/с — больше прокси всё равно не прокачает.
const MAX_KBPS: u64 = 10_000_000;

/// Байт/сек ↔ килобит/сек (как в Charles: пропускная способность в kbps).
/// Вход ограничен [`MAX_KBPS`] в `parse_custom`, переполнения нет.
fn kbps_to_bytes(kbps: u64) -> u64 {
    kbps * 1000 / 8
}

fn bytes_to_kbps(bps: u64) -> u64 {
    bps * 8 / 1000
}

/// Свои значения из полей: `Err` — понятная ошибка.
pub fn parse_custom(down: &str, up: &str, latency: &str) -> Result<(u64, u64, u64), String> {
    let number = |name: &str, value: &str| {
        value
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("{name}: enter a whole number"))
    };
    let (down, up, latency) = (
        number("Download kbps", down)?,
        number("Upload kbps", up)?,
        number("Latency ms", latency)?,
    );
    if down == 0 || up == 0 {
        return Err("Bandwidth must be greater than 0 kbps".into());
    }
    if down > MAX_KBPS || up > MAX_KBPS {
        return Err(format!("Bandwidth must be at most {MAX_KBPS} kbps"));
    }
    Ok((kbps_to_bytes(down), kbps_to_bytes(up), latency))
}

impl RulesScreen {
    pub(super) fn throttle_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let rules = self.store.read(cx).rules.clone();
        let current = rules.throttle.clone();
        let store = self.store.clone();
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);

        let enable = enable_checkbox(
            "throttle",
            "Enable Throttling",
            tool_enabled(&rules, Tool::Throttle),
            {
                let store = store.clone();
                move |checked, cx| {
                    RulesStore::apply(&store, cx, move |rules| {
                        Ok(with_tool_enabled(rules, Tool::Throttle, checked))
                    });
                }
            },
        );

        let presets = THROTTLE_PRESETS.iter().enumerate().map(|(ix, preset)| {
            let store = store.clone();
            let inputs = self.throttle_inputs();
            let active = current.as_ref().is_some_and(|t| t.name == preset.name);
            let button = Button::new(("throttle-preset", ix)).label(preset.name);
            let button = if active { button.primary() } else { button };
            button
                .on_click(move |_, window, cx| {
                    let scope = store
                        .read(cx)
                        .rules
                        .throttle
                        .clone()
                        .and_then(|t| t.r#match);
                    RulesStore::apply(&store, cx, move |rules| {
                        Ok(with_throttle_preset(rules, &THROTTLE_PRESETS[ix], scope))
                    });
                    fill_inputs(&inputs, &THROTTLE_PRESETS[ix], window, cx);
                })
                .into_any_element()
        });

        let custom_active = current.as_ref().is_some_and(|t| t.name == CUSTOM_THROTTLE);
        let apply = Button::new("throttle-apply")
            .label("Apply custom")
            .when(custom_active, |b| b.primary())
            .on_click({
                let (store, inputs, weak) = (
                    store.clone(),
                    self.throttle_inputs(),
                    cx.entity().downgrade(),
                );
                move |_, _, cx| {
                    let text = |i: &Entity<InputState>, cx: &App| i.read(cx).value().to_string();
                    let parsed = parse_custom(
                        &text(&inputs[0], cx),
                        &text(&inputs[1], cx),
                        &text(&inputs[2], cx),
                    );
                    let error = match parsed {
                        Ok((down, up, latency)) => {
                            RulesStore::apply(&store, cx, move |rules| {
                                Ok(with_throttle_custom(rules, down, up, latency))
                            });
                            None
                        }
                        Err(e) => Some(e),
                    };
                    let _ = weak.update(cx, |s, cx| {
                        s.throttle_error = error;
                        cx.notify();
                    });
                }
            });

        let scope_text = match current.as_ref().and_then(|t| t.r#match.as_ref()) {
            Some(m) => format!("Only {}", format_location(m)),
            None => "All traffic".to_string(),
        };
        let limit = Button::new("throttle-scope")
            .label("Limit to location…")
            .disabled(current.is_none())
            .on_click({
                let store = store.clone();
                move |_, window, cx| {
                    open_location_dialog(
                        DialogKind::ThrottleScope,
                        store.clone(),
                        DialogInit::default(),
                        window,
                        cx,
                    )
                }
            });
        let all = Button::new("throttle-all")
            .label("All traffic")
            .disabled(current.as_ref().is_none_or(|t| t.r#match.is_none()))
            .on_click({
                let store = store.clone();
                move |_, _, cx| {
                    RulesStore::apply(&store, cx, |rules| with_throttle_scope(rules, None));
                }
            });

        let status = match &current {
            Some(t) => format!(
                "{} — ↓ {} kbps  ↑ {} kbps  +{} ms",
                t.name,
                t.down_bps.map(bytes_to_kbps).unwrap_or(0),
                t.up_bps.map(bytes_to_kbps).unwrap_or(0),
                t.latency_ms
            ),
            None => "No profile — pick a preset or enter custom values.".into(),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(enable)
            .child(div().text_size(px(13.)).text_color(muted).child(
                "Simulates a slow network: limits bandwidth both ways and adds latency to every request.",
            ))
            .child(div().text_size(px(13.)).child(status))
            .child(section("Presets", div().flex().flex_wrap().gap(px(8.)).children(presets).into_any_element()))
            .child(section(
                "Custom",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap(px(10.))
                            .child(field("Download kbps", &self.throttle.down_kbps))
                            .child(field("Upload kbps", &self.throttle.up_kbps))
                            .child(field("Latency ms", &self.throttle.latency_ms))
                            .child(apply),
                    )
                    .when_some(self.throttle_error.clone(), |el, err| {
                        el.child(div().text_size(px(12.)).text_color(danger).child(err))
                    })
                    .into_any_element(),
            ))
            .child(section(
                "Applies to",
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().text_size(px(13.)).child(scope_text))
                    .child(limit)
                    .child(all)
                    .into_any_element(),
            ))
            .into_any_element()
    }

    fn throttle_inputs(&self) -> [Entity<InputState>; 3] {
        [
            self.throttle.down_kbps.clone(),
            self.throttle.up_kbps.clone(),
            self.throttle.latency_ms.clone(),
        ]
    }
}

/// Пресет заполняет поля своих значений — их можно подправить и Apply.
fn fill_inputs(
    inputs: &[Entity<InputState>; 3],
    preset: &application::use_cases::rules::edit::ThrottlePreset,
    window: &mut gpui_kit::Window,
    cx: &mut App,
) {
    let values = [
        bytes_to_kbps(preset.down_bps),
        bytes_to_kbps(preset.up_bps),
        preset.latency_ms,
    ];
    for (input, value) in inputs.iter().zip(values) {
        input.update(cx, |state, cx| {
            state.set_value(value.to_string(), window, cx)
        });
    }
}

fn section(title: &'static str, body: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(body)
        .into_any_element()
}

fn field(label: &'static str, input: &Entity<InputState>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(3.))
        .child(div().text_size(px(12.)).child(label))
        .child(div().w(px(130.)).child(Input::new(input)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_values_convert_kbps_to_bytes() {
        assert_eq!(parse_custom("768", "384", "150"), Ok((96_000, 48_000, 150)));
    }

    #[test]
    fn custom_values_reject_garbage_and_zero() {
        assert!(
            parse_custom("18446744073709551615", "1", "0").is_err(),
            "no overflow"
        );
        assert!(parse_custom("fast", "1", "0").is_err());
        assert!(parse_custom("0", "1", "0").is_err());
        assert!(parse_custom("1", "1", "-5").is_err());
    }

    #[test]
    fn kbps_roundtrip() {
        assert_eq!(bytes_to_kbps(kbps_to_bytes(768)), 768);
    }
}
