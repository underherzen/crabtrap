//! Прямые (non-proxy) запросы на порт прокси: `/ssl` — инструкция и
//! скачивание CA (бутстрап устройства ДО настройки прокси), `/ca.pem`,
//! `/crabtrap.mobileconfig` — профиль iOS (CA + Wi-Fi-прокси одной
//! установкой), прочее — баннер «proxy is running». Те же страницы — через
//! прокси по адресу-ловушке `http://crabtrap.ssl` (как chls.pro/ssl).

use application::dto::certificates::CaStatusDto;
use http::{Request, Response};
use hyper::body::Incoming;
use rustls::pki_types::{CertificateDer, pem::PemObject};

use crate::body::{MonitoredBody, StreamTracker};
use crate::http_proxy::{ProxyService, text_response};
use crate::mobileconfig::{DeviceProfile, MOBILECONFIG_MIME, mobileconfig, xml_escape};
use crate::wifi;

/// Порт, если в Host его нет (страница открыта по http без порта).
const DEFAULT_HTTP_PORT: u16 = 80;

/// Адрес-ловушка: запрос к нему через прокси отдаёт страницу устройства, а
/// не уходит в сеть (домена нет — в обход прокси он не откроется).
pub(crate) const SETUP_HOST: &str = "crabtrap.ssl";

/// Запрос к адресу-ловушке.
pub(crate) fn is_setup_host<B>(req: &Request<B>) -> bool {
    req.uri()
        .host()
        .is_some_and(|host| host.eq_ignore_ascii_case(SETUP_HOST))
}

impl ProxyService {
    /// Прямые (non-proxy) запросы на листенер: `/ssl`, `/ca.pem`, профиль, баннер.
    pub(crate) async fn direct_hit(self, req: Request<Incoming>) -> Response<MonitoredBody> {
        match req.uri().path() {
            "/ssl" => self.ssl_page(&req).await,
            "/" if is_setup_host(&req) => self.ssl_page(&req).await,
            "/ca.pem" => self.ca_pem().await,
            "/crabtrap.mobileconfig" => self.device_profile(&req).await,
            _ => text_response(200, "crabtrap proxy is running"),
        }
    }

    /// Страница инструкции установки CA (тёмная, inline-CSS, без внешних
    /// ассетов — работает оффлайн и до настройки прокси).
    pub(crate) async fn ssl_page(self, req: &Request<Incoming>) -> Response<MonitoredBody> {
        let status = match self.state.certs.ca_status().await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "CA unavailable for /ssl");
                return text_response(500, "crabtrap: certificate authority unavailable");
            }
        };
        let fingerprint = status
            .fingerprint
            .unwrap_or_else(|| "unavailable".to_string());
        let created = status
            .created_at
            .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let page = SslPage {
            fingerprint: &fingerprint,
            created: &created,
            proxy: self.device_proxy(req),
            ssid: wifi::current_ssid().await,
        };
        static_response("text/html; charset=utf-8", ssl_page_html(&page))
    }

    /// PEM корневого CA (MIME как у классических CA-раздатчиков).
    pub(crate) async fn ca_pem(self) -> Response<MonitoredBody> {
        match self.read_ca("/ca.pem").await {
            Ok((_, pem)) => static_response("application/x-x509-ca-cert", pem),
            Err(response) => response,
        }
    }

    /// Профиль iOS: CA + ручной прокси на адрес, по которому устройство
    /// открыло страницу, для сети `?ssid=` (пусто — только CA).
    pub(crate) async fn device_profile(self, req: &Request<Incoming>) -> Response<MonitoredBody> {
        let Some((host, port)) = self.device_proxy(req) else {
            return text_response(400, "crabtrap: open this page by the Mac's address");
        };
        let (status, pem) = match self.read_ca("/crabtrap.mobileconfig").await {
            Ok(ca) => ca,
            Err(response) => return response,
        };
        let Some(der) = CertificateDer::pem_slice_iter(pem.as_bytes())
            .next()
            .and_then(Result::ok)
        else {
            tracing::error!(path = %status.cert_path.display(), "ca.pem holds no certificate");
            return text_response(500, "crabtrap: cannot read the CA certificate");
        };
        let ssid = ssid_param(req.uri().query());
        let profile = DeviceProfile {
            ca_der: der.as_ref(),
            ca_fingerprint: status.fingerprint.as_deref().unwrap_or_default(),
            proxy_host: &host,
            proxy_port: port,
            ssid: ssid.as_deref(),
        };
        static_response(MOBILECONFIG_MIME, mobileconfig(&profile))
    }

    /// Адрес прокси для устройства: через адрес-ловушку — листенер
    /// соединения, при прямом заходе — Host страницы.
    fn device_proxy<B>(&self, req: &Request<B>) -> Option<(String, u16)> {
        if is_setup_host(req) {
            return self
                .listener_addr
                .map(|addr| (addr.ip().to_string(), addr.port()));
        }
        proxy_endpoint(req)
    }

    /// Статус CA и его PEM; ошибка — готовый 500-ответ (холодный путь,
    /// размер Err не важен).
    #[allow(clippy::result_large_err)]
    async fn read_ca(&self, route: &str) -> Result<(CaStatusDto, String), Response<MonitoredBody>> {
        let status = self.state.certs.ca_status().await.map_err(|e| {
            tracing::error!(route, error = %e, "CA unavailable");
            text_response(500, "crabtrap: certificate authority unavailable")
        })?;
        let pem = tokio::fs::read_to_string(&status.cert_path).await.map_err(|e| {
            tracing::error!(route, path = %status.cert_path.display(), error = %e, "cannot read ca.pem");
            text_response(500, "crabtrap: cannot read ca.pem")
        })?;
        Ok((status, pem))
    }
}

/// 200 с готовым телом.
fn static_response(content_type: &str, body: impl Into<bytes::Bytes>) -> Response<MonitoredBody> {
    let body = body.into();
    Response::builder()
        .status(200)
        .header(http::header::CONTENT_TYPE, content_type)
        .header(http::header::CONTENT_LENGTH, body.len().to_string())
        .body(MonitoredBody::from_body(
            http_body_util::Full::new(body),
            StreamTracker::new(|_, _, _| {}),
        ))
        .expect("static response builds")
}

/// Адрес прокси, как его видит устройство: Host запроса к странице.
fn proxy_endpoint<B>(req: &Request<B>) -> Option<(String, u16)> {
    let host = req.headers().get(http::header::HOST)?.to_str().ok()?;
    let authority: http::uri::Authority = host.parse().ok()?;
    let port = authority.port_u16().unwrap_or(DEFAULT_HTTP_PORT);
    Some((authority.host().to_string(), port))
}

/// `?ssid=` (percent-decoded, обрезанный; пусто — нет сети).
fn ssid_param(query: Option<&str>) -> Option<String> {
    url::form_urlencoded::parse(query?.as_bytes())
        .find(|(key, _)| key == "ssid")
        .map(|(_, value)| value.trim().to_string())
        .filter(|ssid| !ssid.is_empty())
}

/// Данные страницы `/ssl`.
struct SslPage<'a> {
    fingerprint: &'a str,
    created: &'a str,
    /// Адрес прокси для устройства (Host страницы).
    proxy: Option<(String, u16)>,
    /// Сеть этого Mac (подставляется в форму профиля).
    ssid: Option<String>,
}

/// HTML `/ssl`: профиль iOS, статус CA + скачивание, инструкции по платформам.
fn ssl_page_html(page: &SslPage) -> String {
    let ssid = xml_escape(page.ssid.as_deref().unwrap_or_default());
    let proxy = page
        .proxy
        .as_ref()
        .map(|(host, port)| format!("{}:{port}", xml_escape(host)))
        .unwrap_or_else(|| "this Mac".to_string());
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>crabtrap — SSL certificate</title>
<style>
 :root {{ color-scheme: dark; }}
 body {{ font: 15px/1.55 -apple-system, "SF Pro Text", system-ui, sans-serif;
        background: #16181d; color: #d7dae0; margin: 0; padding: 32px 16px; }}
 .card {{ max-width: 640px; margin: 0 auto; background: #1e2128;
         border: 1px solid #2c313a; border-radius: 12px; padding: 24px 28px; }}
 h1 {{ font-size: 20px; margin: 0 0 4px; }} .muted {{ color: #8b93a1; }}
 code, .fp {{ font-family: ui-monospace, "SF Mono", Menlo, monospace;
        font-size: 12px; color: #9ecbff; word-break: break-all; }}
 .dl {{ display: inline-block; margin: 14px 0 4px; padding: 9px 16px;
       background: #2563eb; color: #fff; border-radius: 8px; border: 0;
       font: inherit; text-decoration: none; font-weight: 600; }}
 input {{ font: inherit; width: 100%; box-sizing: border-box; margin-top: 4px;
         padding: 8px 10px; border-radius: 8px; border: 1px solid #2c313a;
         background: #16181d; color: #d7dae0; }}
 hr {{ border: 0; border-top: 1px solid #2c313a; margin: 20px 0; }}
 li {{ margin: 6px 0; }}
 .warn {{ color: #e3b341; }}
</style></head><body><div class="card">
<h1>crabtrap device setup</h1>
<p class="muted">Install the certificate authority to let crabtrap decrypt
HTTPS traffic of this proxy.</p>
<hr>
<h3>iPhone / iPad</h3>
<a class="dl" href="/crabtrap.mobileconfig">Install certificate</a>
<ol>
 <li>Open this page in Safari, tap <b>Install certificate</b> and allow the download.</li>
 <li>Settings &rarr; <b>Profile Downloaded</b> &rarr; Install.</li>
 <li><span class="warn">Required:</span> Settings &rarr; General &rarr; About &rarr;
     Certificate Trust Settings &rarr; enable <b>crabtrap CA</b>. If it is listed more
     than once, older ones are stale: remove their profiles in Settings &rarr; General
     &rarr; VPN &amp; Device Management.</li>
 <li>Settings &rarr; Wi-Fi &rarr; &#9432; next to the network &rarr; Configure Proxy
     &rarr; Manual: server and port <b>{proxy}</b>.</li>
</ol>
<details><summary class="muted">Or set the Wi-Fi proxy by the profile too</summary>
<form action="/crabtrap.mobileconfig" method="get">
 <label>Wi-Fi network to route through {proxy}
  <input name="ssid" value="{ssid}" placeholder="Network name"
         autocapitalize="off" autocorrect="off" spellcheck="false"></label>
 <button class="dl" type="submit">Install certificate and Wi-Fi proxy</button>
</form>
<p class="muted">Replaces the step with Configure Proxy. Removing the profile undoes both,
and iOS may forget the network with it.</p>
</details>
<hr>
<p>Fingerprint (SHA-256):<br><span class="fp">{fingerprint}</span></p>
<p class="muted">Created: {created}</p>
<a class="dl" href="/ca.pem">Download ca.pem</a>
<h3>macOS</h3>
<ol>
 <li>Click <a href="/ca.pem" style="color:#9ecbff">Download ca.pem</a>.</li>
 <li>Open the crabtrap app &rarr; Certificates &rarr; <b>Install Trust</b>
     (an administrator password prompt appears once).</li>
</ol>
<h3>Android</h3>
<ol>
 <li>Set the Wi-Fi proxy to {proxy} manually (Wi-Fi network &rarr; Proxy &rarr; Manual).</li>
 <li>Download ca.pem, then Settings &rarr; Security &rarr; Encryption &amp;
     credentials &rarr; Install a certificate &rarr; CA certificate.</li>
 <li><span class="warn">Android 7+:</span> user-installed CAs are trusted by
     browsers and apps that opt in via networkSecurityConfig only.</li>
</ol>
</div></body></html>"#,
        fingerprint = page.fingerprint,
        created = page.created,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_host(host: &str) -> Request<()> {
        Request::builder()
            .uri("/ssl")
            .header("host", host)
            .body(())
            .expect("request")
    }

    #[test]
    fn proxy_endpoint_comes_from_host() {
        assert_eq!(
            proxy_endpoint(&request_with_host("192.168.1.118:8080")),
            Some(("192.168.1.118".to_string(), 8080))
        );
        assert_eq!(
            proxy_endpoint(&request_with_host("mac.local")),
            Some(("mac.local".to_string(), DEFAULT_HTTP_PORT))
        );
        assert_eq!(
            proxy_endpoint(&Request::builder().body(()).expect("request")),
            None
        );
    }

    #[test]
    fn ssid_param_is_decoded_and_trimmed() {
        assert_eq!(ssid_param(Some("ssid=Home+5G")).as_deref(), Some("Home 5G"));
        assert_eq!(
            ssid_param(Some("ssid=%D0%94%D0%BE%D0%BC")).as_deref(),
            Some("Дом")
        );
        assert_eq!(ssid_param(Some("ssid=+")), None);
        assert_eq!(ssid_param(None), None);
    }

    #[test]
    fn page_prefills_network_and_escapes_it() {
        let page = SslPage {
            fingerprint: "AB",
            created: "now",
            proxy: Some(("192.168.1.118".into(), 8080)),
            ssid: Some("Tom & \"Jerry\"".into()),
        };
        let html = ssl_page_html(&page);
        assert!(
            html.contains(r#"value="Tom &amp; &quot;Jerry&quot;""#),
            "{html}"
        );
        assert!(html.contains("192.168.1.118:8080"));
        assert!(
            html.contains(r#"href="/crabtrap.mobileconfig""#),
            "certificate-only first"
        );
        assert!(html.contains(r#"action="/crabtrap.mobileconfig""#));
    }
}
