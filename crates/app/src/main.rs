//! crabtrap — композиционный корень: конфиг → infrastructure → engine →
//! GPUI-окно.
//!
//! Phase 4: таблица трафика (DataTable) + capture-конвейер — события
//! прокачиваются через `RecordExchangeEventUseCase` в ExchangeRepository,
//! а TeeConfig движка складывает тела в общий MemorySpillBodyStore.
//! Никакой бизнес-логики: только wiring.

mod app_state;
mod runtime;
mod shutdown;
mod tray;
mod ui;

use std::sync::Arc;

use application::use_cases::RecordExchangeEventUseCase;
use gpui_kit::component::Root;
use gpui_kit::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, px, size,
};
use infrastructure::services::ChannelEventPublisher;

use app_state::AppState;
use ui::CrabtrapRoot;
use ui::status_bar::EngineStatusStore;
use ui::traffic::filter_bar::{bind_filter_input, build_filter_input};
use ui::traffic::search::FocusSearch;
use ui::traffic::search::SearchStore;
use ui::traffic::search::{bind_search_input, build_search_input};
use ui::traffic::store::TrafficStore;
use ui::traffic::table::build_traffic_table;

/// Каталог моков «Save Response & Map Local» внутри app-support.
const MAP_LOCAL_MOCKS_DIR: &str = "map_local";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = config::AppConfig::load_or_init();

    // Инфраструктура + движок (порядок важен: publisher → runtime → state).
    let event_publisher = Arc::new(ChannelEventPublisher::new());
    // Тела: threshold/cap из конфига; инстанс общий для движка (tee) и UI
    // (Phase 5 — инспектор читает по BodyRef).
    let body_repository =
        Arc::new(infrastructure::repositories::MemorySpillBodyStore::from_config(&config));
    // Правила (rules.json): супервизор перепушивает их движку при каждом
    // рестарте (Start после Stop).
    let rule_repository =
        Arc::new(infrastructure::repositories::FileRuleRepository::from_config(&config));
    let engine_cfg = engine::EngineConfig::new(
        config.proxy.listen_addr.clone(),
        config.proxy.max_concurrent_conns,
    )
    .with_body_capture(
        config.capture.record_bodies,
        config.capture.body_capture_cap_bytes,
    );
    // Сертификаты MITM (Phase 3): реальный rcgen-сервис из infrastructure.
    let certificate_service: Arc<dyn application::services::CertificateService> =
        Arc::new(infrastructure::services::RcgenCertificateService::from_config(&config));
    // Скрипты (Track S): пул JS-потоков, общий для движка и пробных
    // запусков из редактора.
    let script_runtime: Arc<dyn application::services::ScriptRuntime> =
        Arc::new(infrastructure::services::BoaScriptRuntime::new());
    // Цепочка перехвата: скрипты → Rewrite → брейкпоинты (пользователь
    // видит итоговое сообщение) → правила (Block / Map Local / Map Remote). Снапшот правил приходит командой UpdateRules от
    // супервизора при старте и при каждом изменении.
    let decoder: Arc<dyn application::services::BodyDecoderService> =
        Arc::new(infrastructure::services::RustBodyDecoder::new());
    let rules_pipeline = Arc::new(infrastructure::services::RulesInterceptionPipeline::new(
        Default::default(),
    ));
    let breakpoint_pipeline = Arc::new(
        infrastructure::services::BreakpointInterceptionPipeline::new(
            rules_pipeline,
            Default::default(),
            event_publisher.clone(),
            decoder.clone(),
        ),
    );
    let rewrite_pipeline = Arc::new(infrastructure::services::RewriteInterceptionPipeline::new(
        breakpoint_pipeline,
        Default::default(),
        decoder.clone(),
    ));
    let pipeline = infrastructure::services::ScriptingInterceptionPipeline::new(
        rewrite_pipeline,
        Default::default(),
        infrastructure::services::ScriptingDeps {
            runtime: script_runtime.clone(),
            decoder,
            events: event_publisher.clone(),
        },
    );
    let pipeline: Arc<dyn application::services::InterceptionPipeline> = Arc::new(pipeline);
    // UI окон брейкпоинтов решает прямо в цепочке (см. ui::breakpoint).
    let pipeline_for_ui = pipeline.clone();
    let engine_runtime = runtime::EngineRuntime::start(
        event_publisher.clone(),
        pipeline,
        certificate_service.clone(),
        body_repository.clone(),
        Some(rule_repository.clone()),
        engine_cfg,
    );
    let state = AppState::compose_with(
        config.clone(),
        event_publisher.clone(),
        engine_runtime.command_handle(),
        body_repository,
        rule_repository,
        certificate_service.clone(),
        script_runtime,
        Arc::new(engine_runtime.direct_client()),
    );
    // Ресивер событий двигается в pump; сам runtime уезжает в on_app_quit
    // (shutdown при выходе).
    let engine_events = engine_runtime.events(&event_publisher);
    let runtime_handle = engine_runtime.runtime_handle();

    // CA создаётся/переиспользуется при старте, fingerprint видно в логе
    // (стабильность между запусками).
    {
        let cert = certificate_service.clone();
        runtime_handle.spawn(async move {
            match application::services::CertificateService::ensure_ca(&*cert).await {
                Ok(status) => println!(
                    "[ssl  ] CA fingerprint={} path={}",
                    status.fingerprint.as_deref().unwrap_or("?"),
                    status.cert_path.display()
                ),
                Err(e) => println!("[ssl  ] CA init failed: {e}"),
            }
        });
    }

    let record_bodies = config.capture.record_bodies;
    let buffer_len = config.capture.buffer_len;
    let width = config.ui.window_width;
    let height = config.ui.window_height;
    let listen_addr = config.proxy.listen_addr.clone();
    let max_concurrent_conns = config.proxy.max_concurrent_conns;
    let pinned_hosts = config.ui.pinned_hosts.clone();
    let sidebar_visible = config.ui.sidebar_visible;
    // kill / Ctrl-C / остановка из IDE: снять системный прокси перед выходом.
    let (quit_tx, quit_rx) = async_channel::bounded::<()>(1);
    shutdown::install_signal_cleanup(&state, &runtime_handle, quit_tx);

    // Движок гасится в on_app_quit (код после run() на macOS недостижим).
    let mut engine_for_quit = Some(engine_runtime);

    gpui_kit::application()
        .with_assets(ui::theme::icons::AppAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            // Liquid Glass: светлая/тёмная палитра по системе до открытия окна.
            ui::theme::apply::apply_system(cx);

            // ⌘F — фокус поля поиска (ловится на корневом div окна).
            cx.bind_keys([gpui_kit::KeyBinding::new("cmd-f", FocusSearch, None)]);

            // Выход: Cmd+Q / меню Exit / закрытие окна (одно окно — как в
            // Charles). Очистка — синхронно в on_app_quit, см. shutdown.rs.
            cx.bind_keys([gpui_kit::KeyBinding::new("cmd-q", shutdown::Quit, None)]);
            cx.on_action(|_: &shutdown::Quit, cx: &mut App| cx.quit());
            // Сигнал завершения (см. shutdown::install_signal_cleanup).
            cx.spawn(async move |cx| {
                if quit_rx.recv().await.is_ok() {
                    cx.update(|cx| cx.quit());
                }
            })
            .detach();
            {
                let state = state.clone();
                let runtime = runtime_handle.clone();
                cx.on_app_quit(move |_| {
                    shutdown::run_quit_cleanup(&state, &runtime, engine_for_quit.take());
                    async {}
                })
                .detach();
            }

            // Хранилища UI + pump событий (батчи по 64, через use case).
            let status_store = cx.new(|_| EngineStatusStore::default());
            let traffic_store = cx.new(|_| {
                let mut store = TrafficStore::new(record_bodies).with_capacity(buffer_len);
                store.set_pinned(pinned_hosts.iter().cloned().collect());
                store
            });
            let recorder = Arc::new(RecordExchangeEventUseCase::new(
                state.exchange_repository.clone(),
                state.body_repository.clone(),
            ));
            // Клиенты соединений (приложение/устройство) — Track G8.
            let clients = ui::clients::build(
                state.client_process_service.clone(),
                runtime_handle.clone(),
                &traffic_store,
                &listen_addr,
                cx,
            );
            // Окна брейкпоинтов — до pump'а (первое InterceptWaiting).
            ui::breakpoint::install(pipeline_for_ui.clone(), cx);
            // Консоль скриптов (Track S): ScriptLog → окна Script Editor.
            let script_console = ui::scripting::console::ScriptConsoleStore::install(cx);
            runtime::pump_events(
                cx,
                engine_events,
                recorder,
                runtime::PumpTargets {
                    status: status_store.clone(),
                    traffic: traffic_store.clone(),
                    clients,
                    script_console: script_console.clone(),
                },
            );

            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
            let options = WindowOptions {
                // Нативный тайтл-бар прозрачный: в нём живёт наш тулбар
                // (TitleBar сам двигает окно и ловит двойной клик → zoom).
                titlebar: Some(TitlebarOptions {
                    title: Some("crabtrap".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(gpui_kit::point(
                        px(ui::theme::metrics::TRAFFIC_LIGHTS_X),
                        px(ui::theme::metrics::TRAFFIC_LIGHTS_Y),
                    )),
                }),
                app_owns_titlebar_drag: true,
                window_min_size: Some(size(px(900.), px(560.))),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_background: WindowBackgroundAppearance::Blurred,
                ..Default::default()
            };
            let state_for_window = state.clone();
            let cert_service_for_store = certificate_service.clone();
            let handle_for_root = runtime_handle.clone();
            let main_window = cx
                .open_window(options, move |window, cx| {
                    ui::theme::apply::follow_system(window);
                    // Сущности, требующие &mut Window (таблица, фильтр, экран
                    // сертификатов), собираются здесь.
                    let table = build_traffic_table(&traffic_store, window, cx);
                    let filter_input = build_filter_input(window, cx);
                    bind_filter_input(&filter_input, &traffic_store, cx);
                    // Поиск по заголовкам/телам (Phase 5): поле + store.
                    let search_input = build_search_input(window, cx);
                    let search = cx.new(|_| {
                        SearchStore::new(
                            &state_for_window,
                            handle_for_root.clone(),
                            traffic_store.clone(),
                        )
                    });
                    bind_search_input(&search_input, &search, cx);
                    // Панель деталей (Phase 5): следит за выбором строки таблицы.
                    let detail = cx.new(|cx| {
                        ui::detail::DetailStore::new(
                            state_for_window.clone(),
                            handle_for_root.clone(),
                            &traffic_store,
                            cx,
                        )
                    });
                    // Системный прокси macOS: тоггл «macOS Proxy» + первичный
                    // статус (networksetup).
                    let sys_proxy = cx.new(|_| {
                        ui::system_proxy::SystemProxyStore::new(
                            &state_for_window,
                            handle_for_root.clone(),
                        )
                    });
                    ui::system_proxy::SystemProxyStore::refresh(&sys_proxy, cx);
                    ui::system_proxy::SystemProxyStore::start_polling(&sys_proxy, cx);
                    // SSL Proxying (MITM): тоггл + стартовое состояние из rules.json.
                    let ssl_proxying = cx.new(|_| {
                        ui::ssl_proxying::SslProxyingStore::new(
                            state_for_window.rule_repository.clone(),
                            state_for_window.engine_command.clone(),
                            handle_for_root.clone(),
                        )
                    });
                    ui::ssl_proxying::SslProxyingStore::refresh(&ssl_proxying, cx);
                    let rules_store = cx.new(|_| {
                        ui::rules::store::RulesStore::new(
                            &state_for_window,
                            handle_for_root.clone(),
                        )
                    });
                    // Правки SSL-списка на экране правил → кнопка SSL на тулбаре.
                    {
                        let ssl_proxying = ssl_proxying.clone();
                        cx.observe(&rules_store, move |_, cx| {
                            ui::ssl_proxying::SslProxyingStore::refresh(&ssl_proxying, cx);
                        })
                        .detach();
                    }
                    // Иконка в строке меню macOS (как у Charles).
                    tray::install(
                        tray::TrayDeps {
                            state: state_for_window.clone(),
                            runtime: handle_for_root.clone(),
                            window: window.window_handle(),
                            traffic: traffic_store.clone(),
                            ssl_proxying: ssl_proxying.clone(),
                            system_proxy: sys_proxy.clone(),
                            rules: rules_store.clone(),
                        },
                        cx,
                    );
                    // Окна Tools (правила), Script Editor и Settings открываются
                    // отдельно.
                    let script_tools = ui::scripting::ScriptTools {
                        test: Arc::new(application::use_cases::scripting::TestScriptUseCase::new(
                            application::use_cases::scripting::TestScriptDeps {
                                exchanges: state_for_window.exchange_repository.clone(),
                                bodies: state_for_window.body_repository.clone(),
                                decoder: state_for_window.body_decoder.clone(),
                                runtime: state_for_window.script_runtime.clone(),
                            },
                        )),
                        script_runtime: state_for_window.script_runtime.clone(),
                        runtime: handle_for_root.clone(),
                        console_width: Arc::new(std::sync::atomic::AtomicU32::new(
                            state_for_window.config.ui.script_console_width.to_bits(),
                        )),
                    };
                    ui::scripting::window::install(
                        rules_store.clone(),
                        script_console.clone(),
                        script_tools,
                        cx,
                    );
                    ui::compose::install(
                        ui::compose::ComposeDeps {
                            repeat: Arc::new(
                                application::use_cases::compose::RepeatExchangeUseCase::new(
                                    state_for_window.exchange_repository.clone(),
                                    state_for_window.body_repository.clone(),
                                    state_for_window.direct_http.clone(),
                                ),
                            ),
                            send: Arc::new(
                                application::use_cases::compose::SendComposedRequestUseCase::new(
                                    state_for_window.direct_http.clone(),
                                    state_for_window.body_decoder.clone(),
                                ),
                            ),
                            runtime: handle_for_root.clone(),
                        },
                        cx,
                    );
                    ui::rules::save_mock::install(
                        ui::rules::save_mock::SaveMockDeps {
                            use_case: Arc::new(
                                application::use_cases::rules::save_response_mock::SaveResponseMockUseCase::new(
                                    state_for_window.exchange_repository.clone(),
                                    state_for_window.body_repository.clone(),
                                    state_for_window.body_decoder.clone(),
                                    Arc::new(infrastructure::repositories::DirectoryMockFileStore::new(
                                        config::AppConfig::app_dir().join(MAP_LOCAL_MOCKS_DIR),
                                    )),
                                ),
                            ),
                            runtime: handle_for_root.clone(),
                        },
                        cx,
                    );
                    ui::rules::window::install(rules_store, cx);
                    ui::settings::install(
                        ui::settings::SettingsDeps {
                            runtime: handle_for_root.clone(),
                            cert_service: cert_service_for_store.clone(),
                            state: state_for_window.clone(),
                            status: status_store.clone(),
                            sys_proxy: sys_proxy.clone(),
                        },
                        cx,
                    );
                    let (ssl_proxying_for_menu, sys_proxy_for_menu) =
                        (ssl_proxying.clone(), sys_proxy.clone());
                    let sidebar = cx.new(|_| {
                        ui::sidebar::SidebarStore::new(traffic_store.clone(), sidebar_visible)
                    });
                    ui::sidebar::SidebarStore::install(&sidebar, cx);
                    let sidebar_for_menu = sidebar.clone();
                    let detail_for_menu = detail.clone();
                    let view = cx.new(|_| {
                        CrabtrapRoot::new(
                            state_for_window.clone(),
                            status_store.clone(),
                            traffic_store.clone(),
                            table,
                            filter_input,
                            search_input,
                            search,
                            handle_for_root.clone(),
                            detail,
                            sys_proxy,
                            ssl_proxying,
                            sidebar,
                        )
                    });
                    ui::app_menu::install(
                        ui::app_menu::AppMenuDeps {
                            state: state_for_window.clone(),
                            runtime: handle_for_root.clone(),
                            status: status_store.clone(),
                            traffic: traffic_store.clone(),
                            ssl_proxying: ssl_proxying_for_menu,
                            system_proxy: sys_proxy_for_menu,
                            sidebar: sidebar_for_menu,
                            detail: detail_for_menu,
                        },
                        cx,
                    );
                    ui::updates::check(
                        &state_for_window,
                        &handle_for_root,
                        ui::updates::Trigger::Startup,
                        cx,
                    );
                    // Root красит `theme.background` под всем окном — гасим, иначе
                    // размытие окна (Blurred) не просвечивает.
                    cx.new(|cx| {
                        gpui_kit::Styled::bg(
                            Root::new(gpui_kit::AnyView::from(view), window, cx),
                            gpui_kit::transparent_black(),
                        )
                    })
                })
                .expect("failed to open crabtrap window");
            cx.activate(true);
            // Выход — по закрытию ГЛАВНОГО окна; окно Tools закрывается само
            // по себе (иконка в строке меню и Cmd+Q работают как раньше).
            let main_id = main_window.window_id();
            cx.on_window_closed(move |cx: &mut App, id| {
                if id == main_id {
                    cx.quit();
                }
            })
            .detach();

            // Движок уже слушает с момента EngineRuntime::start (супервизор):
            // отдельный Start не нужен. Статус приходит событием EngineStatus.
            let _ = (&state.engine_command, &listen_addr, max_concurrent_conns);
        });
}
