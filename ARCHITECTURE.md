# Crabtrap Architecture (`crabtrap`)

Этот документ предназначен в первую очередь **для чтения агентом**: чтобы быстро
понять, **какие слои существуют**, **какие зависимости допустимы**, и **куда
добавлять код** при реализации новой функциональности. Модель — clean/hexagonal;
поверх неё добавлен слой
`engine` (прокси-движок). Рабочий трекер фаз — `PLAN.md`.

## TL;DR

- **`domain`**: чистая предметная область (HttpExchange, RuleSet, VO, контракты
  репозиториев, `DomainError`). Никакого IO — ни файлов, ни сокетов, ни tokio.
- **`application`**: use cases, DTO на границах и *порты* (trait'ы сервисов:
  `ProxyEngineService`, `InterceptionPipeline`, `EventPublisher`,
  `CertificateService`, ...). **Без tokio** — runtime-agnostic.
- **`engine`** (Phase 2+): адаптер прокси-движка — MITM-прокси на чистом tokio,
  **без gpui**. Реализует `ProxyEngineService`/`DirectHttpService`, шлёт
  `ProxyEvent`'ы через `EventPublisher`.
- **`infrastructure`**: реализации портов/репозиториев, не связанные с сетевым
  конвейером: in-memory exchange store (ring buffer), body store (memory+spill),
  rules.json, канал событий, сертификаты (Phase 3), системный прокси.
- **`app`** (Phase 4+): композиционный корень + GPUI-UI.
  Создаёт репозитории/сервисы, связывает каналы, рисует экраны.
- **`config`**: `AppConfig` (TOML в app-support dir), `load_or_init()`/`save()`.

Направление зависимостей:

```text
app  -> application -> domain
app  -> engine      -> application -> domain
app  -> infrastructure -> application -> domain
config -> (app, infrastructure, engine)

domain НЕ зависит ни от кого
```

## Workspace layout

```text
crabtrap/
  Cargo.toml               # workspace (edition 2024, resolver 3), общие зависимости
  crates/
    domain/                # 0 IO: entities/ value_objects/ repositories/ errors.rs
    application/           # use_cases/ dto/ services/ (ПОРТЫ); БЕЗ tokio
    engine/                # MITM-прокси на tokio, БЕЗ gpui (Phase 2+)
    infrastructure/        # exchange_store, body_store, rule_store, channel_publisher, ...
    config/                # AppConfig (TOML)
    app/                   # bin crabtrap: композиция + GPUI (Phase 4+)
```

## Слои и что в них лежит

### `crates/domain` (Domain)

```text
crates/domain/src/
  entities/                # HttpExchange/HttpRequest/HttpResponse, BodyRef, RuleSet
                           # (+ все rule-сущности), network, session, certificate
  value_objects/           # ids (ExchangeId, ...), Host, http_vo (Method, HeaderEntry,
                           #   Protocol, ...), TimingPhases, TlsInfo, TrafficMatch,
                           #   FilterQuery
  repositories/            # trait'ы: ExchangeRepository, BodyRepository, RuleRepository,
                           #   SslProxyRepository, SessionRepository (+BodySource),
                           #   CertificateRepository, ExternalProxyRepository, NetworkRepository
  errors.rs                # DomainError (thiserror, String-payload)
```

Правила:

- **Можно**: бизнес-типы, инварианты (Host-нормализация, TrafficMatch-глоб),
  доменные ошибки, типы идентификаторов, контракты `*Repository`.
- **Нельзя**: любые детали IO (файлы, сокеты, tokio, gpui), `bytes`, доступ к ФС.
  `regex` — единственная «вычислительная» зависимость помимо serde/chrono/uuid.
- Тела обменов домену неизвестны как байты: только `BodyRef` (ссылка). Методы
  `BodyRepository` принимают `Vec<u8>`; работа с `bytes::Bytes` — деталь engine.

### `crates/application` (Application / Use Cases)

```text
crates/application/src/
  use_cases/               # capture/ (RecordExchangeEvent, GetExchangeList, ClearTraffic),
                           #   inspection/, rules/ (Get/UpdateRuleSet), proxy_config/,
                           #   certificates/, sessions/, compose/
  dto/                     # capture.rs (ExchangeSummaryDto, ProxyEvent),
                           #   proxy_config.rs (EngineCommand, ProxySettingsDto),
                           #   inspection.rs, rules.rs, certificates.rs, sessions.rs, compose.rs
  services/                # ПОРТЫ: ProxyEngineService, InterceptionPipeline(!),
                           #   EventPublisher, CertificateService, SystemProxyService,
                           #   DirectHttpService, RevealService
```

Правила:

- **Можно**: оркестрацию репозиториев/портов, DTO, валидацию входов
  (например, `UpdateRuleSetUseCase` проверяет инварианты RuleSet).
- **Нельзя**: tokio (никаких `#[tokio::test]`, `tokio::spawn`), прямое IO,
  детали UI. Юнит-тесты используют `futures::executor::block_on`.
- Use case-паттерн: `struct XUseCase { deps: Arc<dyn T> }`,
  `new(...)`, `pub async fn execute(dto) -> Result<Out, DomainError>`.

### `crates/engine` (Engine — Phase 2+)

Адаптер прокси: `server.rs`, `http_proxy.rs`, `connect.rs` (CONNECT →
Mitm|Passthrough через `InterceptionPipeline::on_connect`), `mitm.rs`
(CertIssuer), `capture.rs` (RecordingStream tee), `intercept.rs`, `throttle.rs`.

Правила:

- **Чистый tokio, БЕЗ gpui** — крейт переиспользуется headless-бинарём
  `crabtrapd`.
- Движок не знает про UI: всё, что нужно сообщить наружу — `ProxyEvent` в
  `EventPublisher`; всё, что приходит извне — `EngineCommand`.
- **Сертификатная крипта — НЕ здесь**: rcgen живёт в infrastructure
  (`RcgenCertificateService`), движок потребляет PEM-DTO через порт
  `CertificateService` (Phase 3) и сам строит rustls `CertifiedKey`.
- Единственное касание infrastructure — headless-бинарь `crabtrapd`
  (композиция реального cert-сервиса; фича `mitm-bin`, по умолчанию вкл.).
  Библиотека движка остаётся на портах application.
- Аватары домена в конвейере: `InterceptableMessage` (owned request/response),
  решения — `ConnectDecision` / `InterceptOutcome`.

### `crates/infrastructure` (Infrastructure / Adapters)

```text
crates/infrastructure/src/
  repositories/            # InMemoryExchangeRepository (ring buffer),
                           # MemorySpillBodyStore (memory + disk spill),
                           # FileRuleRepository (rules.json)
  services/                # ChannelEventPublisher (async-channel)
                           # Phase 3+: certificate_service, system_proxy_service, ...
```

Правила:

- **Можно**: ФС, каналы, блокировки, внешние утилиты ОС (`osascript`,
  `networksetup`, `security`), сериализацию, ретраи.
- **Нельзя**: бизнес-правила (их место в domain/application), gpui.

### `crates/app` (Composition Root + GPUI — Phase 4+)

`main.rs` (конфиг -> infra -> engine -> окна), `app_state.rs`
(`Arc<dyn *Repository>` + `Arc<dyn *Service>`), `runtime.rs` (tokio-потоки +
`async-channel` + pump событий в gpui), `ui/` (traffic/, inspector/, rules/, ...).

### `crates/config` (Config)

`AppConfig`: TOML в `~/Library/Application Support/crabtrap/config.toml`.
Читается в композиции (`app/main.rs`), передаётся туда, где нужен
(infrastructure-сторам: ёмкости и каталоги).

## Ключевые контракты (границы слоёв)

- **`ProxyEvent`** (engine → UI, `application::dto::capture`):
  ExchangeStarted / RequestBodyDone / ResponseHeaders / BodyProgress /
  ExchangeCompleted / ExchangeFailed / ConnectionOpened/Closed /
  InterceptWaiting/Resolved / EngineStatus. Единственный транспорт —
  **async-channel** (`ChannelEventPublisher`).
- **`EngineCommand`** (UI → engine, `application::dto::proxy_config`): Start /
  Stop / UpdateRules{snapshot} / UpdateSslProxying / UpdateThrottle /
  ResolveIntercept{id, Resume(edited)|Abort} / SetExternalProxy / SetDnsOverrides.
- **`InterceptionPipeline`** (application-порт, реализуется infrastructure):
  `on_connect(host, port) -> ConnectDecision`, `on_request/on_response(ctx) ->
  InterceptOutcome { Proceed{message, route}, Abort{mode} }`. Маршрутизация
  map local/remote/block — это `RouteDecision`, брейкпоинт — парковка на
  oneshot внутри реализации.
- **`ExchangeRepository` / `BodyRepository` / `RuleRepository`** — контракты
  хранения; `RecordExchangeEventUseCase` сводит поток `ProxyEvent` в обмены.

## Как определять «куда писать код» (decision tree)

### 1) Это бизнес-смысл, инвариант, доменная модель?

- Да → `crates/domain`:
  - новая/изменённая сущность → `domain/src/entities`
  - новый value object → `domain/src/value_objects`
  - контракт репозитория → `domain/src/repositories`
  - доменная ошибка → `domain/src/errors.rs`

### 2) Это сценарий/оркестрация «что сделать» (use case)?

- Да → `crates/application/src/use_cases/<context>/...`
  - вход/выход use case'а → `crates/application/src/dto/<context>/...`
  - нужен внешний мир (движок, ОС, сеть) → trait в
    `crates/application/src/services`

### 3) Это поведение прокси-конвейера (байты, TLS, стримы, соединения)?

- Да → `crates/engine` (реализация `ProxyEngineService`/`DirectHttpService`,
  emit `ProxyEvent`'ы). Конвейер решений о трафике НЕ пишется здесь: движок
  спрашивает `InterceptionPipeline` и исполняет вердикт.

### 4) Это конкретная реализация IO/хранения/OС без конвейера соединений?

- Да → `crates/infrastructure`:
  - реализация доменного репозитория → `infrastructure/src/repositories`
  - реализация application-порта (каналы, файлы, osascript) →
    `infrastructure/src/services`

### 5) Это UI, композиция, мост async↔gpui?

- Да → `crates/app`:
  - wiring зависимостей → `app/src/main.rs`, `app_state.rs`
  - pump событий/команд → `app/src/runtime.rs`
  - экраны → `app/src/ui/...` (UI тонкий: знает DTO и use case'ы, не домен
    хранения)

## Практические правила зависимости (для агента)

- **События только через `async-channel`**: никакого tokio-IO в gpui-тасках
  (см. D4 в PLAN.md). `tokio::sync::mpsc` вне tokio-рантайма не работает,
  gpui — smol-based; `async_channel` корректен для обоих.
- **`engine` — чистый tokio-адаптер без gpui**: тестируется интеграционными
  тестами (`engine/tests/`) и переиспользуется headless-бинарём `crabtrapd`.
- **`app` — только композиция + GPUI**: никакой бизнес-логики; хендл UI-действия
  = собрать DTO → вызвать use case → отправить `EngineCommand`.
- **Use case'ы принимают зависимости явно** (`new(Arc<dyn ...>)`), никаких
  глобальных состояний.
- **DTO живут в `application`**, не в `app`/`engine` — внешние слои тонкие.
- **Domain без IO**: если в `domain` понадобился файл/сокет/tokio — логика
  попала не в тот слой; контракт уходит в репозиторий, реализация — в infra.
- Сериализуемые схемы (`rules.json`, сессии) держать стабильными: новые поля —
  с `#[serde(default)]`.

## Типовые потоки данных

### Захват трафика (Phase 4)

```text
engine MonitoredBody/StreamTracker (tee ДЕКОДИРОВАННЫХ кадров, cap; raw wire — D7/Phase 5)
  -> ProxyEvent (ExchangeStarted{tls} / RequestBodyDone{body} / ...Completed{body})
  -> ChannelEventPublisher (async-channel)
  -> app/runtime.rs (pump, батчи по 64 события/пробуждение)
  -> RecordExchangeEventUseCase::execute(event)  -- HttpExchange в ExchangeRepository
  -> GPUI TrafficStore.apply_batch (таблица/статусы/статистика)
  -> infrastructure::InMemoryExchangeRepository (ring buffer) + MemorySpillBodyStore
     (единый инстанс стора тел у движка и UI: tee пишет, инспектор читает по BodyRef)
```

### Правила (Phase 6)

```text
UI-редактор -> UpdateRuleSetUseCase (валидация) -> RuleRepository (FileRuleRepository,
rules.json, version++) -> UI шлёт EngineCommand::UpdateRules{snapshot}
-> engine hot-reload (снапшот подменяется без разрыва соединений)
```

### Брейкпоинт (Phase 7)

```text
engine: on_request -> InterceptionPipeline (infrastructure)
  -> BreakpointRule совпал -> парковка на oneshot + ProxyEvent::InterceptWaiting
  -> UI модалка -> EngineCommand::ResolveIntercept{Resume(edited)|Abort}
  -> oneshot разбужен -> InterceptOutcome::Proceed{message, route} -> engine продолжает
```

## Где «bounded context» в текущей структуре

Bounded contexts отражены внутри `application`: `use_cases/*` и `dto/*`
(capture, inspection, rules, certificates, sessions, compose, proxy_config).
Экраны UI (`app/src/ui/*`) зеркалят эти контексты, но не считаются
«владельцами» логики — они только адаптеры.
