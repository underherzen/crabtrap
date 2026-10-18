//! Встроенные аддоны скриптов (как Built-in Addons в Proxyman):
//! `btoa` / `atob` и `$crypto` (`sha256`, `sha512`, `randomUUID`).
//! Нативные функции boa: без доступа к файлам и сети.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsNativeError, JsResult, JsString, JsValue, NativeFunction, js_string};

/// Зарегистрировать аддоны в глобальном объекте контекста.
pub fn install(context: &mut Context) -> JsResult<()> {
    context.register_global_builtin_callable(
        js_string!("btoa"),
        1,
        NativeFunction::from_fn_ptr(btoa),
    )?;
    context.register_global_builtin_callable(
        js_string!("atob"),
        1,
        NativeFunction::from_fn_ptr(atob),
    )?;
    let crypto = ObjectInitializer::new(context)
        .function(NativeFunction::from_fn_ptr(sha256), js_string!("sha256"), 1)
        .function(NativeFunction::from_fn_ptr(sha512), js_string!("sha512"), 1)
        .function(
            NativeFunction::from_fn_ptr(random_uuid),
            js_string!("randomUUID"),
            0,
        )
        .build();
    context.register_global_property(js_string!("$crypto"), crypto, Attribute::all())
}

fn string_arg(args: &[JsValue], context: &mut Context) -> JsResult<String> {
    let value = args.first().cloned().unwrap_or_default();
    Ok(value.to_string(context)?.to_std_string_escaped())
}

/// `btoa`: строка из символов 0–255 → base64 (как в браузере).
fn btoa(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = string_arg(args, context)?;
    let bytes = text
        .chars()
        .map(|c| u8::try_from(u32::from(c)))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| {
            JsNativeError::typ().with_message("btoa: string contains characters outside Latin-1")
        })?;
    Ok(JsValue::from(JsString::from(BASE64.encode(bytes))))
}

/// `atob`: base64 → «двоичная» строка (символ на байт).
fn atob(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = string_arg(args, context)?;
    let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let bytes = BASE64
        .decode(compact)
        .map_err(|_| JsNativeError::typ().with_message("atob: invalid base64"))?;
    let binary: String = bytes.into_iter().map(char::from).collect();
    Ok(JsValue::from(JsString::from(binary)))
}

/// Hex-хэш UTF-8 байтов строки.
fn digest<D: sha2::Digest>(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = string_arg(args, context)?;
    Ok(JsValue::from(JsString::from(hex(&D::digest(
        text.as_bytes(),
    )))))
}

fn sha256(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    digest::<sha2::Sha256>(args, context)
}

fn sha512(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    digest::<sha2::Sha512>(args, context)
}

fn random_uuid(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from(
        uuid::Uuid::new_v4().to_string(),
    )))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use boa_engine::Source;

    fn eval(code: &str) -> String {
        let mut context = Context::default();
        install(&mut context).expect("addons");
        context
            .eval(Source::from_bytes(code))
            .expect("eval")
            .to_string(&mut context)
            .expect("string")
            .to_std_string_escaped()
    }

    #[test]
    fn base64_roundtrip_like_browsers() {
        assert_eq!(eval("btoa('hello')"), "aGVsbG8=");
        assert_eq!(eval("atob('aGVsbG8=')"), "hello");
        assert_eq!(
            eval("try { btoa('ж'); 'ok' } catch (e) { e.name }"),
            "TypeError"
        );
    }

    #[test]
    fn crypto_hashes_and_uuid() {
        assert_eq!(
            eval("$crypto.sha256('abc')"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(eval("$crypto.sha512('').length"), "128");
        assert_eq!(eval("$crypto.randomUUID().length"), "36");
    }
}
