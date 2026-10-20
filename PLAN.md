# crabtrap — план реализации

> Charles-подобный прокси-отладчик на Rust + GPUI для macOS.
> Этот файл — рабочий трекер: отмечай `- [x]` по мере завершения пунктов.
> Правило готовности фазы: все подпункты `[x]` + пройден блок «Проверка».

## Статус фаз

| Фаза | Название | Статус | Дата завершения |
|---|---|---|---|
| 0 | Scaffold + GPUI-окно | ✅ | 2026-10-02 |
| 1 | Домен + порты + in-memory адаптеры | ✅ | 2026-10-01 |
| 2 | HTTP-прокси + CONNECT-туннель | ✅ | 2026-10-02 |
| 3 | MITM + CA + раздача сертификата устройствам | ✅ | 2026-10-02 |
| 4 | Capture-конвейер + таблица трафика | ✅ | 2026-10-02 |
| 5 | Инспекторы + фильтры (MVP готов) | ✅ | 2026-10-03 |
| 6 | Throttle + block + map local/remote + правила | ✅ | 2026-10-16 |
| G | Редизайн UI/UX: Liquid Glass (как Proxyman) | ✅ | 2026-10-17 |
| S | Scripting (JS-скрипты, как в Proxyman) | 🚧 | — |
| 7 | Breakpoints + rewrite + repeat/compose | ✅ | 2026-10-18 |
| 8 | HAR export + сессии + mirror | ⬜ | — |
| 9 | Внешний прокси/PAC + SOCKS + client certs | ⬜ | — |
| 10 | Reverse proxy + port forwarding + DNS spoof | ⬜ | — |
| 11 | HTTP/2 + WebSockets | ✅ | 2026-10-18 |
| 12 | Упаковка и полировка | 🚧 (нет Developer ID) | — |
| P | Паритет с Proxyman (Map Local, матчинг, экспорт, Allow, DNS) | 🚧 | — |

Оценки: MVP (0–5) ≈ 15–19 дней; полный паритет (0–12) ≈ 40–45 дней.

---

## Context

Своё десктоп-приложение для отладки HTTP/HTTPS трафика (аналог Charles Proxy):
- Стек полностью Rust, UI — GPUI (gpui-kit).
- Архитектура — clean/hexagonal (см. `ARCHITECTURE.md`).
- macOS на первом этапе (Apple Silicon, Rust 1.98.1 stable).
- HTTP + HTTPS MITM в MVP сразу.
- Финальный охват — все основные фичи Charles, поэтапно.

## Ключевые технические решения

| # | Решение | Обоснование |
|---|---|---|
| D1 | Движок прокси: **свой, на hyper 1.x + hyper-util + tokio** | Фичи Charles (raw-байты, брейкпоинты с паузой, throttle, map local/remote, repeat) требуют владения конвейером байт. Готовые crates не подходят (hyper-proxy — клиентский, rpxy — не отладчик) |
| D2 | Сертификаты: **rcgen 0.14 + rustls 0.23 (провайдер `ring` везде)** | Без OpenSSL на macOS, контроль ALPN. CA в `~/Library/Application Support/crabtrap/certs/` (chmod 600, переиспользуется), листья — LRU по (host, alpn) через `rustls::server::ResolvesServerCert`. Доверие на Mac: `security add-trusted-cert` через `osascript ... with administrator privileges` |
| D3 | GPUI: **`gpui-kit = "0.6"`** (зонт-крейт, включает gpui-pre 0.3.x + компоненты) | Виртуализированные `Table`/`List`, `Input`, `Dock`, `TabBar`. Пиновать версию (0.x churn). API сверять по `examples/` в репо gpui-kit |
| D4 | Мост async: **tokio (multi-thread) на выделенных OS-потоках; обмен только через `async-channel`** | `tokio::sync::mpsc` вне tokio не работает, gpui — smol-based. `async_channel` корректно ожидается обоими. UI → `Sender<EngineCommand>`, engine → `Receiver<ProxyEvent>`; в gpui `cx.spawn` цикл `rx.recv().await` → `cx.update` → `cx.notify` |
| D5 | Тела: **memory ≤1 MiB → spill на диск** (`~/Library/Caches/crabtrap/bodies/`), ring buffer обменов (1000), кап захвата 50 MiB (поток продолжает проксироваться) | Память под контролем при больших загрузках |
| D6 | Персистентность сессий: **каталог-формат** (`session.json` + `bodies/`) за портом `SessionRepository` | Читается глазами, без C-зависимостей; swap на sqlite позже не сломает UI |
| D7 | Парсинг: **tee-слой `RecordingStream` ПОД hyper** (wire-форма: chunked как chunked), семантика — hyper. HTTP/1.1 сразу; h2/WS — поздние фазы (до того ALPN только `http/1.1`). Pretty-view — декодированный (gzip/br через `async-compression`) |
| D8 | Перехват: **единый async-порт `InterceptionPipeline`** — движок await-ит `on_request/on_response → InterceptOutcome { Proceed { message, route }, Abort }`, `route = Origin \| MapLocal \| MapRemote \| Blocked`. Breakpoint = парковка на `oneshot` в `BreakpointRegistry`; throttle = token-bucket обёртка тела + latency sleep | Чистая проекция UI-действий на use cases |

## Персистентность (что и где хранится)

1. **`config.toml`** (app-support dir): порт, capture-параметры, каталоги, UI. Загрузка при старте, атомарная запись (tmp+rename) при изменении из настроек.
2. **`rules.json`** (app-support dir): ВСЕ правила — Map Local/Remote, Rewrite, Block, Breakpoints, Throttle, SSL Proxying-список; позже DNS overrides, port forwarding, reverse proxy, внешний прокси, клиентские сертификаты (только ссылки; пароли — в macOS Keychain через security-framework, не в файле). JSON (не TOML — regex/glob/пути с бэкслешами), у правила стабильный UUID. Запись на каждое изменение в редакторе (CRUD → `UpdateRuleSetUseCase` → save + version++ → `EngineCommand::UpdateRules` → hot-reload без разрыва соединений). Битый файл → `rules.json.corrupt-<ts>`, старт с пустого набора + предупреждение.
3. **Захваченный трафик**: только RAM (ring buffer + spill-кэш), рестарт не переживает; сохранить = явный Save Session (фаза 8).

## Workspace (слои clean/hexagonal + `engine`)

```
crabtrap/
├── Cargo.toml                # workspace, resolver 3, edition 2024, [workspace.dependencies]
├── rust-toolchain.toml       # stable
├── PLAN.md                   # этот файл
└── crates/
    ├── domain/               # 0 IO: entities/ value_objects/ repositories/(trait'ы) errors.rs
    │   ├── entities/         # exchange.rs (HttpExchange, HttpRequest/Response, ExchangeStatus),
    │   │                     # body.rs (BodyRef), rule.rs (RuleSet: Rewrite/MapLocal/MapRemote/
    │   │                     #   Block/Breakpoint/Throttle/SslProxyEntry), network.rs, session.rs, certificate.rs
    │   ├── value_objects/    # ids.rs, host.rs, http_vo.rs, timing.rs (TimingPhases), tls.rs (TlsInfo),
    │   │                     #   match_pattern.rs (TrafficMatch), filter_query.rs
    │   └── repositories/     # ExchangeRepository, BodyRepository, RuleRepository (RuleSet снапшот + version),
    │                         # SslProxyRepository, SessionRepository, CertificateRepository, ...
    ├── application/          # use_cases/ dto/ services/ (ПОРТЫ); БЕЗ tokio — runtime-agnostic
    │   ├── dto/              # capture.rs (ExchangeSummaryDto, ProxyEvent), proxy_config.rs (EngineCommand),
    │   │                     #   inspection.rs, rules.rs, certificates.rs, sessions.rs, compose.rs
    │   ├── services/         # ProxyEngineService, InterceptionPipeline(!), EventPublisher,
    │   │                     #   CertificateService, SystemProxyService, DirectHttpService, RevealService
    │   └── use_cases/        # capture/ inspection/ proxy_config/ rules/ certificates/ sessions/ compose/
    ├── engine/               # адаптер: MITM-прокси, чистый tokio, БЕЗ gpui (+bin crabtrapd headless)
    │   └── src/              # server.rs, http_proxy.rs, connect.rs (CONNECT→Mitm|Passthrough), mitm.rs
    │                         #   (CertIssuer), client.rs (UpstreamClient), capture.rs (RecordingStream tee,
    │                         #   ThrottleBody), intercept.rs, throttle.rs, commands.rs
    ├── infrastructure/       # реализации портов/репозиториев: exchange_store (ring buffer), body_store
    │   └── src/              #   (memory+spill), rule_store (rules.json), channel_publisher,
    │                         #   interception_pipeline (+BreakpointRegistry), certificate_service,
    │                         #   system_proxy_service (networksetup), ...
    ├── config/               # AppConfig: TOML, load_or_init()/save()
    └── app/                  # композиционный корень + GPUI; bin crabtrap
        └── src/              # main.rs, app_state.rs (AppState: Arc<dyn Port> + config), runtime.rs
                            #   (tokio-потоки + каналы + pump событий), ui/ (traffic/, inspector/, rules/, ...)
```

Направления зависимостей: `app → application → domain`, `app → engine → application → domain`, `app → infrastructure → application → domain`, `config → (app, infrastructure, engine)`. Domain — только serde/chrono/thiserror/async-trait/uuid.

Ключевые контракты:
- **`ProxyEvent`** (engine→UI): ExchangeStarted / RequestBodyDone / ResponseHeaders / BodyProgress / ExchangeCompleted / ExchangeFailed / ConnectionOpened/Closed / InterceptWaiting/Resolved / EngineStatus (+WS-события в фазе 11).
- **`EngineCommand`** (UI→engine): Start/Stop, UpdateRules{snapshot}, UpdateThrottle, ResolveIntercept{id, Resume(edited)|Abort}, SetExternalProxy, SetDnsOverrides.
- **Use case-паттерн**: `struct XUseCase { deps: Arc<dyn T> }`, `new(...)`, `pub async fn execute(dto) -> Result<Out, DomainError>`.

---

## Фазы

### Phase 0 — Scaffold + GPUI-окно (~0.5 дня) ✅ (2026-10-02)

- [x] Workspace: корневой `Cargo.toml` (resolver 3, edition 2024, каталог `[workspace.dependencies]`, `[profile.dev.package."*"] opt-level = 3`)
- [x] `rust-toolchain.toml` (stable), `.gitignore`
- [x] Крейт `config`: `AppConfig` (Proxy/Capture/Storage/Ui), TOML, `load_or_init()`/`save()` атомарно, тест roundtrip
- [x] Крейты `domain`, `application`, `infrastructure` — реализованы по-настоящему в Phase 1 (не заглушки)
- [x] Крейты-заглушки: `engine` (+bin `crabtrapd`), `app` (bin `crabtrap`) — наполнены в Phase 2
- [x] `app/main.rs`: окно GPUI «crabtrap» (центр, 1400×900 из конфига) + статус-бар «Proxy: stopped»
- [x] `git init` + первый коммит

**Проверка**: `cargo build` зелёный; `cargo run -p app --bin crabtrap` открывает окно (проверка совместимости gpui-kit с macOS 14.5 — если нет, даунгрейд версии gpui-kit); `~/Library/Application Support/crabtrap/config.toml` создался.

### Phase 1 — Домен + порты + in-memory адаптеры (~1–2 дня) ✅ (2026-10-01)

- [x] `domain`: entities (exchange, body, rule, network, session, certificate), value_objects (ids, host, http_vo, timing, tls, match_pattern, filter_query), repositories (trait'ы `#[async_trait]` → `Result<T, DomainError>`), `errors.rs` (DomainError: NotFound/InvalidEntity/RepositoryError/ValidationError/DuplicateEntity/Conflict/Timeout/InfrastructureError)
- [x] `application`: dto (capture/inspection/proxy_config/rules/certificates/sessions/compose), порты (ProxyEngineService, InterceptionPipeline, EventPublisher, CertificateService, SystemProxyService, DirectHttpService, RevealService), `ProxyEvent`/`EngineCommand`; use cases: RecordExchangeEvent, GetExchangeList, GetExchangeDetail, ClearTraffic, Get/UpdateRuleSet (валидация + version++)
- [x] `infrastructure`: InMemoryExchangeRepository (ring buffer), MemorySpillBodyStore (spill+cap), FileRuleRepository (rules.json, атомарно, corrupt-политика), ChannelEventPublisher (async-channel)
- [x] Юнит-тесты domain (инварианты VO, фильтрация) + application (use cases на тестовых репо) + infrastructure (сторам, tempdir)

**Проверка**: `cargo test` зелёный (57 тестов); `cargo tree -p domain` — без IO-крейтов; `cargo tree -p application` — без tokio; `cargo clippy --workspace --all-targets` — 0 warnings; `cargo fmt --check` — чисто.

### Phase 2 — Прозрачный HTTP-прокси + CONNECT-туннель, без MITM (~2–3 дня) ✅ (2026-10-02)

- [x] `engine/server.rs`: accept-loop, пер-коннект задачи, лимит коннектов
- [x] `engine/http_proxy.rs`: absolute-form → origin-form, фильтр hop-by-hop заголовков
- [x] `engine/connect.rs`: CONNECT → dial-first → 502/504 при недоступном origin → 200 → `upgrade::on().await` → ветка Passthrough (копирование байт в обе стороны)
- [x] `engine/client.rs`: UpstreamClient (hyper-util pool, исходящий TLS через rustls + rustls-native-certs, ALPN http/1.1)
- [x] События ExchangeStarted/Completed (пока заголовки), ConnectionOpened/Closed, EngineStatus
- [x] `app/runtime.rs`: tokio-потоки + async-channel, pump событий в gpui (батчами по 64), статус листенера в статус-баре
- [x] Бинарь `crabtrapd` (headless, аргы --listen/--json, env CRABTRAP_LISTEN, Ctrl-C → Stop)
- [x] Integration-тест `engine/tests/` (движок + локальные origin-серверы, raw-wire контроль)

**Проверка**: `cargo run -p engine --bin crabtrapd` → `curl -x http://127.0.0.1:8080 http://example.com -v` (200, exchange в событиях); `curl -x http://127.0.0.1:8080 https://example.com -v` (туннель, валидный сертификат оригинала); keep-alive на одном соединении; большие тела стримятся без буферизации.
**Грабли**: таймауты только на connect; hop-by-hop (`Proxy-Connection`, `Keep-Alive`) фильтровать в обе стороны.

### Phase 3 — MITM + CA + раздача сертификата устройствам (~3–4 дня) ✅ (2026-10-02)

- [x] `engine/mitm.rs`: CertIssuer — LRU `CertifiedKey` по (host, ca_fingerprint); **весь rcgen — в infrastructure** (`RcgenCertificateService`): движок получает PEM-DTO через порт `CertificateService` и парсит их rustls-pemfile'ом (решение Phase 3 — engine не зависит от rcgen); SAN: голый домен + wildcard (+punycode)
- [x] `engine/connect.rs`: ветка Mitm по ssl-списку (пустой список = Passthrough, Charles-семантика), TLS-терминация (tokio-rustls, ALPN только `http/1.1`) → hyper http1 на расшифрованном стриме; dial-first сохранён (probe-соединение закрывается, трафик идёт через пул UpstreamClient)
- [x] `infrastructure/certificate_service.rs`: ensure_ca (persist/reuse, corrupt-политика, 0700/0600) / trust_status (`security verify-cert -p ssl`) / install_trust (osascript admin + `security add-trusted-cert -p ssl`) / regenerate_ca (+`FileCertificateRepository` для ca-meta.json)
- [x] Раздача CA устройствам: листенер отвечает на **прямые origin-form запросы** (`GET /ssl` — HTML-инструкция, `GET /ca.pem` — PEM); работает оффлайн и ДО настройки прокси (бутстрап)
- [x] UI-экран сертификатов (временная кнопка «Certificates» в тулбаре — навигация Phase 4): статус CA, fingerprint, Install Trust (GUI-промпт), Regenerate CA с инлайн-подтверждением
- [x] `application/certificates` use cases (GetCaStatus / InstallTrust / RegenerateCa)
- [x] crabtrapd: `--mitm <glob>` (повторяемый) + env `CRABTRAP_MITM`, fingerprint CA в stdout при старте
- [x] Тайминги: connect_ms (диалап исходящего, Connected-метрики коннектора) + tls_ms (MITM-хендшейк клиент↔crabtrap; upstream-хендшейк для обычных https); dns_ms — 0 до Phase 10

**Проверка**: `curl -x http://127.0.0.1:8080 --cacert .../ca.pem https://example.com -v` — расшифрованный exchange; wildcard-SAN лист валиден для поддоменов; перезапуск НЕ регенерирует CA; после Trust Safari/Chrome без предупреждений; хосты вне ssl-листа — туннелем; `curl http://127.0.0.1:8080/ssl` (без -x) отдаёт PEM.
**Устройства**: iOS — профиль → **Certificate Trust Settings → тумблер**; Android — Install CA certificate; Android 7+: митмится только браузер и app'ы с opt-in networkSecurityConfig (задокументировать).
**Грабли**: crypto provider `ring` везде (rcgen+rustls); pinning-приложения — только passthrough.

### Phase 4 — Capture-конвейер + таблица трафика в GPUI — ядро MVP (~3–4 дня) ✅

- [x] Tee-захват тел в `engine/src/body.rs` (`StreamTracker`+`TeeConfig`, НЕ отдельный capture.rs): кадры ДЕКОДИРОВАННОГО тела после http1-кодека hyper (raw wire — D7, Phase 5), cap на буфер (поток продолжается), ровно-один терминальный сброс → `BodyRepository::put` (memory ≤1 MiB → диск, кап 50 MiB); дескриптор из Content-Type/Content-Encoding. События: `RequestBodyDone{body: BodyRef}` / `ExchangeCompleted{body: BodyRef}`
- [x] `RecordExchangeEventUseCase` — врезан в pump приложения (Phase 4): событие → HttpExchange в ExchangeRepository; TLS-параметры (версия/cipher/ALPN с MITM-хендшейка) едут в `ExchangeStarted.tls` и мержатся в сущность
- [x] UI `traffic/`: TrafficStore (Entity; чистая `apply_event` покрыта тестами) + `ExchangeTableDelegate` на gpui-kit `DataTable` (виртуализация), колонки: Host/Path/Method/Status(цвет 2xx-5xx)/SSL(mitm)/Type/Size(req→resp)/Time/Tags; клик → полоса-селектор (полный инспектор — Phase 5)
- [x] Тулбар Start/Stop/Clear/Recording(`EngineCommand::SetBodyCapture` на лету)/Pause + фильтр-Input; статус-бар: порт/соединения/обмены/bytes captured/failed
- [x] Батчинг событий: pump дренирует по 64 события за пробуждение (async-channel)
- [x] Ring buffer обменов (`buffer_len`); тела чистятся Clear'ом (обмены+тела вместе) — авто-GC тел при вытеснении сделан в Phase 5 (`drain_evicted_bodies` + `gc_evicted_bodies`)
- [x] Экран сертификатов: QR-код `http://<lan-ip>:<port>/ssl` (LAN-IP пересчитывается при каждом открытии — DHCP плавает; qrcode→div-грид), URL selectable-полем + Copy

Сверх плана: `crabtrapd --cap-bytes N / --no-bodies`, `[xchg]`-строки печатают stored-байты и TLS-версию; тумблер захвата тел без рестарта движка.

**Проверка**: системный прокси (`networksetup -setwebproxy Wi-Fi 127.0.0.1 8080` + `-setsecurewebproxy`) → браузер → таблица наполняется, HTTPS расшифрован; скачивание 500 MB не съедает память; 10k строк не лагают (виртуализация); телефон по QR качает сертификат.
**Грабли**: тела в UI не клонировать — только BodyRef; события только через async-channel (никакого tokio-IO в gpui-тасках).

### Phase 5 — Инспекторы + фильтры — MVP готов (~4–5 дней) ✅ 2026-10-03

- [x] UI `inspector/`: Headers (request/response), Raw (re-serialized — wire-фрейминг не сохраняется tee после кодека, помечено в UI), Hex (offset-страницы), JSON дерево (виртуализированное), Image preview (downscale), Cookies, Timing chart по фазам
- [x] Декомпрессия gzip/br/deflate (flate2+brotli, BodyDecoderService в infrastructure)
- [x] Фильтр таблицы: `parse_filter_query` (application) → `TrafficStore::set_filter` — слова (AND), `host:`, `status:`, `method:`, `type:`, `re:`; поиск по заголовкам и телам (подключено к UI в hotfix 2026-10-16)
- [x] Charset-детект (encoding_rs, windows-1251/UTF-16/BOM)

**Проверка**: JSON рендерится деревом; brotli показывается decoded + raw; hex с offset; PNG картинкой; timing сходится с `curl -w`; фильтры сужают таблицу; поиск находит по телу.
**Грабли**: поиск лениво (по загруженным телам); большие image — downscale.

### Hotfix 2026-10-03 — после MVP (по фидбеку)

- [x] Починена кнопка Start: `Stop` убивал поток движка и канал — команды уходили в никуда. Теперь app-супервизор (`runtime.rs`): один живой tokio-рантайм, `engine::run` — перезапускаемая задача; `EngineHandle::send(Start)` поднимает движок заново (свежий канал + перепуш правил/throttle/ssl-списка/захвата тел из rules.json); ошибка bind видна как `EngineStatus{error}`. Тест `engine_restarts_after_stop_and_start`
- [x] Кнопка «macOS Proxy» в тулбаре: тоггл системного HTTP/HTTPS-прокси через `networksetup` (`NetworksetupSystemProxyService` + Set/Unset/GetStatus use cases, bypass по умолчанию 127.0.0.1/localhost/*.local/169.254.0.0/16); статус при старте, ошибки CLI — на кнопке

### Hotfix 2026-10-16 — аудит «записано vs работает»

Аудит фаз 0–5 (код + живой прогон через crabtrapd/curl) нашёл расхождения; исправлено:

- [x] Выход: очистка в `cx.on_app_quit` (`app/src/shutdown.rs`) — код после `run()` на macOS недостижим, системный прокси не снимался; + Cmd+Q, Proxy › Exit, выход при закрытии окна
- [x] CA: `init_lock` против двух CA при первом запуске; IO-ошибка ≠ порча (не перевыпускаем); ключ сразу 0600; UI и движок делят один `RcgenCertificateService` (Regenerate CA доходил только до UI)
- [x] Таблица ограничена `buffer_len`; `capture.record_bodies` из конфига учитывается
- [x] Фильтры подключены к UI (раньше — только подстрока host+path); `type:` по Content-Type ответа, `re:` по полному URL
- [x] Поиск по телам снимает Content-Encoding и charset (charset-хелперы переехали в application)
- [x] CONNECT-туннели, блоки и неудачные CONNECT видны в трафике (метод CONNECT, тег `tunnel`, байты/время); причина ошибки upstream — полная цепочка (refused/DNS) вместо «request body Dropped»; гард обмена при уходе клиента; счётчик байт туннеля больше не удваивается
- [x] SSL Proxying: глобальный флаг `ssl_proxy_enabled` (выключение не стирает список), шаблоны без учёта регистра, SAN листа — только точный хост
- [x] Туннели занимают слот лимита соединений (permit в `ConnGuard`); JoinSet'ы чистятся; ConnectionClosed для туннеля — по его закрытию
- [x] Битый `config.toml` откладывается в `.corrupt-<ts>`, нечитаемый не перезаписывается
- [ ] Ручная проверка: выход при включённом macOS Proxy снимает системный прокси (не автоматизировано — меняет сетевые настройки машины)

Известные ограничения (не исправлялись): crabtrapd берёт certs_dir не из конфига; `ui.theme` игнорируется (XP-тема фиксирована); тайминги pooled-соединений исторические; IPv6-хосты отклоняются доменным `Host`.

### Phase 6 — Throttle + block + map local/remote + правила ✅ (2026-10-16)

- [x] Домен `entities/rule_routing.rs`: Block > Map Local > Map Remote (первое включённое совпадение), CONNECT-блок только для host-only правил, throttle по match; флаг `enabled` у правил (старый rules.json → включено)
- [x] `infrastructure/services/rules_pipeline.rs`: `RulesInterceptionPipeline` — Map Local (файл / каталог + path, без `..`, `/` → index.html), Map Remote (origin/path/query из target, `preserve_host`), Block (код | Close → обрыв соединения); горячая перезагрузка через `InterceptionPipeline::update_rules` (UpdateRules)
- [x] `engine/forward.rs`: маршруты применяются; локальные ответы идут тем же relay, что и origin (события, захват тел, тайминги); тело запроса всё равно захватывается; событие `ExchangeTagged` (`blocked` / `map-local` / `map-remote` / `throttled`)
- [x] `engine/throttle.rs`: `ThrottledBody` (обёртка тела, кванты по 50 мс), latency перед запросом, up/down раздельно, scope по location
- [x] UI: меню Tools → SSL Proxying / Block List / Map Local / Map Remote / Throttle (XP-вкладки, списки с галочкой и Remove, location в стиле Charles, пресеты throttle); правки → валидация → rules.json → движок без разрыва соединений
- [x] Рефакторинг: `http_proxy.rs` 866 → 342 строки (`direct_hit.rs`, `exchange_timing.rs`, `forward.rs`)

### UX-итерация Tools (2026-10-17) ✅

- [x] Окно «crabtrap — Tools» отдельно от трафика (меню Tools, иконка в строке меню, правый клик); выход — только по главному окну
- [x] Вкладки по паттерну Charles: «Enable <инструмент>» (новые флаги `blocks/map_local/map_remote/throttle_enabled`), таблица с Checkbox, выделением и двойным кликом, Add…/Edit…/Duplicate/Remove, Delete/Backspace
- [x] Диалог Add/Edit Location: Host/Path/Method, ошибки под полями, предпросмотр «Matches», разбор вставленного URL, Map Local «Choose…», цель Map Remote по полям, Block 403/Drop
- [x] Throttle: свои значения kbps/мс, пресеты заполняют поля, область «Limit to location…»
- [x] Правый клик по запросу: Map Local… / Map Remote… / Block… с подставленными полями, «Enable SSL Proxying for host», Copy URL
- [x] Map Local мокает только ответ (запрос — дело Rewrite/Breakpoints): поля Status (200–599, по умолчанию 200) и Content-Type (пусто — по расширению); старые rules.json → 200/auto; нет файла — по-прежнему 404
- [ ] Map Local: файл с сырым HTTP-ответом (`HTTP/1.1 …` + заголовки) и «Save response as mock…» из трафика

### Track G — Редизайн UI/UX: Liquid Glass по мотивам Proxyman (~9–11 дней) ✅

Решения (2026-10-17): светлая+тёмная тема вслед за системой; XP-тема удаляется полностью;
инспектор снизу, request | response рядом; раздел Apps — после основного редизайна (G8).
Ограничение GPUI: blur только ЗА окном (`WindowBackgroundAppearance::Blurred` →
NSVisualEffectView), backdrop-blur внутри окна нет → «стекло» = полупрозрачные слои поверх
размытого окна + светлая кромка + тень; поповеры/диалоги почти непрозрачные.

- [x] G0 Spike: Blurred-окно + прозрачный титлбар с тулбаром (drag/zoom), материал blur
      (захардкожен `Selection` в gpui-pre-macos), нативное меню macOS, плавность 10k строк
      - blur не просвечивал: `Root` gpui-component красит `theme.background` под всем окном →
        в `main` фон Root прозрачный (`Styled::bg`, refine_style идёт после его `.bg`)
      - материал: gpui сам снимает с NSVisualEffectView тонировку и насыщенность → размытие
        нейтральное, форк не нужен; оттенок даём своими слоями (`ui/theme`)
      - титлбар: `TitleBar` gpui-component + `app_owns_titlebar_drag`, светофоры по центру
        52px; кнопки внутри кликаются, drag двигает окно, двойной клик — zoom/restore
      - меню: `cx.set_menus` (`ui/app_menu.rs`), пересборка только при смене галочек;
        XP-менюбар в окне удалён
      - 10k строк (release): прокрутка плавная, ~30% CPU при быстром скролле
- [x] G1 Дизайн-система `ui/theme/`: токены поверхностей/радиусов/отступов/шрифтов,
      семантические цвета методов/статусов/тегов, light+dark; GlassPanel, Pill, SegmentedControl,
      ToolbarButton, StatusPill; тесты цветов и контраста ≥4.5:1
      - `palette` (слои window → sidebar/content → header/stripe, raised для поповеров), `semantic`
        (метод/статус/тег/ContentKind → тон), `metrics`, `apply` (палитра → ThemeColor поверх
        `Theme::change(mode)`, следование за системой), `components/` (+ FilterChip)
      - контраст текста ≥4.5:1 проверяется на стопке слоёв поверх белого/серого (светлая) и
        чёрного/серого (тёмная) рабочего стола; `CRABTRAP_THEME=light|dark` — принудительная тема
- [x] G2 Каркас окна: тулбар в заголовке (пауза/запись/очистка, плашка состояния, Tools,
      раскладка), меню Proxy/View/Help → строка меню macOS, нижняя панель вместо таскбара
      - `ui/toolbar.rs`: запись (красная точка), пауза, очистка · StatusPill (прокси — клик
        старт/стоп, SSL Proxying и macOS Proxy — тумблеры с подсказками про Stale/Other) ·
        Tools ▾ · поиск; дополнительные Lucide-иконки — `theme/icons.rs` (`icon_assets!`)
      - таблица и инспектор — две GlassPanel в resizable-сплите, фильтр — полоса над таблицей
      - нижняя панель: состояние прокси, соединения, «N of M exchanges», объём, сбои
      - переключатель раскладки инспектора — в G5
- [x] G3 Боковая панель: Pinned, All, Domains (хост → пути, счётчики), Pin/Unpin
      - `ui/sidebar/`: Library (All, Pinned) → закреплённые домены → остальные; домен
        раскрывается в разделы (первый сегмент пути); виртуализированный `uniform_list`
      - выбор = `traffic::scope::Scope` в TrafficStore (сужает таблицу вместе с фильтром и
        поиском); дерево доменов пересобирается только при новых строках
      - Pin/Unpin — правый клик по домену и по строке таблицы; закреплённые и видимость
        панели — в `[ui]` config.toml (`AppConfig::update` перечитывает файл, битый не трогает)
      - тумблер панели: кнопка в тулбаре, View → Show Sidebar (⌃⌘S)
- [x] G4 Таблица + чипы HTTP/HTTPS/WS и JSON/Form/XML/JS/CSS/Img/Doc/Other; точки состояния,
      плашки метода/статуса, бейджи тегов
      - колонки: точка · Method · Status (плашки) · Host (замок у HTTPS) · Path · Type · Size ·
        Time · Tags (плашки); на выделенной строке текст и плашки белые
      - `traffic/quick_filter.rs`: протокол — один из, типы содержимого — несколько; полоса
        фильтра показывает выбор сайдбара чипом с ✕
      - фикс: подсветка выбранной строки рисуется делегатом по id обмена (TableState держал
        индекс и после фильтра/вытеснения подсвечивал чужую строку); индекс TableState
        переносится только при смене вида (`view_generation`), не на каждый батч
- [x] G5 Инспектор: строка метод/статус/URL, request | response рядом с вкладками,
      раскладка снизу/справа, перетаскиваемые разделители
      - строка обмена: плашки метода/статуса, URL, длительность · размер, Copy URL
      - Request: Overview · Headers · Query (новая) · Body · Cookies · Raw · Hex;
        Response: Headers · Body · Cookies · Raw · Hex — переключатели SegmentedControl
      - вкладки рисуют одну сторону (`inspector::Side` в ExchangeContext); внутренние
        переключатели Request/Response у Body и Hex убраны
      - раскладка (под таблицей: стороны рядом; справа: друг под другом) — кнопка тулбара,
        View-меню, `[ui] inspector_layout` в config.toml
      - фикс: состояния вкладок Body/Raw/Hex жили на слабых ссылках, которые держали только
        обработчики кликов — без них (загрузка, Raw) состояние пересоздавалось каждый кадр и
        тело вечно «грузилось»; теперь `inspector::tab_state` — сильные ссылки на текущий обмен
      - цвета JSON-подсветки, ошибок и фаз тайминга — из палитры (читаемы в тёмной теме)
- [x] G6 Окно Tools (навигация списком) + Settings (⌘,: Certificates, System Proxy)
      - общий каркас `ui/window_shell.rs`: размытое окно, прозрачный тайтл-бар, список
        разделов (`NavItem` с плашкой «On»/«Not trusted») + GlassPanel содержимого
      - Tools: инструменты списком, у включённых — «On»; таблицы правил — в палитре
      - Settings (crabtrap → Settings… ⌘,, Help → Certificate Setup…): Certificates
        (переехали из главного окна — экран и пункты View → Traffic/Certificates убраны) и
        System Proxy (состояние, тумблер, эффективные/прописанные серверы, bypass)
      - фикс: диалог Add/Edit правила был без кнопок — gpui-component 0.6.6 рисует их только
        из явного `.footer(...)`; добавлен футер Cancel / Add|Save
- [x] G7 Полировка: анимации, пустые состояния, горячие клавиши, удаление XP-кода,
      ручная проверка скриншотами в обеих темах
      - XP-код удалён (`xp.rs`, `xp_tabs.rs`, менюбар, таскбар); предупреждений сборки нет
      - проявление инспектора при смене обмена (one-shot 140 мс, без вечных анимаций —
        постоянная перерисовка окна дорогая); пустые состояния таблицы, инспектора, сайдбара
      - ⌘L — фильтр, ⌘F — поиск, ⌘R запись, ⌘K очистка, ⌃⌘S сайдбар, ⌘, Settings,
        ⌥⌘P/B/L/R/T — инструменты; полоса поиска — в палитре
      - нижняя панель: «V of N» только при фильтре, «M seen» — когда старые вытеснены буфером
      - проверено скриншотами (главное окно, Tools, Settings, диалог) в светлой и тёмной
        теме; 10k строк в release (buffer_len 10000): 14–32% CPU при быстром скролле, 2% в покое
- [x] G9 Буфер 10k: `buffer_len` по умолчанию 10 000; TrafficStore — кольцо с номерами
      (`traffic/rows.rs`), видимые строки и счётчики доменов/приложений инкрементальны
      (работа на пачку событий ∝ пачке, не числу строк); ExchangeRepository — индекс id → seq
      (линейный поиск на каждое событие шёл на UI-потоке); бюджет памяти тел 256 МиБ
      (`body_memory_budget_bytes`, сверх — на диск). Release, 10k строк, ~1400 req/s с
      вытеснением: UI-поток ~50–60% — столько же, сколько при 1000 (раньше рос до насыщения)
      - стрелки/PageUp/PageDown считаются от выбранного обмена (TableState терял индекс после
        вытеснения — ↓ прыгал наверх); кнопки тайтл-бара не запускают перенос окна от дрожания
- [x] G8 (опц.) Apps: процесс-клиент по локальному порту (libproc), иконки, колонка Client
      - порт `ClientProcessService` (application) → `MacClientProcessService` (infrastructure):
        один `lsof -nP -iTCP:<proxy> -sTCP:ESTABLISHED -Fpcn` на пачку новых соединений
        (структур socket_fdinfo в libc нет, ручные union-структуры хрупки), `proc_pidpath` —
        путь, внешний `.app` — имя (помощники Chrome → «Google Chrome»), иконка — NSWorkspace
        → 64×64 PNG
      - `ui/clients.rs`: loopback-соединения копятся 80 мс и уходят пачкой; адрес из сети —
        устройство (имя = IP); колонка Client, секция Apps в сайдбаре, `Scope::App`
      - ограничение: соединение, закрытое до lsof (одиночный запрос без keep-alive), уже
        без процесса — такие обмены без клиента; keep-alive (браузеры) определяются

**Проверка**: каждая фаза собирается, `cargo test --workspace` зелёный, функции (поиск,
фильтры, контекстные меню, Tools, сертификаты, системный прокси) работают как раньше.

### Track S — Scripting: JS-скрипты на запрос/ответ (как Proxyman Scripting Tool) 🚧

Решения: движок JS — **boa_engine** (чистый Rust, без C; пул потоков, `!Send`-контексты);
S1 — общая с Phase 7 основа (движок применяет правки пайплайна); MVP = S1–S4, затем S5.

- [x] **S1. Движок применяет правки пайплайна** (основа Phase 7)
  - [x] порт: `InterceptionPipeline::wants_body`, `InterceptableMessage.body: Option<Vec<u8>>`,
        `InterceptContext.request` (запрос на фазе ответа), `RouteDecision::Respond` (mock)
  - [x] engine: буферизация тела по `wants_body` с капом (сверх капа — поток без правок),
        применение method/uri/headers/body запроса и status/headers/body ответа
        (Content-Length пересчитан, Transfer-Encoding снят), тег `modified`/`mocked`
  - [x] ResponseHeaders публикуется ПОСЛЕ пайплайна ответа (в трафике — изменённый ответ)
  - [x] интеграционные тесты `engine/tests/pipeline_edits.rs`
  - [x] ревью: неизменённые заголовки — байт-в-байт из оригинала; SSE/multipart-replace не
        буферизуются, пауза 5 с / 30 с всего → поток без правок; заменённое незагруженное тело
        дочитывается; Host от пайплайна не перетирается; 1xx/204/304 без длины;
        `ProxyEvent::RequestEdited` → итоговый запрос в инспекторе (хост строки — исходный)
- [x] **S2. Домен**: `ScriptRule` + `RuleSet.scripts`/`scripts_enabled` (`serde(default)`),
      `matching_scripts`, валидация в `UpdateRuleSetUseCase`
- [x] **S3. Рантайм** (+ ревью: тело сравнивается `JSON.stringify` в JS — int64 не портятся;
      Promise без разрешения — ошибка; ограниченная очередь, отменённые задания пропускаются,
      зависший поток заменяется; проверка глубины/размера перед JSON; `bodyBase64` для
      двоичного тела; query пересобирается с нетронутыми параметрами байт-в-байт; правило
      фазы ответа — выбранное на запросе; распаковка на `spawn_blocking`): порт `ScriptRuntime` + DTO; `infrastructure/services/scripting/`
      (пул boa-потоков, биндинги request/response, лимиты/таймаут, `$sharedState`, console →
      `ProxyEvent::ScriptLog`); `ScriptingPipeline` поверх `RulesInterceptionPipeline`
      (Block → onRequest → Map Local/Remote; onResponse)
- [x] **S4. UI**: вкладка Scripting в Tools, окно Script Editor (Matching Rule, галочки
      Request/Response/Mock API, редактор JS, Console, Save & Activate ⌘S), «Script…» в
      контекстном меню трафика
- [x] **S5.** Сниппеты (15, меню Snippets — вставка в курсор), аддоны (`btoa`/`atob`,
      `$crypto.sha256/sha512/randomUUID`), Test — черновик на последнем подходящем обмене
      (итог правок + console в консоль), Reset `$sharedState`
- [ ] Ручная проверка окна Script Editor в приложении (вёрстка, ⌘S, консоль) — не делалась
      агентом: GUI-инстанс пишет в настоящие rules.json/config и снимает системный прокси
- [x] Подсветка JS в редакторе (gpui-kit `tree-sitter-javascript`, C только в UI); консоль — read-only
      редактор с подсветкой JSON: выделение, ⌘C, Copy all; граница редактор/консоль перетаскивается,
      ширина сохраняется в `[ui].script_console_width`

**Проверка**: скрипт меняет заголовок/статус/JSON-тело ответа (curl видит правки, в трафике
тег), Mock API отвечает без origin, бесконечный цикл не вешает обмен, console.log в консоли.

### Phase 7 — Breakpoints + rewrite + repeat/edit&resend/compose (~4–5 дней) ✅ (2026-10-18, окна — без ручной проверки)

Основа — S1 (движок применяет правки пайплайна, тела для пайплайна). Цепочка пайплайнов:
Scripting → Rewrite → Breakpoints (видят итоговое сообщение) → Rules (Block/Map).

- [x] **7a. Rewrite**: применение `RewriteOp` в домене (заголовки, find/replace в теле — текст/regex,
      URL, статус), `rewrite_enabled`, все совпавшие правила по порядку; `RewriteInterceptionPipeline`
      (тела распакованы, как у скриптов — общий хелпер); вкладка Rewrite в Tools с диалогом операций
- [x] **7b. Repeat / Edit & Resend / Compose**: `engine::DirectHttpClient` (async-channel в движок,
      тот же конвейер `forward`: правила, скрипты, захват) → `DirectHttpService`; теги
      `repeated`/`composed`; окно Compose (метод, URL, заголовки, тело, ответ); пункты Repeat /
      Edit & Resend… в контекстном меню, Compose… в меню
- [x] **7c. Breakpoints**: `BreakpointRule.enabled` + `breakpoints_enabled`; `BreakpointInterceptionPipeline`
      — парковка на oneshot, `InterceptWaiting{preview}` / `InterceptResolved`, таймаут → Resume без
      правок; `EngineCommand::ResolveIntercept` → пайплайн; окно брейкпоинта (заголовки/тело/статус,
      Execute / Abort); вкладка Breakpoints в Tools

**Проверка**: breakpoint паркует curl, в UI InterceptWaiting; Edit → Resume → upstream видит правки (httpbin.org/headers); repeat даёт новый exchange с тегом; compose с нуля работает.

### Phase 8 — HAR export + сессии + mirror (~3 дня) ⬜

- [ ] HAR-сериализатор (serde: log/entries/startedDateTime/timings-маппинг из TimingPhases/content base64, опция «без тел»)
- [x] Сессии — файлом `.crabtrap` по выбранному пути (Track P7), вместо каталога сессий
- [ ] Mirror responses to disk (дерево host/path)
- [ ] UI: менеджер сессий, меню Export HAR

**Проверка**: HAR валиден (Chrome DevTools / har-validator); сессия 1000 обменов сохраняется/загружается (тела и timings на месте); mirror раскладывает файлы по дереву.

### Phase 9 — Внешний прокси (PAC) + SOCKS5 + клиентские сертификаты (~4 дня) ⬜

- [ ] `engine/external_proxy.rs`: upstream HTTP-прокси (CONNECT + absolute-form, Proxy-Authorization), учёт в UpstreamClient
- [ ] `engine/pac.rs`: загрузка PAC + `FindProxyForURL` через boa_engine
- [ ] `engine/socks.rs`: SOCKS5 listener (greet/auth-none/CONNECT, доменная адресация = SOCKS5h) → общий конвейер MITM
- [ ] Клиентские сертификаты per-host (ClientCertificate из rules.json; пароли pkcs12 — Keychain)
- [ ] UI: настройки внешнего прокси/PAC/SOCKS

**Проверка**: цепочка crabtrap → внешний прокси (виден в его логах); PAC применяется; `curl --socks5-hostname 127.0.0.1:1080 https://example.com` виден как обычный MITM-exchange; mTLS-сайт проходит с клиентским сертом.

### Phase 10 — Reverse proxy + port forwarding + DNS spoofing (~3 дня) ⬜

- [x] `engine/reverse.rs`: отдельный listener → перезапись Host/URL на target (preserve_host on/off) — Track P7
- [ ] `engine/port_forward.rs`: TCP forward (запись соединения без exchange)
- [x] DNS spoofing — сделан в Track P5 (override-таблица в коннекторе и CONNECT; системный резолвер)
- [ ] UI network-контекста

**Проверка**: reverse `localhost:9090 → https://api.example.com`; port forward `127.0.0.1:5433 → db:5432` (psql); DNS spoof `evil.example.com → 127.0.0.1` (exchange фиксирует оригинальный Host).

### Phase 11 — HTTP/2 + WebSockets (~5 дней) ✅ (2026-10-18; resend WS — нет)

- [x] Сервер: ALPN h2 (hyper-util server-auto: h1/h2) в MITM-ветке
- [x] Клиент: ALPN `h2,http/1.1`, `Connected::negotiated_h2` — legacy-пул ведёт h2 сам; `te: trailers` сохраняется (gRPC)
- [x] `engine/ws.rs`: детект Upgrade → HTTP/1.1-клиент, 101 → `hyper::upgrade::on` обеих сторон, пересылка + парсер фреймов (`ws_frames.rs`) → `ProxyEvent::WebSocketMessage`; `Sec-WebSocket-Extensions` снимается (без permessage-deflate); сообщения в обмене (потолок 2000 × 64 KiB); вкладка Messages у ответа
- [x] ревью: сообщения — в репозитории отдельно от обмена (VecDeque, 16 MiB/обмен), инспектор
      подгружает дельту; Stop закрывает открытые сокеты (aborted); сокет держит слот соединения;
      close→reset — штатное закрытие; трейлеры (gRPC) переживают загрузку тела; h2: Cookie склеиваются,
      Host≠authority → HTTP/1.1-клиент, MITM h2 привязан к хосту CONNECT
- [x] WS: resend сообщения (Track P7)
- [ ] WS: idle-таймаут; правила фазы ответа на рукопожатии 101 не применяются;
      тесты wss через MITM и POST/gRPC через h2 — не написаны
- [x] Raw-view для h2 — decoded-рендер (HPACK wire не показать), пометка в UI; Protocol=HTTP/2 в трафике

**Проверка**: h2-only сайт (Cloudflare) через MITM работает, protocol=HTTP/2 в деталях; ws://echo.websocket.org показывает сообщения; wss через MITM; повторная отправка WS-сообщения.

### Track P — Паритет с Proxyman (по итогам сравнения 2026-10-20) 🚧

Порядок — по пользе. Новые поля `rules.json` — только `serde(default)`; валидация — в
`UpdateRuleSetUseCase`; применение — горячее (без разрыва соединений).
Вне рамок: `$http`/файлы в скриптах, Protobuf, Diff (решаются отдельно).

- [x] **P1. Map Local как в Proxyman** (2026-10-20)
  - [x] `MapLocalRule.headers: Vec<HeaderEntry>` → `RouteDecision::MapLocal` → ответ движка
        (дописываются после Content-Type/Content-Length движка; на 404 «нет файла» не применяются)
  - [x] валидация: имя — токен, значение — ASCII в одну строку; фрейминг/hop-by-hop запрещены,
        Content-Type — только своим полем (`response_form::map_local_header_error`)
  - [x] UI: редактор заголовков ответа в диалоге Map Local
  - [x] «Save Response & Map Local…»: `SaveResponseMockUseCase` + порт `MockFileStore`
        (`DirectoryMockFileStore`, `<app-support>/map_local/<host>/…`, без перезаписи: `-2`, `-3`…);
        диалог с префиллом status/Content-Type/заголовков (без Content-Encoding/Set-Cookie/Date/ETag/Alt-Svc)
  - [x] таблица MIME: avif, mp4, webm, mp3, ttf/otf/woff, csv, yaml, map, md
  - [x] документация: `docs/guide.md` (все Tools), ссылка в README, Help → User Guide
  - [ ] ручная проверка диалога и пункта меню в GUI — не делалась агентом
- [x] **P2. Матчинг** (2026-10-20)
  - [x] домен: `MatchTarget` (URL/хост/path/метод/статус/тип/GraphQL) вместо позиционных аргументов;
        `TrafficMatch.url_regex` (якорный, кэш компиляции `regex_cache`, общий с Rewrite и `body_regex`),
        `ignore_query`, `graphql_operation` (glob); `RuleSet::graphql_body_needed`
  - [x] `application::matching::RequestTarget` — единый разбор запроса для всех пайплайнов, throttle
        и Test скрипта; имя операции из `operationName`/документа (query или JSON-тело, пакет — первая)
  - [x] тело ради GraphQL грузится только когда правило ждёт операцию (`wants_body` правил)
  - [x] валидация: regex компилируется, ≤ 2048 символов; GraphQL не пустой; throttle без GraphQL;
        условие из одних флагов — пустое
  - [x] UI: общий компонент `MatchFields` (Wildcard/Regex, Ignore query, Method, GraphQL op,
        превью, Test URL) в диалогах Tools, Rewrite и Script Editor; таблица правил показывает regex/GraphQL
  - [x] документация: раздел Matching rules в `docs/guide.md`
  - [ ] ручная проверка диалогов в GUI — не делалась агентом
- [x] **P3. Экспорт** (2026-10-20)
  - [x] Copy as cURL: запрос, ушедший на сервер (после правок) + его тело; sh-кавычки, `--data-raw`
        (без `@file`), бинарное — `printf '\ooo' | curl --data-binary @-` (NUL цел), Host — если отличается, `--globoff`
  - [x] HAR 1.2 (`use_cases/export/har.rs`): тела ответа распакованы (бинарь — base64), cookies,
        queryString, postData, тайминги (`-1` — не измерено), теги — в `comment`; обмены без ответа — нет
  - [x] `ExportTrafficUseCase` + порт `ExportFileWriter`/`ExportFile`: HAR пишется потоком (`HarWriter`,
        тела — по одному обмену); `FsExportFileWriter`: tmp 0600 с уникальным именем, fsync, rename, без commit — удаляется
  - [x] UI: контекстное меню (Copy as cURL, Export as HAR…), меню File (сессия; с телами / без)
  - [x] Copy as fetch / fetch (Node.js) как в Chrome (`use_cases/export/fetch.rs`): запрещённые
        заголовки убраны, `referrer` / `mode` / `credentials` для браузера, cookie + Referer для Node;
        все «Copy …» — в подменю Copy
  - [x] документация: раздел Export в `docs/guide.md`, README, CHANGELOG
  - [x] Import HAR — Track P7 (`sessions/har_import.rs`)
  - [ ] ручная проверка в GUI и открытие HAR в Chrome DevTools — не делалась агентом
- [x] **P4. Allow List** (2026-10-20)
  - [x] домен: `AllowEntry`, `RuleSet.allow_list`/`allow_enabled` (старый rules.json — выключен),
        `records_host` (действует только при ≥1 активном хосте), общий `host_pattern_matches` с SSL
  - [x] порт: `InterceptionPipeline::records_host`, `ConnectDecision::Bypass`; обёртки делегируют
  - [x] движок: CONNECT вне списка — туннель без записи (`SilentPublisher`), HTTP — обычный путь, но
        `BypassPipeline` + без событий/tee/throttle; Block List важнее; Repeat/Compose пишутся всегда
  - [x] UI: вкладка Tools → Allow List (⌥⌘A), «Allow Only <host>» в контекстном меню, пометка в строке
        состояния; валидация шаблонов
  - [x] документация: раздел Allow List в `docs/guide.md`, README, CHANGELOG
  - [ ] ручная проверка в GUI — не делалась агентом
- [x] **P5. DNS Spoofing** (2026-10-20)
  - [x] домен: `DnsSpoofEntry`, `RuleSet.dns_spoof`/`dns_spoof_enabled` (старый rules.json — вкл, пусто),
        `effective_dns_overrides`; `DnsOverride` — шаблон хоста, `spoofed_ip` (первое совпадение, общий
        `host_pattern_matches`)
  - [x] движок: подмена в `HttpsConnector` и dial CONNECT; `SetDnsOverrides` при изменении таблицы
        пересоздаёт `UpstreamClients` (пустые пулы — keep-alive не ходит на старый адрес); тег
        `dns-spoofed` у обмена (итоговый хост после Map Remote) и туннеля
  - [x] app: `SetDnsOverrides` при каждом изменении правил и перезапуске движка; Tools → DNS Spoofing,
        диалог с полем IP (`parse_ip`, `[::1]` допускается), «DNS Spoofing…» в контекстном меню
  - [x] валидация шаблонов в `UpdateRuleSetUseCase`; тесты `engine/tests/dns_spoof.rs`
  - [x] документация: раздел DNS Spoofing в `docs/guide.md`, README, CHANGELOG
  - [ ] ручная проверка в GUI — не делалась агентом
- [x] **P6. Map Remote/сеть** (2026-10-20)
  - [x] `MapRemoteRule.path_mode` (`Replace` по умолчанию | `Prefix`): ведущие сегменты пути, совпавшие с неизменными сегментами `path_glob`
        (без `*`; `v1*` — только ровно `v1`) → путь цели; остаток сохраняется; нет совпадения — путь цели спереди
  - [x] WS/WSS: рукопожатие идёт по маршруту Map Remote (тест `websocket.rs`); цели `ws://`/`wss://`
        = `http(s)://` (валидация, форма); wss через MITM — тот же `forward`, отдельного теста нет
  - [x] throttle: `loss_percent` (0–100) в профиле/DTO, обрыв после задержки (`fastrand`), тег
        `dropped`; пресеты Lossy 3G, Very Bad Network, 100% Loss; поле Drop % в UI; `ThrottleValues`
  - [x] документация: Map Remote и Throttle в `docs/guide.md`, CHANGELOG
  - [ ] ручная проверка в GUI — не делалась агентом
- [ ] **P7.** Крупное — по существующим фазам: Reverse Proxy (Phase 10), resend WS (Phase 11),
      сессии/mirror (Phase 8)
  - [x] Reverse Proxy (2026-10-20): `ReverseProxyEntry` в RuleSet (`reverse_proxy_enabled`, старый
        rules.json — вкл, пусто), `effective_reverse_proxies`; `EngineCommand::SetReverseProxies` —
        `ReverseProxies` в цикле `run` (дифф таблицы, общий лимит соединений, интерфейс основного
        листенера), событие `ReverseProxyStatus`; origin-form → цель + path/query, Host цели или
        клиента, CONNECT — 405, тег `reverse-proxy`; валидация (порт, origin без пути, уникальный порт
        среди включённых); Tools → Reverse Proxy (`reverse_dialog.rs`, `ReverseProxyForm`),
        уведомление о занятом порте; тесты `engine/tests/reverse_proxy.rs`
  - [x] resend WS (2026-10-20): `EngineCommand::SendWebSocketMessage` → регистр `WsSockets`
        (`ws_inject.rs`, очередь на направление); `pump` вставляет кадр только на границе сообщения
        (`WsParser::at_message_boundary`), к серверу — с маской; `WebSocketMessage.injected`;
        `WebSocketSendFailed` → уведомление; вкладка Messages: Resend, Edit & Send…, Send Message…
  - [x] сессии (2026-10-20): `.crabtrap` = gzip NDJSON (`sessions/format.rs`: заголовок + запись на
        обмен с телами base64 и WS-сообщениями, потоком, потолок строки 512 MiB);
        `SessionFileUseCase` save/open/import_har (новые id, перепривязка тел, тег `imported`,
        Pending → Aborted, частичный импорт при битом хвосте); порт `ImportFileReader`;
        `TrafficStore::import`; File → Open/Save Session (⌘O/⌘S), Import HAR…; инспектор берёт
        сжатие из `BodyRef.encoding`
  - [ ] mirror

**Проверка**: на каждую подфазу — `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`,
`cargo test --workspace`; ручная — собственный тестовый инстанс на отдельном порту + `curl -x`.

### Phase 12 — Упаковка и полировка (~3 дня) 🚧

- [x] Иконка (`packaging/macos/icon/AppIcon.svg` → `scripts/make-icon.sh`: resvg + iconutil → `AppIcon.icns`)
- [x] Меню и hotkeys: ⌘F поиск (было), ⌃R Repeat и ⌃⌥B Breakpoint для выбранного запроса
      (⌘R занят записью); пункты Tools → Repeat Selected / Breakpoint for Selected; About (было)
- [x] `.app`-бандл: `packaging/macos/Info.plist.in` (LSMinimumSystemVersion 13.0, NSHighResolutionCapable,
      NSLocalNetworkUsageDescription), `scripts/bundle-macos.sh [--universal]` → `dist/*.app` + `.dmg` + `.sha256`
- [x] codesign: hardened runtime БЕЗ app-sandbox (`entitlements.plist`); Developer ID + `notarytool` —
      если заданы MACOS_SIGN_IDENTITY / App Store Connect key, иначе ad-hoc
- [ ] Developer ID и нотаризация — нет сертификата (пока ad-hoc, README: «Open Anyway» / xattr)
- [x] Release-профиль (opt-level 3, lto thin, codegen-units 1, strip); README; LICENSE (MIT OR Apache-2.0); CHANGELOG
- [x] Предупреждение «системный прокси указывает на crabtrap, а прокси остановлен» в статус-баре
- [x] Check for Updates: порт `ReleaseFeed` → `GithubReleaseFeed` (curl), меню + тихая проверка при старте
- [x] CI (fmt, clippy -D warnings, test, cargo deny, smoke-бандл); release.yml (тег → universal .dmg →
      черновик релиза); homebrew.yml (публикация → cask в `<owner>/homebrew-tap`); `scripts/release.sh`
- [x] cargo deny: rustls 0.23.46 (4 уязвимости), lru 0.18 (unsound), rustls-pemfile → pki-types PemObject

**Проверка**: `cargo build --release`; .app запускается; `spctl -a -vv` проходит (только с Developer ID —
пока ad-hoc: rejected, ожидаемо); firewall-промпт обработан.

---

## Сквозные риски

1. **smol vs tokio**: события в gpui — только через `async-channel`; в gpui-тасках никакого tokio-IO.
2. **gpui-kit vs macOS 14.5**: дока требует macOS 15+ — проверить на Phase 0; план Б — даунгрейд версии gpui-kit / git-dep zed gpui.
3. **QUIC/HTTP3**: браузеры уходят в UDP 443 мимо прокси — документировать (отключать QUIC в браузере при отладке).
4. **Память**: ring buffer + GC тел с фазы 4; метрики в статус-баре.
5. **gpui-kit 0.x churn**: пин версии, UI-обёртки тонкие (TableDelegate знаем только в `traffic/table.rs`).
6. **Время**: UTC chrono, тайминги — монотонный `Instant`.
7. **Тестируемость**: integration-тесты `engine/tests/` с фазы 2.
