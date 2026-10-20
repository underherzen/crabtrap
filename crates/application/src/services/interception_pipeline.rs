//! Порт конвейера перехвата — сердце управления трафиком.
//!
//! Движок await-ит решения пайплайна на каждой фазе обмена; здесь
//! проецируются правила (rewrite/map/block/breakpoint/throttle) и
//! парковка брейкпоинтов (oneshot в BreakpointRegistry, Phase 7).

use std::path::PathBuf;

use domain::entities::exchange::{HttpRequest, HttpResponse};
use domain::entities::rule::{BreakPhase, RuleSet};
use domain::value_objects::{ExchangeId, InterceptId};

use crate::dto::rules::InterceptResolutionDto;
use domain::value_objects::Host;
use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo};

/// Решение по CONNECT-соединению.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectDecision {
    /// Расшифровать (MITM) — хост в SSL-Proxying списке.
    Mitm,
    /// Прозрачный туннель — байты не трогаем.
    Passthrough,
    /// Отказать в соединении.
    Block,
}

/// Куда дальше двигаться обмену после пайплайна.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// Штатно в origin.
    Origin,
    /// Ответить локальным файлом (Map Local). `root` — путь из правила
    /// (файл или каталог); `relative` — уже декодированный и очищенный path
    /// запроса для правила-каталога (None — путь отклонён: скрытый файл,
    /// закодированный разделитель). Файловую систему проверяет движок.
    /// `status` / `content_type` / `headers` — ответ, когда файл найден
    /// (`None` — тип по расширению; `headers` — дополнительные, по порядку).
    MapLocal {
        root: PathBuf,
        relative: Option<PathBuf>,
        status: u16,
        content_type: Option<String>,
        headers: Vec<HeaderEntry>,
    },
    /// Перенаправить на другой URL (Map Remote). `preserve_host` — оставить
    /// исходный заголовок Host (виртуальные хосты на staging).
    MapRemote { url: url::Url, preserve_host: bool },
    /// Заблокировать кодом (Block List).
    Blocked(u16),
    /// Ответить готовым ответом без похода в origin (скрипт Mock API).
    /// Content-Length движок выставляет сам.
    Respond {
        status: u16,
        headers: Vec<HeaderEntry>,
        body: Vec<u8>,
    },
}

impl RouteDecision {
    /// Тег строки трафика для неисходного маршрута.
    pub fn tag(&self) -> Option<&'static str> {
        match self {
            Self::Origin => None,
            Self::MapLocal { .. } => Some("map-local"),
            Self::MapRemote { .. } => Some("map-remote"),
            Self::Blocked(_) => Some("blocked"),
            Self::Respond { .. } => Some("mocked"),
        }
    }
}

/// Как оборвать обмен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortMode {
    /// Молча закрыть соединение.
    Close,
    /// Ответить синтетическим ответом и закрыть.
    Respond { status: u16, body: Vec<u8> },
}

/// Результат фазы пайплайна.
///
/// `Proceed.message`, отличающийся от исходного, движок применяет: у
/// запроса — method/uri/headers/body, у ответа — status/headers/body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterceptOutcome {
    /// Продолжить: возможно, сообщение отредактировано; маршрут задан
    /// правилами (Map/Block) или `Origin`.
    Proceed {
        message: InterceptableMessage,
        route: RouteDecision,
    },
    /// Оборвать обмен. `tag` — тег строки трафика (`blocked` у Block List),
    /// чтобы обрыв по правилу отличался от сетевой ошибки.
    Abort {
        mode: AbortMode,
        tag: Option<&'static str>,
    },
}

/// Владеемое перехваченное сообщение (request ИЛИ response),
/// удобно для редактирования в модалке брейкпоинта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterceptableMessage {
    /// Заполнено, если это запрос.
    pub method: Option<String>,
    /// Заполнено, если это запрос.
    pub uri: Option<String>,
    pub version: Option<HttpVersionVo>,
    /// Заполнено, если это ответ.
    pub status: Option<u16>,
    pub reason: Option<String>,
    pub headers: Vec<HeaderEntry>,
    /// Тело без Content-Encoding-преобразований движка (как на проводе).
    /// `None` — тело не загружено (идёт потоком: пайплайн не просил его через
    /// [`InterceptionPipeline::wants_body`] или оно больше капа). `Some` в
    /// ответе пайплайна — заменить тело этим буфером.
    pub body: Option<Vec<u8>>,
}

impl InterceptableMessage {
    pub fn from_request(request: &HttpRequest) -> Self {
        Self {
            method: Some(request.method.to_string()),
            uri: Some(request.uri.clone()),
            version: Some(request.version),
            status: None,
            reason: None,
            headers: request.headers.clone(),
            body: None,
        }
    }

    pub fn from_response(response: &HttpResponse) -> Self {
        Self {
            method: None,
            uri: None,
            version: Some(response.version),
            status: Some(response.status),
            reason: response.reason.clone(),
            headers: response.headers.clone(),
            body: None,
        }
    }

    /// Это запрос (а не ответ)?
    pub fn is_request(&self) -> bool {
        self.method.is_some() && self.status.is_none()
    }
}

/// Контекст фазы: какой обмен и на какой фазе перехвачен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterceptContext {
    pub exchange_id: ExchangeId,
    pub phase: BreakPhase,
    pub message: InterceptableMessage,
    /// На фазе ответа — итоговый запрос (после правок фазы запроса), чтобы
    /// правила ответа матчились по URL/методу. На фазе запроса — `None`.
    pub request: Option<InterceptableMessage>,
}

/// Единый async-порт перехвата (см. D8 в PLAN.md).
///
/// Методы намеренно infallible: ошибка правила (битый regex и т.п.)
/// деградирует в Proceed{Origin} / Passthrough, а не роняет соединение.
#[async_trait::async_trait]
pub trait InterceptionPipeline: Send + Sync {
    /// Решение по CONNECT до TLS-хендшейка (SSL-Proxying список + block).
    async fn on_connect(&self, host: &Host, port: u16) -> ConnectDecision;

    /// Нужно ли пайплайну тело этой фазы (скрипт/rewrite тела совпал).
    /// Дёшево и синхронно: движок вызывает до буферизации; `true` —
    /// тело собирается в память (с капом) и приходит в `message.body`.
    fn wants_body(&self, _context: &InterceptContext) -> bool {
        false
    }

    /// Обработка запроса (rewrite request / breakpoints / map / block).
    async fn on_request(&self, context: InterceptContext) -> InterceptOutcome;

    /// Обработка ответа (rewrite response / breakpoints / block).
    async fn on_response(&self, context: InterceptContext) -> InterceptOutcome;

    /// Горячая перезагрузка правил (EngineCommand::UpdateRules) — без
    /// разрыва соединений. Пайплайны без правил игнорируют.
    fn update_rules(&self, _rules: RuleSet) {}

    /// Решение пользователя по брейкпоинту (EngineCommand::ResolveIntercept).
    /// Пайплайны без брейкпоинтов игнорируют; обёртки передают внутрь.
    fn resolve_intercept(&self, _id: InterceptId, _resolution: InterceptResolutionDto) {}
}
